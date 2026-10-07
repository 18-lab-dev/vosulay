//! X11 focus observation stays off the application presentation thread.
use std::{
    ffi::{c_char, c_int, c_ulong, c_void},
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
static FOCUS: AtomicU64 = AtomicU64::new(0);
static STAMP: AtomicU64 = AtomicU64::new(0);
static ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);
static OBSERVER: LazyLock<()> = LazyLock::new(|| {
    let _ = std::thread::Builder::new()
        .name("vosulay-xfocus".into())
        .spawn(|| unsafe {
            let library = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if library.is_null() {
                return;
            }
            type Open = unsafe extern "C" fn(*const c_char) -> *mut c_void;
            type Focus = unsafe extern "C" fn(*mut c_void, *mut c_ulong, *mut c_int) -> c_int;
            let open = libc::dlsym(library, c"XOpenDisplay".as_ptr());
            let focus = libc::dlsym(library, c"XGetInputFocus".as_ptr());
            if open.is_null() || focus.is_null() {
                return;
            }
            let open: Open = std::mem::transmute(open);
            let focus: Focus = std::mem::transmute(focus);
            let display = open(std::ptr::null());
            if display.is_null() {
                return;
            }
            loop {
                let mut window = 0;
                let mut revert = 0;
                if focus(display, &mut window, &mut revert) != 0 {
                    FOCUS.store(window, Ordering::Relaxed);
                    STAMP.store(ORIGIN.elapsed().as_millis() as u64 + 1, Ordering::Release);
                } else {
                    STAMP.store(0, Ordering::Release);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
});
pub(super) fn observe_x11() {
    LazyLock::force(&OBSERVER);
}
pub(super) fn focused(window: u64) -> bool {
    let stamp = STAMP.load(Ordering::Acquire);
    stamp != 0
        && (ORIGIN.elapsed().as_millis() as u64 + 1).saturating_sub(stamp) <= 100
        && FOCUS.load(Ordering::Relaxed) == window
}
