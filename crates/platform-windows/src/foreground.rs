//! Unprivileged foreground notifications and retained, process-instance-safe selection.
//! The hook and its message pump stay alive while asynchronous injection is pending.
use crate::{
    WindowsOptions,
    gpu_mailbox::GpuMailbox,
    target_selection::{Identity, Selection, Ticket},
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::oneshot;
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, WAIT_FAILED, WAIT_TIMEOUT},
        System::Threading::{
            CreateEventW, GetProcessTimes, INFINITE, OpenProcess, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
            SetEvent, WaitForSingleObject,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow,
                GetWindowThreadProcessId, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
                QS_ALLINPUT, TranslateMessage, WINEVENT_OUTOFCONTEXT,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

struct OwnedHandle(HANDLE);
// Kernel process/event handles can be queried and waited on from any thread.
// Arc ownership keeps them open until every in-flight wait/attachment is done.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Process {
    handle: OwnedHandle,
    identity: Identity,
}
impl Process {
    fn alive(&self) -> bool {
        (unsafe { WaitForSingleObject(self.handle.0, 0) }) == WAIT_TIMEOUT
    }
    fn supported(pid: u32, options: &WindowsOptions) -> Option<Self> {
        let handle = OwnedHandle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
            }
            .ok()?,
        );
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let process = Self {
            handle,
            identity: Identity {
                pid,
                created: (u64::from(created.dwHighDateTime) << 32)
                    | u64::from(created.dwLowDateTime),
            },
        };
        if !process.alive() {
            return None;
        }
        if !options.pids.contains(&pid) {
            let mut path = [0u16; 32768];
            let mut length = path.len() as u32;
            unsafe {
                QueryFullProcessImageNameW(
                    process.handle.0,
                    PROCESS_NAME_WIN32,
                    PWSTR(path.as_mut_ptr()),
                    &mut length,
                )
            }
            .ok()?;
            let path = &path[..length as usize];
            let start = path
                .iter()
                .rposition(|&unit| unit == u16::from(b'/') || unit == u16::from(b'\\'))
                .map_or(0, |position| position + 1);
            let name = &path[start..];
            if !process_name_matches(name, "osu!.exe")
                && !process_name_matches(name, "osulazer.exe")
                && !options
                    .process_names
                    .iter()
                    .any(|wanted| process_name_matches(name, wanted))
            {
                return None;
            }
        }
        Some(process)
    }
}

fn process_name_matches(name: &[u16], wanted: &str) -> bool {
    char::decode_utf16(name.iter().copied())
        .map(|unit| {
            unit.unwrap_or(char::REPLACEMENT_CHARACTER)
                .to_ascii_lowercase()
        })
        .eq(wanted
            .chars()
            .map(|character| character.to_ascii_lowercase()))
}

#[derive(Clone)]
pub(crate) struct Target {
    process: Arc<Process>,
    ticket: Ticket,
}
impl Target {
    pub(crate) fn pid(&self) -> u32 {
        self.ticket.identity.pid
    }
    pub(crate) fn alive(&self) -> bool {
        self.process.alive()
    }
    pub(crate) fn same_selection(&self, other: &Self) -> bool {
        self.ticket == other.ticket
    }
}

#[derive(Default)]
struct TrackedSelection {
    policy: Selection,
    process: Option<Arc<Process>>,
}
struct State {
    options: WindowsOptions,
    selection: Mutex<TrackedSelection>,
    error: Mutex<Option<String>>,
    gpu: Arc<GpuMailbox>,
    wake: OwnedHandle,
    stopping: AtomicBool,
}
impl State {
    fn observe(&self, hwnd: HWND) {
        let pid = window_pid(hwnd);
        if let Some(pid) = pid {
            let selection = self.selection.lock().unwrap_or_else(|e| e.into_inner());
            if selection
                .process
                .as_ref()
                .is_some_and(|process| process.identity.pid == pid && process.alive())
            {
                // A live held handle rules out PID reuse; do not re-open/query
                // or allocate for another foreground window in the same client.
                return;
            }
        }
        let foreground = pid.and_then(|pid| Process::supported(pid, &self.options));
        let mut selection = self.selection.lock().unwrap_or_else(|e| e.into_inner());
        let alive = selection
            .process
            .as_ref()
            .is_some_and(|process| process.alive());
        selection
            .policy
            .observe(alive, foreground.as_ref().map(|process| process.identity));
        if let Some(process) = foreground {
            if selection
                .process
                .as_ref()
                .is_none_or(|old| old.identity != process.identity)
            {
                selection.process = Some(Arc::new(process));
            }
        } else if !alive {
            selection.process = None;
        }
    }
    fn target(&self) -> Result<Option<Target>> {
        if let Some(error) = &*self.error.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(anyhow!("foreground notification source failed: {error}"));
        }
        let selection = self.selection.lock().unwrap_or_else(|e| e.into_inner());
        Ok(selection
            .policy
            .ticket
            .zip(selection.process.as_ref())
            .map(|(ticket, process)| Target {
                process: process.clone(),
                ticket,
            }))
    }
}

