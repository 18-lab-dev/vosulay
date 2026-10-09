//! X11 focus observation stays off the application's presentation thread and
//! uses its own XCB connection: XCB reports errors per request, whereas Xlib
//! errors on a vanished window would reach the application's global handler.
use std::{
    ffi::{c_char, c_int, c_void},
    sync::{
        LazyLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static FOCUS: AtomicU64 = AtomicU64::new(0);
// Wine draws into an offscreen client window while X focus is on a separate
// top-level, so the focused window is matched to this process as well.
static OWNED: AtomicBool = AtomicBool::new(false);
static STAMP: AtomicU64 = AtomicU64::new(0);
static ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

#[repr(C)]
#[derive(Clone, Copy)]
struct Cookie(u32);

#[repr(C)]
struct FocusReply {
    response_type: u8,
    revert_to: u8,
    sequence: u16,
    length: u32,
    focus: u32,
}

#[repr(C)]
struct TreeReply {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    length: u32,
    root: u32,
    parent: u32,
}

#[repr(C)]
struct AtomReply {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    length: u32,
    atom: u32,
}

#[repr(C)]
struct PropertyReply {
    response_type: u8,
    format: u8,
    sequence: u16,
    length: u32,
    kind: u32,
    bytes_after: u32,
    value_len: u32,
}

struct Xcb {
    connection: *mut c_void,
    get_focus: unsafe extern "C" fn(*mut c_void) -> Cookie,
    get_focus_reply: unsafe extern "C" fn(*mut c_void, Cookie, *mut *mut c_void) -> *mut FocusReply,
    query_tree: unsafe extern "C" fn(*mut c_void, u32) -> Cookie,
    query_tree_reply: unsafe extern "C" fn(*mut c_void, Cookie, *mut *mut c_void) -> *mut TreeReply,
    get_property: unsafe extern "C" fn(*mut c_void, u8, u32, u32, u32, u32, u32) -> Cookie,
    get_property_reply:
        unsafe extern "C" fn(*mut c_void, Cookie, *mut *mut c_void) -> *mut PropertyReply,
    property_value: unsafe extern "C" fn(*const PropertyReply) -> *const c_void,
    pid_atom: u32,
}

impl Xcb {
    unsafe fn open() -> Option<Self> {
        let library =
            unsafe { libc::dlopen(c"libxcb.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return None;
        }
        macro_rules! symbol {
            ($name:literal) => {{
                let pointer = unsafe { libc::dlsym(library, $name.as_ptr()) };
                if pointer.is_null() {
                    return None;
                }
                unsafe { std::mem::transmute_copy(&pointer) }
            }};
        }
        let connect: unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut c_void =
            symbol!(c"xcb_connect");
        let has_error: unsafe extern "C" fn(*mut c_void) -> c_int =
            symbol!(c"xcb_connection_has_error");
        let intern: unsafe extern "C" fn(*mut c_void, u8, u16, *const c_char) -> Cookie =
            symbol!(c"xcb_intern_atom");
        let intern_reply: unsafe extern "C" fn(
            *mut c_void,
            Cookie,
            *mut *mut c_void,
        ) -> *mut AtomReply = symbol!(c"xcb_intern_atom_reply");
        let connection = unsafe { connect(std::ptr::null(), std::ptr::null_mut()) };
        if connection.is_null() || unsafe { has_error(connection) } != 0 {
            return None;
        }
        let name = b"_NET_WM_PID";
        let reply = unsafe {
            intern_reply(
                connection,
                intern(connection, 1, name.len() as u16, name.as_ptr().cast()),
                std::ptr::null_mut(),
            )
        };
        let pid_atom = if reply.is_null() {
            0
        } else {
            let atom = unsafe { (*reply).atom };
            unsafe { libc::free(reply.cast()) };
            atom
        };
        Some(Self {
            connection,
            get_focus: symbol!(c"xcb_get_input_focus"),
            get_focus_reply: symbol!(c"xcb_get_input_focus_reply"),
            query_tree: symbol!(c"xcb_query_tree"),
            query_tree_reply: symbol!(c"xcb_query_tree_reply"),
            get_property: symbol!(c"xcb_get_property"),
            get_property_reply: symbol!(c"xcb_get_property_reply"),
            property_value: symbol!(c"xcb_get_property_value"),
            pid_atom,
        })
    }

    /// Errors (including a window destroyed meanwhile) arrive as a null reply
    /// with an error object that must be freed.
    unsafe fn reply<T>(&self, reply: *mut T, error: *mut c_void) -> Option<*mut T> {
        if !error.is_null() {
            unsafe { libc::free(error) };
        }
        (!reply.is_null()).then_some(reply)
    }

    unsafe fn focus(&self) -> Option<u32> {
        let mut error = std::ptr::null_mut();
        let reply = unsafe {
            (self.get_focus_reply)(
                self.connection,
                (self.get_focus)(self.connection),
                &mut error,
            )
        };
        let reply = unsafe { self.reply(reply, error) }?;
        let focus = unsafe { (*reply).focus };
        unsafe { libc::free(reply.cast()) };
        Some(focus)
    }

    unsafe fn pid(&self, window: u32) -> Option<u32> {
        // XCB_ATOM_CARDINAL
        let mut error = std::ptr::null_mut();
        let reply = unsafe {
            (self.get_property_reply)(
                self.connection,
                (self.get_property)(self.connection, 0, window, self.pid_atom, 6, 0, 1),
                &mut error,
            )
        };
        let reply = unsafe { self.reply(reply, error) }?;
        let pid = unsafe {
            ((*reply).format == 32 && (*reply).value_len == 1)
                .then(|| *(self.property_value)(reply).cast::<u32>())
        };
        unsafe { libc::free(reply.cast()) };
        pid
    }

    unsafe fn parent(&self, window: u32) -> Option<u32> {
        let mut error = std::ptr::null_mut();
        let reply = unsafe {
            (self.query_tree_reply)(
                self.connection,
                (self.query_tree)(self.connection, window),
                &mut error,
            )
        };
        let reply = unsafe { self.reply(reply, error) }?;
        let (root, parent) = unsafe { ((*reply).root, (*reply).parent) };
        unsafe { libc::free(reply.cast()) };
        (parent != 0 && parent != root).then_some(parent)
    }

    /// Reparenting window managers put the client's `_NET_WM_PID` on a
    /// descendant of their frame, so every window up to the root is checked.
    unsafe fn owned(&self, mut window: u32) -> bool {
        if self.pid_atom == 0 || window <= 1 {
            return false;
        }
        let pid = std::process::id();
        for _ in 0..16 {
            if unsafe { self.pid(window) } == Some(pid) {
                return true;
            }
            match unsafe { self.parent(window) } {
                Some(parent) => window = parent,
                None => return false,
            }
        }
        false
    }
}

static OBSERVER: LazyLock<()> = LazyLock::new(|| {
    let _ = std::thread::Builder::new()
        .name("vosulay-xfocus".into())
        .spawn(|| unsafe {
            let Some(xcb) = Xcb::open() else {
                return;
            };
            let mut last = None;
            loop {
                match xcb.focus() {
                    Some(window) => {
                        if last != Some(window) {
                            OWNED.store(xcb.owned(window), Ordering::Relaxed);
                            last = Some(window);
                        }
                        FOCUS.store(u64::from(window), Ordering::Relaxed);
                        STAMP.store(ORIGIN.elapsed().as_millis() as u64 + 1, Ordering::Release);
                    }
                    None => STAMP.store(0, Ordering::Release),
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
});

pub(crate) fn observe() {
    LazyLock::force(&OBSERVER);
}

/// Whether X input focus is on `window` or on a window of this process.
pub(crate) fn focused(window: u64) -> bool {
    observe();
    let stamp = STAMP.load(Ordering::Acquire);
    stamp != 0
        && (ORIGIN.elapsed().as_millis() as u64 + 1).saturating_sub(stamp) <= 100
        && (FOCUS.load(Ordering::Relaxed) == window || OWNED.load(Ordering::Relaxed))
}
