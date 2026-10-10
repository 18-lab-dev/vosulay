//! Dispatches the default GLib main context beside CEF's external pump.
//!
//! On Linux, Chromium watches its X11 or Wayland display connection through
//! GLib sources on the default main context. `CefDoMessageLoopWork` only runs
//! Chromium's task queue, so CEF's reference external pump also iterates that
//! context. Without it, selection and data-offer events are never handled:
//! copying never answers other clients, and a paste waits forever for the
//! clipboard, blocking the page.
//!
//! The host thread sleeps in `park_timeout`, so a waker thread polls GLib's
//! descriptors and unparks it when one becomes ready.

use libc::{c_int, c_void, pollfd};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, Thread},
    time::{Duration, Instant},
};

type Context = *mut c_void;

struct Api {
    default: unsafe extern "C" fn() -> Context,
    acquire: unsafe extern "C" fn(Context) -> c_int,
    release: unsafe extern "C" fn(Context),
    prepare: unsafe extern "C" fn(Context, *mut c_int) -> c_int,
    query: unsafe extern "C" fn(Context, c_int, *mut c_int, *mut pollfd, c_int) -> c_int,
    check: unsafe extern "C" fn(Context, c_int, *mut pollfd, c_int) -> c_int,
    dispatch: unsafe extern "C" fn(Context),
}

impl Api {
    /// Binds the GLib that libcef already loaded; a second copy would own a
    /// different default context.
    fn load() -> Option<Self> {
        let library = unsafe {
            libc::dlopen(
                c"libglib-2.0.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_NOLOAD,
            )
        };
        if library.is_null() {
            return None;
        }
        macro_rules! symbol {
            ($name:literal) => {{
                let pointer = unsafe { libc::dlsym(library, $name.as_ptr()) };
                if pointer.is_null() {
                    return None;
                }
                // SAFETY: the GLib 2 ABI for this symbol matches the field type.
                unsafe { std::mem::transmute_copy(&pointer) }
            }};
        }
        Some(Self {
            default: symbol!(c"g_main_context_default"),
            acquire: symbol!(c"g_main_context_acquire"),
            release: symbol!(c"g_main_context_release"),
            prepare: symbol!(c"g_main_context_prepare"),
            query: symbol!(c"g_main_context_query"),
            check: symbol!(c"g_main_context_check"),
            dispatch: symbol!(c"g_main_context_dispatch"),
        })
    }
}

/// Bounds one pump so a source that is always ready cannot starve the host.
const MAX_DISPATCHES: usize = 16;

pub(crate) struct GlibPump {
    api: Api,
    context: Context,
    fds: Vec<pollfd>,
    deadline: Option<Instant>,
    waker: Arc<Waker>,
}

impl GlibPump {
    /// Call on the CEF main thread after `cef_initialize`.
    pub(crate) fn new(host: Thread) -> Option<Self> {
        let api = Api::load()?;
        let context = unsafe { (api.default)() };
        let waker = Waker::start(host)?;
        Some(Self {
            api,
            context,
            fds: Vec::new(),
            deadline: Some(Instant::now()),
            waker,
        })
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Dispatches ready sources, then arms the waker with GLib's descriptors.
    pub(crate) fn pump(&mut self) {
        let api = &self.api;
        if unsafe { (api.acquire)(self.context) } == 0 {
            // Another thread owns the default context; it dispatches it.
            self.deadline = None;
            return;
        }
        self.deadline = Some(Instant::now());
        for _ in 0..MAX_DISPATCHES {
            let mut priority = 0;
            unsafe { (api.prepare)(self.context, &mut priority) };
            let mut timeout = -1;
            let count = loop {
                let capacity = self.fds.len() as c_int;
                let count = unsafe {
                    (api.query)(
                        self.context,
                        priority,
                        &mut timeout,
                        self.fds.as_mut_ptr(),
                        capacity,
                    )
                };
                if count <= capacity {
                    break count;
                }
                self.fds.resize(
                    count as usize,
                    pollfd {
                        fd: -1,
                        events: 0,
                        revents: 0,
                    },
                );
            };
            let ready = &mut self.fds[..count as usize];
            for fd in ready.iter_mut() {
                fd.revents = 0;
            }
            unsafe { libc::poll(ready.as_mut_ptr(), ready.len() as _, 0) };
            if unsafe { (api.check)(self.context, priority, ready.as_mut_ptr(), count) } != 0 {
                unsafe { (api.dispatch)(self.context) };
                continue;
            }
            self.deadline = u64::try_from(timeout)
                .ok()
                .map(|timeout| Instant::now() + Duration::from_millis(timeout));
            self.waker.arm(ready);
            break;
        }
        unsafe { (api.release)(self.context) };
    }
}

struct Waker {
    host: Thread,
    armed: Mutex<Option<Vec<pollfd>>>,
    changed: Condvar,
    /// Interrupts the waker's poll when the main thread re-arms it.
    interrupt: [c_int; 2],
}

// SAFETY: pollfd is plain data; the pipe descriptors are owned by the waker.
unsafe impl Send for Waker {}
unsafe impl Sync for Waker {}

impl Waker {
    fn start(host: Thread) -> Option<Arc<Self>> {
        let mut interrupt = [-1; 2];
        if unsafe { libc::pipe2(interrupt.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        let waker = Arc::new(Self {
            host,
            armed: Mutex::new(None),
            changed: Condvar::new(),
            interrupt,
        });
        let worker = waker.clone();
        // The waker lives for the process, like the CEF runtime it serves.
        thread::Builder::new()
            .name("vosulay-glib-waker".into())
            .spawn(move || worker.run())
            .ok()?;
        Some(waker)
    }

    fn arm(&self, fds: &[pollfd]) {
        let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
        *armed = Some(
            fds.iter()
                .map(|fd| pollfd {
                    fd: fd.fd,
                    events: fd.events,
                    revents: 0,
                })
                .collect(),
        );
        drop(armed);
        self.changed.notify_one();
        unsafe { libc::write(self.interrupt[1], [0_u8].as_ptr().cast(), 1) };
    }

    fn run(&self) {
        let mut fds = Vec::new();
        loop {
            let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
            while armed.is_none() {
                armed = self.changed.wait(armed).unwrap_or_else(|e| e.into_inner());
            }
            fds.clear();
            fds.push(pollfd {
                fd: self.interrupt[0],
                events: libc::POLLIN,
                revents: 0,
            });
            fds.extend(armed.as_ref().into_iter().flatten().copied());
            drop(armed);
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } <= 0 {
                continue;
            }
            if fds[0].revents != 0 {
                let mut buffer = [0_u8; 64];
                while unsafe {
                    libc::read(self.interrupt[0], buffer.as_mut_ptr().cast(), buffer.len())
                } > 0
                {}
            }
            if fds[1..].iter().any(|fd| fd.revents != 0) {
                // Readiness stays set until the main thread dispatches, so wait
                // for the next arm instead of spinning on the same descriptors.
                let mut armed = self.armed.lock().unwrap_or_else(|e| e.into_inner());
                *armed = None;
                drop(armed);
                self.host.unpark();
            }
        }
    }
}
