//! X11 focus observation and editor cursor control stay off the application's
//! presentation thread and use their own XCB connection: XCB reports errors per
//! request, whereas Xlib errors on a vanished window would reach the
//! application's global handler.
use parking_lot::Mutex;
use std::{
    collections::HashMap,
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
    generate_id: unsafe extern "C" fn(*mut c_void) -> u32,
    open_font: unsafe extern "C" fn(*mut c_void, u32, u16, *const c_char) -> Cookie,
    #[allow(clippy::type_complexity)]
    create_glyph_cursor: unsafe extern "C" fn(
        *mut c_void,
        u32,
        u32,
        u32,
        u16,
        u16,
        u16,
        u16,
        u16,
        u16,
        u16,
        u16,
    ) -> Cookie,
    change_window_attributes: unsafe extern "C" fn(*mut c_void, u32, u32, *const u32) -> Cookie,
    flush: unsafe extern "C" fn(*mut c_void) -> c_int,
    poll_for_event: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pid_atom: u32,
}

// XCB connections are thread-safe; the observer and cursor hooks share one.
unsafe impl Send for Xcb {}
unsafe impl Sync for Xcb {}

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
            generate_id: symbol!(c"xcb_generate_id"),
            open_font: symbol!(c"xcb_open_font"),
            create_glyph_cursor: symbol!(c"xcb_create_glyph_cursor"),
            change_window_attributes: symbol!(c"xcb_change_window_attributes"),
            flush: symbol!(c"xcb_flush"),
            poll_for_event: symbol!(c"xcb_poll_for_event"),
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

    unsafe fn set_cursor(&self, window: u32, cursor: u32) {
        // XCB_CW_CURSOR
        unsafe { (self.change_window_attributes)(self.connection, window, 1 << 14, &cursor) };
    }

    /// The standard pointer from the core cursor font (XC_left_ptr), owned by
    /// this connection so it outlives any game-side resource.
    unsafe fn arrow(&self) -> u32 {
        let font = unsafe { (self.generate_id)(self.connection) };
        let cursor = unsafe { (self.generate_id)(self.connection) };
        let name = b"cursor";
        unsafe {
            (self.open_font)(
                self.connection,
                font,
                name.len() as u16,
                name.as_ptr().cast(),
            );
            (self.create_glyph_cursor)(
                self.connection,
                cursor,
                font,
                font,
                68,
                69,
                0,
                0,
                0,
                0xffff,
                0xffff,
                0xffff,
            );
        }
        cursor
    }

    /// Requests are unchecked; drop their errors (for example a window or
    /// game cursor destroyed meanwhile) so they do not accumulate.
    unsafe fn discard_events(&self) {
        loop {
            let event = unsafe { (self.poll_for_event)(self.connection) };
            if event.is_null() {
                return;
            }
            unsafe { libc::free(event) };
        }
    }
}

static CONNECTION: LazyLock<Option<Xcb>> = LazyLock::new(|| unsafe { Xcb::open() });

/// Cursors the game set on its windows. Games such as osu! hide the system
/// cursor, which would leave the editor without a pointer. The game's own
/// Display cannot be used to swap them: Wine sets cursors on a connection
/// that never polls events, and other threads' Xlib connections are not
/// guaranteed to be thread-safe.
struct Cursors {
    windows: HashMap<u32, u32>,
    editor: bool,
    arrow: u32,
}

static CURSORS: LazyLock<Mutex<Cursors>> = LazyLock::new(|| {
    Mutex::new(Cursors {
        windows: HashMap::new(),
        editor: false,
        arrow: 0,
    })
});

/// Records a cursor the game set; returns whether the editor's pointer is
/// shown instead, in which case the game's request must not reach the server.
pub(crate) fn define_cursor(window: u64, cursor: u64) -> bool {
    let (Ok(window), Ok(cursor)) = (u32::try_from(window), u32::try_from(cursor)) else {
        return false;
    };
    let mut cursors = CURSORS.lock();
    cursors.windows.insert(window, cursor);
    if !cursors.editor {
        return false;
    }
    if let Some(xcb) = CONNECTION.as_ref() {
        unsafe {
            xcb.set_cursor(window, cursors.arrow);
            (xcb.flush)(xcb.connection);
        }
    }
    true
}

pub(crate) fn forget_window(window: u64) {
    if let Ok(window) = u32::try_from(window) {
        CURSORS.lock().windows.remove(&window);
    }
}

unsafe fn sync_cursors(xcb: &Xcb, editor: bool) {
    let mut cursors = CURSORS.lock();
    if cursors.editor == editor {
        return;
    }
    if editor && cursors.arrow == 0 {
        cursors.arrow = unsafe { xcb.arrow() };
    }
    cursors.editor = editor;
    for (&window, &cursor) in &cursors.windows {
        unsafe { xcb.set_cursor(window, if editor { cursors.arrow } else { cursor }) };
    }
    unsafe { (xcb.flush)(xcb.connection) };
}

static OBSERVER: LazyLock<()> = LazyLock::new(|| {
    let _ = std::thread::Builder::new()
        .name("vosulay-xfocus".into())
        .spawn(|| unsafe {
            let Some(xcb) = CONNECTION.as_ref() else {
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
                sync_cursors(xcb, crate::hooks::x11_editing());
                xcb.discard_events();
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