thread_local! {
    static HOOK_STATE: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
}
unsafe extern "system" fn foreground_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    HOOK_STATE.with(|slot| {
        if let Some(state) = slot.borrow().as_ref() {
            // Use the event HWND, not a later GetForegroundWindow result: a
            // supported client briefly focused before an unrelated app counts.
            state.observe(hwnd);
            state.gpu.worker_wakeup.notify_one();
        }
    });
}

pub(crate) struct ForegroundEvents {
    state: Arc<State>,
    barriers: mpsc::SyncSender<oneshot::Sender<Result<Option<Target>>>>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl ForegroundEvents {
    pub(crate) fn start(options: WindowsOptions, gpu: Arc<GpuMailbox>) -> Result<Self> {
        let state = Arc::new(State {
            options,
            selection: Mutex::new(TrackedSelection::default()),
            error: Mutex::new(None),
            gpu,
            wake: OwnedHandle(unsafe { CreateEventW(None, false, false, PCWSTR::null()) }?),
            stopping: AtomicBool::new(false),
        });
        let (barriers, requests) = mpsc::sync_channel(1);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let thread_state = state.clone();
        let thread = thread::Builder::new()
            .name("vosulay-foreground".into())
            .spawn(move || {
                if let Err(error) = message_pump(&thread_state, requests, ready_tx) {
                    *thread_state.error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("{error:#}"));
                    thread_state.gpu.worker_wakeup.notify_one();
                }
                let _ = done_tx.send(());
            })
            .context("starting foreground notification thread")?;
        let source = Self {
            state,
            barriers,
            done,
            thread: Some(thread),
        };
        ready
            .recv_timeout(Duration::from_secs(5))
            .context("foreground hook initialization timed out")??;
        Ok(source)
    }
    pub(crate) fn target(&self) -> Result<Option<Target>> {
        self.state.target()
    }

    /// Drain native notifications on their owning thread, then sample the live
    /// foreground before approving an asynchronous attachment completion.
    pub(crate) async fn revalidate(&self) -> Result<Option<Target>> {
        let (tx, rx) = oneshot::channel();
        self.barriers
            .try_send(tx)
            .context("foreground notification thread is unavailable")?;
        unsafe { SetEvent(self.state.wake.0) }?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .context("foreground selection revalidation timed out")?
            .context("foreground notification thread stopped")?
    }
}
impl Drop for ForegroundEvents {
    fn drop(&mut self) {
        self.state.stopping.store(true, Ordering::Release);
        let _ = unsafe { SetEvent(self.state.wake.0) };
        // Never let teardown suspend the fail-open input/shutdown deadline.
        if self.done.recv_timeout(Duration::from_secs(1)).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

struct Hook(HWINEVENTHOOK);
impl Drop for Hook {
    fn drop(&mut self) {
        let _ = unsafe { UnhookWinEvent(self.0) };
        HOOK_STATE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
fn message_pump(
    state: &Arc<State>,
    barriers: mpsc::Receiver<oneshot::Sender<Result<Option<Target>>>>,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    HOOK_STATE.with(|slot| *slot.borrow_mut() = Some(state.clone()));
    let hook = Hook(unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    });
    if hook.0.is_invalid() {
        let error = windows::core::Error::from_thread();
        let _ = ready.send(Err(anyhow!(
            "installing foreground WinEventHook failed: {error}"
        )));
        return Err(error.into());
    }
    state.observe(unsafe { GetForegroundWindow() });
    let _ = ready.send(Ok(()));
    while !state.stopping.load(Ordering::Acquire) {
        // PeekMessage delivers out-of-context WinEvent callbacks on this thread.
        // Drain before the barrier, including rapid supported/unrelated switches.
        let mut message = MSG::default();
        while !state.stopping.load(Ordering::Acquire)
            && unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool()
        {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if state.stopping.load(Ordering::Acquire) {
            break;
        }
        while let Ok(reply) = barriers.try_recv() {
            state.observe(unsafe { GetForegroundWindow() });
            let _ = reply.send(state.target());
        }
        // Wait on the retained instance's handle, not a freshly opened PID. An
        // exit wakes a detached worker even with no foreground transition.
        let target = state.target()?;
        if let Some(target) = &target
            && !target.alive()
        {
            state.observe(HWND::default());
            state.gpu.worker_wakeup.notify_one();
            continue;
        }
        let mut handles = [state.wake.0, HANDLE::default()];
        let count = if let Some(target) = &target {
            handles[1] = target.process.handle.0;
            2
        } else {
            1
        };
        let result = unsafe {
            MsgWaitForMultipleObjects(Some(&handles[..count]), false, INFINITE, QS_ALLINPUT)
        };
        ensure!(
            result != WAIT_FAILED,
            "waiting for foreground notifications: {}",
            windows::core::Error::from_thread()
        );
        if count == 2 && result.0 == 1 {
            state.observe(HWND::default());
            state.gpu.worker_wakeup.notify_one();
        }
    }
    Ok(())
}

pub(crate) fn foreground_pid() -> Option<u32> {
    window_pid(unsafe { GetForegroundWindow() })
}
fn window_pid(window: HWND) -> Option<u32> {
    if window.is_invalid() {
        return None;
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }
    (pid != 0).then_some(pid)
}
