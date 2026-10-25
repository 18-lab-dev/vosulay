use crate::{
    WindowsOptions,
    d3d11::{FrameBackend, TextureLease},
    foreground::{ForegroundEvents, Target, foreground_pid},
    gpu_mailbox::{Binding, GpuMailbox},
    tray::Tray,
};
use anyhow::{Context, Result, anyhow, ensure};
use asdf_overlay_client::{
    OverlayDll,
    client::{IpcClientConn, IpcClientEventStream},
};
use asdf_overlay_common::{
    event::{
        OverlayEvent,
        surface::{SurfaceEvent, SurfaceInfo},
    },
    request::{
        BlockInput,
        surface::{SetPosition, UpdateSharedHandle},
    },
};
use asdf_overlay_window_event::{
    WindowEvent,
    input::{
        CursorAction, CursorEvent, CursorInputState, Ime, InputEvent, KeyInputState, KeyboardInput,
        ScrollAxis,
    },
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::{runtime::Builder, task::JoinHandle as AsyncTask};
use vosulay_frame::{
    BrowserFrameBackend, BrowserInput, InputModifiers, KeyKind, MouseButton, PlatformEvent,
    Viewport,
};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::Gdi::{
            DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
            MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
        },
        UI::{Input::KeyboardAndMouse::GetAsyncKeyState, WindowsAndMessaging::GetForegroundWindow},
    },
    core::PCWSTR,
};

const DEADLINE: Duration = Duration::from_secs(5);
const MAX_INPUTS: usize = 512;

struct Shared {
    epoch: Instant,
    heartbeat: AtomicU64,
    rpc_started: AtomicU64,
    stop: AtomicBool,
    safety_stop: AtomicBool,
    capture: AtomicBool,
    capture_generation: AtomicU64,
    gpu: Arc<GpuMailbox>,
    events: Mutex<VecDeque<PlatformEvent>>,
    inputs: Mutex<VecDeque<BrowserInput>>,
    viewport: Mutex<Option<Viewport>>,
    fatal: Mutex<Option<String>>,
    host: thread::Thread,
}
impl Shared {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }
    fn fail(&self, error: impl Into<String>) {
        let mut fatal = self.fatal.lock().unwrap_or_else(|e| e.into_inner());
        if fatal.is_none() {
            *fatal = Some(error.into());
        }
        self.stop.store(true, Ordering::Release);
        self.host.unpark();
        self.gpu.worker_wakeup.notify_one();
    }
    fn emit(&self, event: PlatformEvent) {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.len() == 32 {
            self.fail("native lifecycle event queue overflowed");
            return;
        }
        events.push_back(event);
        self.host.unpark();
    }
    fn input(&self, generation: u64, input: BrowserInput) {
        let mut inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        if !self.capture.load(Ordering::Acquire)
            || generation != self.capture_generation.load(Ordering::Acquire)
        {
            return;
        }
        // Losing a key/button release can leave the editor dragging indefinitely.
        if inputs.len() == MAX_INPUTS {
            self.fail("editor input queue overflowed; releasing capture");
            return;
        }
        if matches!(input, BrowserInput::Move { leave: false, .. })
            && matches!(inputs.back(), Some(BrowserInput::Move { leave: false, .. }))
        {
            *inputs.back_mut().unwrap() = input;
        } else {
            inputs.push_back(input);
        }
        self.host.unpark();
    }
    fn input_capture(&self, enabled: bool) -> bool {
        let mut inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        let previous = self.capture.swap(enabled, Ordering::AcqRel);
        self.capture_generation.fetch_add(1, Ordering::AcqRel);
        inputs.clear();
        previous
    }
}

enum Command {
    Capture(bool, SyncSender<Result<()>>),
    Shutdown,
}

