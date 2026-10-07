//! SDL3 native ABI; SDL2 records are never interpreted here.
use crate::sdl;
use libc::{c_char, c_void};
use std::ffi::CStr;
#[repr(C)]
struct Common {
    kind: u32,
    reserved: u32,
    timestamp: u64,
}
#[repr(C)]
struct Keyboard {
    common: Common,
    window: u32,
    which: u32,
    scancode: u32,
    key: u32,
    modifiers: u16,
    raw: u16,
    down: bool,
    repeat: bool,
}
#[repr(C)]
struct Text {
    common: Common,
    window: u32,
    text: *const c_char,
}
#[repr(C)]
struct Motion {
    common: Common,
    window: u32,
    which: u32,
    state: u32,
    x: f32,
    y: f32,
    xrel: f32,
    yrel: f32,
}
#[repr(C)]
struct Button {
    common: Common,
    window: u32,
    which: u32,
    button: u8,
    down: bool,
    clicks: u8,
    padding: u8,
    x: f32,
    y: f32,
}
#[repr(C)]
struct Wheel {
    common: Common,
    window: u32,
    which: u32,
    x: f32,
    y: f32,
    direction: u32,
    mouse_x: f32,
    mouse_y: f32,
    integer_x: i32,
    integer_y: i32,
}

fn keycode_to_vk(key: u32) -> Option<u16> {
    // SDL3 reassigned SDL2's media scancodes; only the <=257 range is shared.
    match key {
        0x40000106 | 0x40000107 | 0x4000010f => Some(0xb3),
        0x4000010b => Some(0xb0),
        0x4000010c => Some(0xb1),
        0x4000010d => Some(0xb2),
        0x40000102.. => None,
        _ => sdl::keycode_to_vk(key as i32),
    }
}
pub(crate) unsafe fn queued_key_event(event: *const c_void) -> Option<(u32, i32, bool)> {
    let kind = unsafe { *event.cast::<u32>() };
    if kind != 0x300 && kind != 0x301 {
        return None;
    }
    let event = unsafe { &*event.cast::<Keyboard>() };
    keycode_to_vk(event.key)?;
    Some((event.window, event.raw as i32, event.down))
}

pub(crate) unsafe fn route_event(event: *mut c_void) -> bool {
    let kind = unsafe { *event.cast::<u32>() };
    if !matches!(kind,0x20d..=0x20f|0x300|0x301|0x303|0x400..=0x403) {
        return false;
    }
    let id = unsafe { *event.cast::<u8>().add(16).cast::<u32>() };
    let Some(window) = sdl::window_identity(id) else {
        return false;
    };
    match kind {
        0x300 | 0x301 => {
            let e = unsafe { &*event.cast::<Keyboard>() };
            if let Some(vk) = keycode_to_vk(e.key) {
                let mut mods = sdl::modifiers(e.modifiers);
                mods.keypad = (83..=99).contains(&e.scancode);
                mods.left_key = (224..=227).contains(&e.scancode);
                mods.right_key = (228..=231).contains(&e.scancode);
                sdl::key(window, vk as i32, e.raw as i32, mods, e.down)
            } else {
                sdl::capture(window)
            }
        }
        0x303 => {
            let e = unsafe { &*event.cast::<Text>() };
            if e.text.is_null() {
                return false;
            }
            unsafe { CStr::from_ptr(e.text) }
                .to_str()
                .map(|text| sdl::text(window, text))
                .unwrap_or(false)
        }
        0x400 => {
            let e = unsafe { &mut *event.cast::<Motion>() };
            let captured = sdl::motion(window, e.x.round() as i32, e.y.round() as i32, e.state);
            let (dx, dy) = sdl::resume_mouse_motion(window, captured, e.xrel as f64, e.yrel as f64);
            e.xrel = dx as f32;
            e.yrel = dy as f32;
            captured
        }
        0x401 | 0x402 => {
            let e = unsafe { &*event.cast::<Button>() };
            sdl::button(
                window,
                e.x.round() as i32,
                e.y.round() as i32,
                e.button,
                e.down,
                e.clicks,
            )
        }
        0x403 => {
            let e = unsafe { &*event.cast::<Wheel>() };
            let sign = if e.direction == 1 { -120.0 } else { 120.0 };
            sdl::wheel(
                window,
                e.mouse_x.round() as i32,
                e.mouse_y.round() as i32,
                (e.x * sign).round() as i32,
                (e.y * sign).round() as i32,
            )
        }
        0x20d => sdl::send(
            window,
            vosulay_frame::BrowserInput::Move {
                x: 0,
                y: 0,
                modifiers: sdl::current_modifiers(),
                leave: true,
            },
        ),
        0x20e | 0x20f => {
            sdl::set_focus(window, kind == 0x20e);
            false
        }
        _ => false,
    }
}
