// Adapted from storycraft/asdf-overlay c925244c8d14ca36d82585834cdfebf13d6ce86b.
// Copyright storycraft and contributors; MIT OR Apache-2.0.
// Vosulay: publish the forwarding procedure before installing a native callback.
use core::mem;

use asdf_overlay_event::{Event, SurfaceEvent};
use dashmap::Entry;
use once_cell::sync::Lazy;
use scopeguard::defer;
use tracing::{Level, debug, trace, warn};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        self as msg, CallWindowProcA, CallWindowProcW, GWLP_WNDPROC, GetWindowLongPtrA,
        GetWindowLongPtrW, IsWindowUnicode, SetWindowLongPtrA, SetWindowLongPtrW, WNDPROC,
    },
};

use crate::{
    event_sink::OverlayEventSink, hook::opengl::util::get_client_size, surface::Surfaces,
    types::IntDashMap,
};

// HWND -> last WNDPROC
static MAP: Lazy<IntDashMap<u32, WNDPROC>> = Lazy::new(IntDashMap::default);

pub fn install(hwnd: HWND) {
    let key = hwnd.0 as u32;
    if MAP.contains_key(&key) {
        return;
    }

    let unicode = unsafe { IsWindowUnicode(hwnd).as_bool() };
    let previous = unsafe {
        if unicode {
            GetWindowLongPtrW(hwnd, GWLP_WNDPROC)
        } else {
            GetWindowLongPtrA(hwnd, GWLP_WNDPROC)
        }
    };
    if previous == 0 {
        warn!("Cannot read OpenGL forwarding window procedure {key:#x}");
        return;
    }
    super::context::install_callback(
        || match MAP.entry(key) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                entry.insert(unsafe { mem::transmute::<isize, WNDPROC>(previous as isize) });
                true
            }
        },
        || {
            // Publish forwarding state and release the registry guard BEFORE the
            // native call: installation may synchronously enter our procedure.
            let actual = unsafe {
                if unicode {
                    SetWindowLongPtrW(hwnd, GWLP_WNDPROC, ogl_wnd_proc::<true> as *const () as _)
                } else {
                    SetWindowLongPtrA(hwnd, GWLP_WNDPROC, ogl_wnd_proc::<false> as *const () as _)
                }
            };
            if actual == 0 {
                MAP.remove(&key);
                warn!("Cannot install OpenGL window procedure {key:#x}");
            } else {
                // Retain the exact chain returned by Windows, including any other
                // procedure installed between our lookup and replacement.
                MAP.insert(key, unsafe {
                    mem::transmute::<isize, WNDPROC>(actual as isize)
                });
                debug!("Installed OpenGL window procedure {key:#x}");
            }
        },
    );
}

#[inline(always)]
fn proc(hwnd: u32, msg: u32, lparam: LPARAM) {
    let msg::WM_WINDOWPOSCHANGED = msg else {
        return;
    };

    let winpos = unsafe { &*(lparam.0 as *const msg::WINDOWPOS) };
    if winpos.flags.0 & msg::SWP_NOSIZE.0 != 0 {
        return;
    }

    let (width, height) = get_client_size(HWND(hwnd as _)).unwrap_or_default();
    for data in super::MAP.iter() {
        if data.hwnd != hwnd {
            continue;
        }

        let key = *data.key() as _;
        Surfaces::state(key, |state| {
            state.resize(width, height);
            OverlayEventSink::emit(Event::Surface {
                id: key,
                event: SurfaceEvent::Resized { width, height },
            });
        });
    }
}

#[tracing::instrument(level = Level::TRACE)]
unsafe extern "system" fn ogl_wnd_proc<const UNICODE: bool>(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    trace!("WndProc opengl hook called");

    let key = hwnd.0 as u32;
    defer!({
        // cleanup map
        if msg == msg::WM_NCDESTROY {
            trace!("cleanup ogl proc hook: {:?}", hwnd);
            MAP.remove(&key);
        }
    });
    let last_wnd_proc = *MAP.get(&key).unwrap();

    proc(key, msg, lparam);
    unsafe {
        if UNICODE {
            CallWindowProcW(last_wnd_proc, hwnd, msg, wparam, lparam)
        } else {
            CallWindowProcA(last_wnd_proc, hwnd, msg, wparam, lparam)
        }
    }
}