pub struct WindowsController {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
    safety: Option<JoinHandle<()>>,
    tray: Tray,
}
impl WindowsController {
    pub fn start(
        options: WindowsOptions,
        toggle_edit: vosulay_frame::hotkey::NativeHotkey,
        toggle_visibility: Option<vosulay_frame::hotkey::NativeHotkey>,
    ) -> Result<Self> {
        for file in ["asdf_overlay-x64.dll", "asdf_overlay-x86.dll"] {
            ensure!(
                options.dll_dir.join(file).is_file(),
                "missing packaged injection asset {file}"
            );
        }
        let tray = Tray::start(toggle_edit, toggle_visibility)?;
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            heartbeat: AtomicU64::new(1),
            rpc_started: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            safety_stop: AtomicBool::new(false),
            capture: AtomicBool::new(false),
            capture_generation: AtomicU64::new(0),
            gpu: Arc::new(GpuMailbox::default()),
            events: Mutex::new(VecDeque::with_capacity(32)),
            inputs: Mutex::new(VecDeque::with_capacity(MAX_INPUTS)),
            viewport: Mutex::new(None),
            fatal: Mutex::new(None),
            host: thread::current(),
        });
        let (commands, rx) = mpsc::sync_channel(8);
        let safety_shared = shared.clone();
        let safety = thread::Builder::new().name("vosulay-input-safety".into()).spawn(move || {
            while !safety_shared.safety_stop.load(Ordering::Acquire) {
                let now = safety_shared.now();
                let rpc = safety_shared.rpc_started.load(Ordering::Acquire);
                if rpc != 0 && now.saturating_sub(rpc) >= DEADLINE.as_millis() as u64 {
                    terminate("native overlay request exceeded five seconds; exiting to release game input");
                }
                if safety_shared.capture.load(Ordering::Acquire) && now.saturating_sub(safety_shared.heartbeat.load(Ordering::Acquire)) >= DEADLINE.as_millis() as u64 {
                    terminate("manager renderer heartbeat expired; exiting to release game input");
                }
                thread::park_timeout(Duration::from_millis(20));
            }
        })?;
        let worker_shared = shared.clone();
        let worker = match thread::Builder::new()
            .name("vosulay-native-overlay".into())
            .spawn(move || {
                match Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        if let Err(error) =
                            runtime.block_on(worker_main(options, worker_shared.clone(), rx))
                        {
                            worker_shared.fail(format!("{error:#}"));
                        }
                        runtime.shutdown_timeout(Duration::from_secs(1));
                    }
                    Err(error) => {
                        worker_shared.fail(format!("starting overlay IPC runtime: {error}"))
                    }
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                shared.safety_stop.store(true, Ordering::Release);
                let _ = safety.join();
                return Err(error.into());
            }
        };
        Ok(Self {
            shared,
            commands,
            worker: Some(worker),
            safety: Some(safety),
            tray,
        })
    }
    pub fn frame_backend(&self) -> Box<dyn BrowserFrameBackend> {
        Box::new(FrameBackend::new(self.shared.gpu.clone()))
    }
    pub fn poll(&mut self) -> Option<PlatformEvent> {
        if let Some(error) = self
            .shared
            .fatal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Some(PlatformEvent::Fatal(error));
        }
        if let Some(event) = self
            .shared
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Some(event);
        }
        if let Some(viewport) = self
            .shared
            .viewport
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Some(PlatformEvent::Viewport(viewport));
        }
        if let Some(event) = self.tray.poll() {
            if matches!(event, PlatformEvent::ShortcutToggleEdit) && !self.game_focused() {
                return None;
            }
            if matches!(event, PlatformEvent::ToggleEdit) {
                let binding = *self
                    .shared
                    .gpu
                    .binding
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let window = binding.and_then(|binding| binding.info.api.window_id());
                if let Some(window) = window {
                    // Explicit tray edit activation returns focus to the attached
                    // game; the normal capture gate still verifies the foreground PID.
                    let hwnd = HWND(window as usize as *mut _);
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                            hwnd,
                            windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                        );
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                    }
                }
            }
            return Some(event);
        }
        self.shared
            .inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .map(PlatformEvent::Input)
    }
    pub fn game_focused(&self) -> bool {
        let binding = *self
            .shared
            .gpu
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        binding.is_some_and(|binding| foreground_pid() == Some(binding.pid))
    }
    pub fn set_capture(&self, enabled: bool) -> Result<()> {
        self.capture_with_timeout(enabled, DEADLINE)
    }
    /// End normal editing only after held input is up. Games can poll physical
    /// key state as well as messages, so releasing on Escape-down leaks Escape.
    /// Fault cleanup and native dialogs must continue using set_capture(false).
    pub fn end_edit_capture(&self) -> Result<()> {
        let until = Instant::now() + DEADLINE;
        while self.shared.capture.load(Ordering::Acquire)
            && !self.shared.stop.load(Ordering::Acquire)
            && self.game_focused()
            && (1..=255).any(|key| unsafe { GetAsyncKeyState(key) as u16 & 0x8000 != 0 })
        {
            if Instant::now() >= until {
                terminate("edit input did not release within five seconds");
            }
            thread::park_timeout(Duration::from_millis(1));
        }
        self.capture_with_timeout(false, until.saturating_duration_since(Instant::now()))
    }
    fn capture_with_timeout(&self, enabled: bool, timeout: Duration) -> Result<()> {
        if enabled {
            ensure!(
                self.game_focused(),
                "focus the attached game before editing"
            );
            ensure!(
                self.shared
                    .now()
                    .saturating_sub(self.shared.heartbeat.load(Ordering::Acquire))
                    < 2500,
                "manager renderer is not responsive"
            );
        } else {
            self.shared.input_capture(false);
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::Capture(enabled, tx))
            .context("native overlay worker is unavailable")?;
        self.shared.gpu.worker_wakeup.notify_one();
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => terminate("input capture/release did not finish within five seconds"),
        }
    }
    /// Call only after a nonce-matched heartbeat from the trusted manager renderer.
    pub fn heartbeat(&self) {
        self.shared
            .heartbeat
            .store(self.shared.now(), Ordering::Release);
    }
    pub fn set_tray_state(&self, editing: bool, visible: bool) {
        self.tray.set_state(editing, visible);
    }
    pub fn set_hotkeys(
        &self,
        toggle_edit: vosulay_frame::hotkey::NativeHotkey,
        toggle_visibility: Option<vosulay_frame::hotkey::NativeHotkey>,
    ) {
        self.tray.set_hotkeys(toggle_edit, toggle_visibility);
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.shared.input_capture(false);
        self.shared.stop.store(true, Ordering::Release);
        let _ = self.commands.try_send(Command::Shutdown);
        self.shared.gpu.worker_wakeup.notify_one();
        if let Some(worker) = self.worker.take() {
            let until = Instant::now() + DEADLINE;
            while !worker.is_finished() {
                if Instant::now() >= until {
                    terminate("native overlay shutdown exceeded five seconds");
                }
                thread::park_timeout(Duration::from_millis(1));
            }
            worker
                .join()
                .map_err(|_| anyhow!("native overlay worker panicked"))?;
        }
        self.shared.safety_stop.store(true, Ordering::Release);
        if let Some(safety) = self.safety.take() {
            safety
                .join()
                .map_err(|_| anyhow!("input safety thread panicked"))?;
        }
        self.tray.shutdown()
    }
}
impl Drop for WindowsController {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
fn terminate(message: &str) -> ! {
    eprintln!("Vosulay: {message}");
    std::process::exit(1)
}

struct RpcGuard<'a>(&'a Shared);
impl<'a> RpcGuard<'a> {
    fn new(shared: &'a Shared) -> Self {
        shared.rpc_started.store(shared.now(), Ordering::Release);
        Self(shared)
    }
}
impl Drop for RpcGuard<'_> {
    fn drop(&mut self) {
        self.0.rpc_started.store(0, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct Surface {
    info: SurfaceInfo,
    width: u32,
    height: u32,
}
#[derive(Default)]
struct TargetState {
    windows: HashMap<u32, (u32, u32)>,
    surfaces: HashMap<u64, Surface>,
    disconnected: bool,
    capture_ended: bool,
}
struct Connection {
    pid: u32,
    target: Target,
    ipc: IpcClientConn,
    state: Arc<Mutex<TargetState>>,
    reader: AsyncTask<()>,
    binding: Option<Binding>,
    capture: bool,
    lease: Option<Arc<TextureLease>>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn attach(dir: &Path, target: Target, shared: Arc<Shared>) -> Result<Connection> {
    let pid = target.pid();
    let x64 = dir.join("asdf_overlay-x64.dll");
    let x86 = dir.join("asdf_overlay-x86.dll");
    let (ipc, events) = tokio::time::timeout(
        DEADLINE,
        asdf_overlay_client::inject(
            pid,
            OverlayDll {
                x64: Some(&x64),
                x86: Some(&x86),
                arm64: None,
            },
            Some(DEADLINE),
        ),
    )
    .await??;
    let state = Arc::new(Mutex::new(TargetState::default()));
    let reader_state = state.clone();
    // Continuously drain the SDK's unbounded stream, including while an RPC awaits
    // its ACK. Geometry is latest-state and input is separately bounded.
    let reader = tokio::spawn(read_events(events, reader_state, shared, pid));
    Ok(Connection {
        pid,
        target,
        ipc,
        state,
        reader,
        binding: None,
        capture: false,
        lease: None,
    })
}

async fn read_events(
    mut events: IpcClientEventStream,
    state: Arc<Mutex<TargetState>>,
    shared: Arc<Shared>,
    pid: u32,
) {
    let native_trace = std::env::var("VOSULAY_NATIVE_TRACE").unwrap_or_default();
    while let Some(event) = events.recv().await {
        if let OverlayEvent::Tracing(event) = event {
            use asdf_overlay_common::event::tracing::{LogLevel, TracingEvent};
            if let TracingEvent::Event {
                metadata,
                message: Some(message),
            } = event
                && (native_trace == "trace"
                    || (native_trace == "1" && !matches!(metadata.level, LogLevel::Trace)))
            {
                let end = message
                    .char_indices()
                    .nth(2048)
                    .map_or(message.len(), |(index, _)| index);
                eprintln!(
                    "vosulay native pid={pid} {:?} {:?} {}:{:?}: {}",
                    metadata.time,
                    metadata.level,
                    metadata.module_path.as_deref().unwrap_or("?"),
                    metadata.line,
                    &message[..end]
                );
            }
            continue;
        }
        let generation = shared.capture_generation.load(Ordering::Acquire);
        if let OverlayEvent::Window {
            id,
            event: WindowEvent::Input(input),
        } = event
        {
            let binding = *shared.gpu.binding.lock().unwrap_or_else(|e| e.into_inner());
            if shared.capture.load(Ordering::Acquire)
                && binding.is_some_and(|b| {
                    // BlockInput captures all game windows. Stable's keyboard
                    // HWND can differ from its OpenGL drawable; only pointer
                    // coordinates must belong to that drawable.
                    b.pid == pid
                        && (matches!(&input, InputEvent::Keyboard(_))
                            || b.info.api.window_id() == Some(id))
                })
            {
                decode(input, &shared, generation);
            }
            continue;
        }
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            OverlayEvent::Window {
                id,
                event: WindowEvent::Added { width, height } | WindowEvent::Resized { width, height },
            } => {
                state.windows.insert(id, (width, height));
            }
            OverlayEvent::Window {
                id,
                event: WindowEvent::Destroyed,
            } => {
                state.windows.remove(&id);
                state
                    .surfaces
                    .retain(|_, surface| surface.info.api.window_id() != Some(id));
            }
            OverlayEvent::Surface {
                id,
                event:
                    SurfaceEvent::Added {
                        width,
                        height,
                        info,
                    },
            } => {
                state.surfaces.insert(
                    id,
                    Surface {
                        info,
                        width,
                        height,
                    },
                );
            }
            OverlayEvent::Surface {
                id,
                event: SurfaceEvent::Resized { width, height },
            } => {
                if let Some(surface) = state.surfaces.get_mut(&id) {
                    surface.width = width;
                    surface.height = height;
                }
            }
            OverlayEvent::Surface {
                id,
                event: SurfaceEvent::Destroyed,
            } => {
                state.surfaces.remove(&id);
            }
            OverlayEvent::InputBlockingEnded => {
                shared.input_capture(false);
                state.capture_ended = true;
            }
            _ => {}
        }
        shared.gpu.worker_wakeup.notify_one();
    }
    state.lock().unwrap_or_else(|e| e.into_inner()).disconnected = true;
    shared.gpu.worker_wakeup.notify_one();
}

async fn capture(connection: &mut Connection, enabled: bool, shared: &Shared) -> Result<()> {
    if connection.capture == enabled {
        return Ok(());
    }
    if enabled {
        ensure!(
            connection.binding.is_some(),
            "game render surface is not ready"
        );
    }
    let _deadline = RpcGuard::new(shared);
    // BlockInput itself listens process-wide; no redundant per-window listen RPC.
    connection
        .ipc
        .request(BlockInput { block: enabled })
        .await
        .context("changing native input capture")?;
    if !enabled {
        // The RPC ACK may precede the window-thread release. Wait for the native
        // notification before permitting another enable, under the same deadline.
        loop {
            {
                let mut state = connection.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.disconnected || std::mem::take(&mut state.capture_ended) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    connection.capture = enabled;
    shared.input_capture(enabled);
    Ok(())
}

async fn disconnect(current: &mut Option<Connection>, shared: &Shared) -> Result<()> {
    // Clear local capture even if a release RPC fails; dropping the pipe is the
    // native fail-open path and no stale browser input may survive the switch.
    shared.input_capture(false);
    *shared.gpu.binding.lock().unwrap_or_else(|e| e.into_inner()) = None;
    shared
        .gpu
        .update
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(mut connection) = current.take() {
        // A disconnected pipe releases native capture in asdf itself. Never retry
        // a cancelled/partly written IPC request against that connection.
        if connection.target.alive()
            && !connection
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .disconnected
        {
            let released: Result<()> = async {
                capture(&mut connection, false, shared).await?;
                if let Some(binding) = connection.binding {
                    let _deadline = RpcGuard::new(shared);
                    connection
                        .ipc
                        .surface(binding.surface)
                        .request(UpdateSharedHandle::None)
                        .await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = released
                && connection.target.alive()
                && !connection
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .disconnected
            {
                return Err(error);
            }
        }
        shared.emit(PlatformEvent::Detached);
    }
    Ok(())
}

async fn worker_main(
    options: WindowsOptions,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
) -> Result<()> {
    let mut current: Option<Connection> = None;
    let focus = ForegroundEvents::start(options.clone(), shared.gpu.clone())?;
    let mut generation = 0;
    let mut retry_attach = None;
    let mut last_viewport = None;
    let mut last_display_read = Instant::now() - Duration::from_secs(2);
    while !shared.stop.load(Ordering::Acquire) {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Shutdown => {
                    shared.stop.store(true, Ordering::Release);
                    break;
                }
                Command::Capture(enabled, reply) => {
                    let result = match current.as_mut() {
                        Some(connection) => capture(connection, enabled, &shared).await,
                        None if !enabled => Ok(()),
                        None => Err(anyhow!("no attached game")),
                    };
                    if result.is_err() && current.is_some() {
                        // Even a failed enable can have reached the server before
                        // its ACK failed. Drop the pipe rather than assuming no capture.
                        current.take();
                        *shared.gpu.binding.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        shared.input_capture(false);
                        shared.emit(PlatformEvent::Detached);
                    }
                    let _ = reply.send(result);
                }
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let target = focus.target()?;
        if retry_attach
            .as_ref()
            .is_some_and(|(failed, _): &(Target, Instant)| {
                target
                    .as_ref()
                    .is_none_or(|target| !failed.same_selection(target))
            })
        {
            retry_attach = None;
        }
        let dead_or_stale = current.as_ref().is_some_and(|connection| {
            target
                .as_ref()
                .is_none_or(|target| !connection.target.same_selection(target))
                || connection
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .disconnected
        });
        if dead_or_stale {
            disconnect(&mut current, &shared).await?;
            last_viewport = None;
        }
        if let Some(target) = target.filter(|_| current.is_none()) {
            let retry_ready =
                retry_attach
                    .as_ref()
                    .is_none_or(|(failed, until): &(Target, Instant)| {
                        !failed.same_selection(&target) || Instant::now() >= *until
                    });
            if retry_ready && target.alive() {
                let pid = target.pid();
                match attach(&options.dll_dir, target.clone(), shared.clone()).await {
                    Ok(connection) => {
                        let latest = focus.revalidate().await?;
                        if !shared.stop.load(Ordering::Acquire)
                            && target.alive()
                            && latest
                                .as_ref()
                                .is_some_and(|latest| target.same_selection(latest))
                        {
                            current = Some(connection);
                        } else {
                            // This connection has never enabled capture or published
                            // a binding. Dropping it closes IPC and aborts its reader.
                            drop(connection);
                        }
                        retry_attach = None;
                    }
                    Err(error) => {
                        shared.emit(PlatformEvent::Warning(format!(
                            "could not attach to game PID {pid}: {error:#}"
                        )));
                        retry_attach = Some((target, Instant::now() + Duration::from_millis(100)));
                    }
                }
                // Re-enter selection before exposing surfaces from a completion;
                // a foreground notification during injection must win.
                continue;
            }
        }
        if let Some(connection) = current.as_mut() {
            let (surface, ended) = {
                let mut state = connection.state.lock().unwrap_or_else(|e| e.into_inner());
                let foreground = unsafe { GetForegroundWindow() }.0 as usize as u32;
                let viable = |surface: &&Surface| {
                    surface.width > 0
                        && surface.height > 0
                        && surface
                            .info
                            .api
                            .window_id()
                            .is_some_and(|id| state.windows.contains_key(&id))
                };
                let selected = state
                    .surfaces
                    .iter()
                    .filter(|(_, s)| viable(s))
                    .min_by_key(|(id, s)| {
                        (
                            s.info.api.window_id() != Some(foreground),
                            connection.binding.is_none_or(|b| b.surface != **id),
                            **id,
                        )
                    })
                    .map(|(&id, &surface)| (id, surface));
                (selected, std::mem::take(&mut state.capture_ended))
            };
            if ended {
                let captured = std::mem::replace(&mut connection.capture, false);
                shared.input_capture(false);
                if captured {
                    shared.emit(PlatformEvent::CaptureEnded);
                }
            }
            let next = surface.map(|(id, s)| Binding {
                generation,
                pid: connection.pid,
                surface: id,
                info: s.info,
                width: s.width,
                height: s.height,
            });
            let changed = match (connection.binding, next) {
                (Some(old), Some(next)) => {
                    old.pid != next.pid
                        || old.surface != next.surface
                        || old.info != next.info
                        || old.width != next.width
                        || old.height != next.height
                }
                (None, None) => false,
                _ => true,
            };
            if changed {
                if connection.capture {
                    capture(connection, false, &shared).await?;
                    shared.emit(PlatformEvent::CaptureEnded);
                }
                if let Some(old) = connection.binding
                    && next.is_none_or(|next| next.surface != old.surface)
                {
                    let _deadline = RpcGuard::new(&shared);
                    // The old surface may already have been destroyed.
                    let _ = connection
                        .ipc
                        .surface(old.surface)
                        .request(UpdateSharedHandle::None)
                        .await;
                }
                generation += 1;
                connection.binding = next.map(|mut binding| {
                    binding.generation = generation;
                    binding
                });
                connection.lease = None;
                *shared.gpu.binding.lock().unwrap_or_else(|e| e.into_inner()) = None;
                shared
                    .gpu
                    .update
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(binding) = connection.binding {
                    {
                        let _deadline = RpcGuard::new(&shared);
                        connection
                            .ipc
                            .surface(binding.surface)
                            .request(SetPosition { x: 0, y: 0 })
                            .await?;
                    }
                    // Positioning also awaits IPC. Do not publish a surface if
                    // another supported instance won while that RPC was pending.
                    let latest = focus.revalidate().await?;
                    if shared.stop.load(Ordering::Acquire)
                        || !connection.target.alive()
                        || latest
                            .as_ref()
                            .is_none_or(|latest| !connection.target.same_selection(latest))
                    {
                        disconnect(&mut current, &shared).await?;
                        last_viewport = None;
                        continue;
                    }
                    *shared.gpu.binding.lock().unwrap_or_else(|e| e.into_inner()) =
                        connection.binding;
                    shared.emit(PlatformEvent::Attached {
                        pid: connection.pid,
                    });
                } else {
                    shared.emit(PlatformEvent::Detached);
                }
                last_viewport = None;
            }
            if let Some(binding) = connection
                .binding
                .filter(|_| changed || last_display_read.elapsed() >= Duration::from_secs(1))
            {
                last_display_read = Instant::now();
                let viewport = viewport(binding)?;
                if last_viewport != Some(viewport) {
                    *shared.viewport.lock().unwrap_or_else(|e| e.into_inner()) = Some(viewport);
                    last_viewport = Some(viewport);
                    shared.host.unpark();
                }
            }
            let update = shared
                .gpu
                .update
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(update) = update.filter(|update| connection.binding == Some(update.binding))
            {
                let handle = update
                    .handle
                    .map(UpdateSharedHandle::Kmt)
                    .unwrap_or(UpdateSharedHandle::None);
                let _deadline = RpcGuard::new(&shared);
                connection
                    .ipc
                    .surface(update.binding.surface)
                    .request(handle)
                    .await
                    .context("publishing owned D3D11 overlay texture")?;
                connection.lease = update.lease;
            }
        }
        // asdf 2.1 does not expose game-present notifications. CEF is paced by
        // the product's display/FPS clock; no fictitious Present events are emitted.
        let wakeup = shared.gpu.worker_wakeup.notified();
        if current
            .as_ref()
            .is_some_and(|connection| connection.binding.is_some())
        {
            // Attached display metadata still needs bounded maintenance, but
            // native foreground/process-exit events drive target selection.
            let next_display = last_display_read + Duration::from_secs(1);
            let _ = tokio::time::timeout(
                next_display.saturating_duration_since(Instant::now()),
                wakeup,
            )
            .await;
        } else if let Some((failed, until)) = retry_attach.as_ref()
            && focus
                .target()?
                .as_ref()
                .is_some_and(|target| target.same_selection(failed))
        {
            // Retry only a failed attachment to an already selected instance.
            // This is not a foreground/enumeration polling clock.
            let _ =
                tokio::time::timeout(until.saturating_duration_since(Instant::now()), wakeup).await;
        } else {
            // No target: no scan timer. Commands, shutdown, and the native
            // message-pump notifications all wake the same retained permit.
            wakeup.await;
        }
    }
    disconnect(&mut current, &shared).await
}

fn viewport(binding: Binding) -> Result<Viewport> {
    let id = binding
        .info
        .api
        .window_id()
        .context("game surface has no window")?;
    let monitor =
        unsafe { MonitorFromWindow(HWND(id as usize as *mut _), MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    ensure!(
        unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool(),
        "could not read attached game's display"
    );
    let mut mode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    ensure!(
        unsafe {
            EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        }
        .as_bool(),
        "could not read attached display refresh rate"
    );
    ensure!(
        mode.dmDisplayFrequency > 1,
        "attached display did not report a refresh rate"
    );
    Ok(Viewport {
        width: binding.width,
        height: binding.height,
        scale: 1.0,
        refresh_hz: f64::from(mode.dmDisplayFrequency),
    })
}

fn modifiers() -> InputModifiers {
    let down = |key| unsafe { GetAsyncKeyState(key) as u16 & 0x8000 != 0 };
    // The async high bit is physical state; the low bit is NOT a lock toggle.
    // GetKeyState's lock low bits do not depend on this worker receiving WM_KEYDOWN.
    let toggled = |key| unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(key) as u16 & 1 != 0
    };
    InputModifiers {
        shift: down(0x10),
        control: down(0x11),
        alt: down(0x12),
        meta: down(0x5b) || down(0x5c),
        caps_lock: toggled(0x14),
        num_lock: toggled(0x90),
        left_button: down(1),
        middle_button: down(4),
        right_button: down(2),
        ..Default::default()
    }
}
fn characters(
    text: impl Iterator<Item = char>,
    modifiers: InputModifiers,
    shared: &Shared,
    generation: u64,
) {
    for character in text {
        for &character in character.encode_utf16(&mut [0; 2]).iter() {
            shared.input(
                generation,
                BrowserInput::Key {
                    kind: KeyKind::Character,
                    modifiers,
                    windows_key_code: i32::from(character),
                    native_key_code: 0,
                    is_system_key: modifiers.alt,
                    character,
                    unmodified_character: character,
                },
            );
        }
    }
}
fn decode(input: InputEvent, shared: &Shared, generation: u64) {
    let mut modifiers = modifiers();
    match input {
        InputEvent::Cursor(cursor) => {
            let x = cursor.pos.x;
            let y = cursor.pos.y;
            let input = match cursor.event {
                CursorEvent::Move | CursorEvent::Enter => BrowserInput::Move {
                    x,
                    y,
                    modifiers,
                    leave: false,
                },
                CursorEvent::Leave => BrowserInput::Move {
                    x,
                    y,
                    modifiers,
                    leave: true,
                },
                CursorEvent::Scroll { axis, delta } => BrowserInput::Wheel {
                    x,
                    y,
                    modifiers,
                    delta_x: if axis == ScrollAxis::X {
                        i32::from(delta)
                    } else {
                        0
                    },
                    delta_y: if axis == ScrollAxis::Y {
                        -i32::from(delta)
                    } else {
                        0
                    },
                },
                CursorEvent::Action { state, action } => {
                    let button = match action {
                        CursorAction::Left => MouseButton::Left,
                        CursorAction::Middle => MouseButton::Middle,
                        CursorAction::Right => MouseButton::Right,
                        CursorAction::Back | CursorAction::Forward => return,
                    };
                    let up = matches!(state, CursorInputState::Released);
                    match button {
                        MouseButton::Left => modifiers.left_button = !up,
                        MouseButton::Middle => modifiers.middle_button = !up,
                        MouseButton::Right => modifiers.right_button = !up,
                    }
                    let click_count = match state {
                        CursorInputState::Pressed { click_count } => {
                            click_count.min(i32::MAX as u32) as i32
                        }
                        CursorInputState::Released => 1,
                    };
                    BrowserInput::Button {
                        x,
                        y,
                        modifiers,
                        button,
                        up,
                        click_count,
                    }
                }
            };
            shared.input(generation, input);
        }
        InputEvent::Keyboard(KeyboardInput::Key { key, state }) => {
            let code = i32::from(key.code.get());
            let up = state == KeyInputState::Released;
            match code {
                0x10 | 0xA0 | 0xA1 => modifiers.shift = !up,
                0x11 | 0xA2 | 0xA3 => modifiers.control = !up,
                0x12 | 0xA4 | 0xA5 => modifiers.alt = !up,
                0x5B | 0x5C => modifiers.meta = !up,
                _ => {}
            }
            modifiers.keypad = (0x60..=0x6f).contains(&code) || (code == 0x0d && key.extended);
            if matches!(code, 0x10..=0x12 | 0x5B..=0x5C | 0xA0..=0xA5) {
                modifiers.right_key =
                    key.extended || code == 0xA1 || code == 0xA3 || code == 0xA5 || code == 0x5C;
                modifiers.left_key = !modifiers.right_key;
            }
            // The pinned Windows DLL emits Win32 scan codes here, despite the
            // SDK's USB-code documentation. Preserve layout and handedness.
            let native_key_code = 1
                | (i32::from(key.physical_code.get()) << 16)
                | (i32::from(key.extended) << 24)
                | (i32::from(modifiers.alt) << 29)
                | if up {
                    (1u32 << 30 | 1u32 << 31) as i32
                } else {
                    0
                };
            shared.input(
                generation,
                BrowserInput::Key {
                    kind: if up { KeyKind::Up } else { KeyKind::RawDown },
                    modifiers,
                    windows_key_code: code,
                    native_key_code,
                    is_system_key: modifiers.alt,
                    character: 0,
                    unmodified_character: 0,
                },
            );
        }
        InputEvent::Keyboard(KeyboardInput::Char(character)) => {
            characters(std::iter::once(character), modifiers, shared, generation)
        }
        InputEvent::Keyboard(KeyboardInput::Ime(Ime::Commit(text))) => {
            characters(text.chars(), modifiers, shared, generation)
        }
        InputEvent::Keyboard(KeyboardInput::Ime(_)) => {}
    }
}
