//! Editor input for Xlib games that do not use SDL, notably Wine's X11 driver.
//!
//! Events are taken from the game's own Xlib queue: configured shortcuts are
//! recognized there and, while the editor captures input, pointer and keyboard
//! events go to the editor instead of the game. A passive or active X grab from
//! another connection is not used: Wine may already hold a pointer grab for
//! ClipCursor, and a second grab would fail.
use crate::hooks::{capturing_x11, original, route_x11};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    sync::LazyLock,
    time::{Duration, Instant},
};
use vosulay_frame::{BrowserInput, InputModifiers, KeyKind, MouseButton};

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const GENERIC_EVENT: c_int = 35;

/// Xlib's `XKeyEvent`, `XButtonEvent` and `XMotionEvent` share this prefix;
/// `detail` is the keycode, button, or motion hint.
#[repr(C)]
struct InputEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    root: c_ulong,
    subwindow: c_ulong,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
    same_screen: c_int,
}

#[repr(C)]
struct GenericEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    extension: c_int,
}

type Predicate = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_char) -> c_int;
type CheckIfEvent =
    unsafe extern "C" fn(*mut c_void, *mut c_void, Option<Predicate>, *mut c_char) -> c_int;
type NextEvent = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type LookupString =
    unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_ulong, *mut c_void) -> c_int;
type LookupKeysym = unsafe extern "C" fn(*mut c_void, c_int) -> c_ulong;
type QueryExtension =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int, *mut c_int, *mut c_int) -> c_int;
type DefineCursor = unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong) -> c_int;
type CreateFontCursor = unsafe extern "C" fn(*mut c_void, c_uint) -> c_ulong;

static CHECK_IF_EVENT: LazyLock<Option<CheckIfEvent>> =
    LazyLock::new(|| unsafe { original(b"XCheckIfEvent\0") });
static NEXT_EVENT: LazyLock<Option<NextEvent>> =
    LazyLock::new(|| unsafe { original(b"XNextEvent\0") });
static LOOKUP_STRING: LazyLock<Option<LookupString>> =
    LazyLock::new(|| unsafe { original(b"XLookupString\0") });
static LOOKUP_KEYSYM: LazyLock<Option<LookupKeysym>> =
    LazyLock::new(|| unsafe { original(b"XLookupKeysym\0") });
static QUERY_EXTENSION: LazyLock<Option<QueryExtension>> =
    LazyLock::new(|| unsafe { original(b"XQueryExtension\0") });
static DEFINE_CURSOR: LazyLock<Option<DefineCursor>> =
    LazyLock::new(|| unsafe { original(b"XDefineCursor\0") });
static CREATE_FONT_CURSOR: LazyLock<Option<CreateFontCursor>> =
    LazyLock::new(|| unsafe { original(b"XCreateFontCursor\0") });

#[derive(Clone, Copy)]
enum KeyOwner {
    None,
    Game,
    Editor,
}

struct State {
    /// XInput2 major opcode per Display; raw motion would keep moving the
    /// game cursor under the editor.
    xinput: HashMap<usize, c_int>,
    /// Who received each keycode's press; its release goes to the same side.
    owners: [KeyOwner; 256],
    last_click: Option<(c_uint, c_ulong, i32)>,
    /// Cursors the game set per (Display, window): games such as osu! hide
    /// the system cursor, which would leave the editor without a pointer.
    cursors: HashMap<(usize, c_ulong), c_ulong>,
    /// Visible editor cursor per Display.
    arrows: HashMap<usize, c_ulong>,
    /// Whether each Display currently shows the editor cursor, and when it
    /// was last checked.
    cursor_shown: HashMap<usize, (bool, Instant)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        xinput: HashMap::new(),
        owners: [KeyOwner::None; 256],
        last_click: None,
        cursors: HashMap::new(),
        arrows: HashMap::new(),
        cursor_shown: HashMap::new(),
    })
});

/// Shows a normal pointer over the game's windows while the editor captures
/// input and restores the game's own cursors afterwards. Runs on the thread
/// that owns `display`, from inside its event polling.
unsafe fn sync_cursor(display: *mut c_void) {
    let key = display as usize;
    let now = Instant::now();
    let shown = {
        let mut state = STATE.lock();
        let entry = state.cursor_shown.entry(key).or_insert((false, now));
        if now.duration_since(entry.1) < Duration::from_millis(50) {
            return;
        }
        entry.1 = now;
        entry.0
    };
    let capturing = capturing_x11();
    if shown == capturing {
        return;
    }
    let (Some(define), Some(create)) = (*DEFINE_CURSOR, *CREATE_FONT_CURSOR) else {
        return;
    };
    let (windows, arrow) = {
        let mut state = STATE.lock();
        state.cursor_shown.insert(key, (capturing, now));
        let windows: Vec<_> = state
            .cursors
            .iter()
            .filter(|((owner, _), _)| *owner == key)
            .map(|((_, window), cursor)| (*window, *cursor))
            .collect();
        (windows, state.arrows.get(&key).copied())
    };
    if capturing {
        let arrow = arrow.unwrap_or_else(|| {
            // XC_left_ptr
            let arrow = unsafe { create(display, 68) };
            STATE.lock().arrows.insert(key, arrow);
            arrow
        });
        for (window, _) in windows {
            unsafe { define(display, window, arrow) };
        }
    } else {
        for (window, cursor) in windows {
            unsafe { define(display, window, cursor) };
        }
    }
}

/// # Safety
/// Xlib contract for `XDefineCursor`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn XDefineCursor(
    display: *mut c_void,
    window: c_ulong,
    cursor: c_ulong,
) -> c_int {
    let Some(define) = *DEFINE_CURSOR else {
        return 0;
    };
    let key = display as usize;
    let shown = {
        let mut state = STATE.lock();
        state.cursors.insert((key, window), cursor);
        state
            .cursor_shown
            .get(&key)
            .is_some_and(|(shown, _)| *shown)
    };
    // While editing, the game's cursor is applied when editing ends.
    if shown {
        1
    } else {
        unsafe { define(display, window, cursor) }
    }
}

fn modifiers(state: c_uint) -> InputModifiers {
    InputModifiers {
        shift: state & 1 != 0,
        caps_lock: state & 2 != 0,
        control: state & 4 != 0,
        alt: state & 8 != 0,
        num_lock: state & 16 != 0,
        meta: state & 64 != 0,
        left_button: state & 256 != 0,
        middle_button: state & 512 != 0,
        right_button: state & 1024 != 0,
        ..InputModifiers::default()
    }
}

/// Windows virtual-key code for an unshifted X keysym.
pub(crate) fn keysym_to_vk(keysym: c_ulong) -> Option<i32> {
    Some(match keysym {
        0x61..=0x7a => (keysym - 0x20) as i32,
        0x30..=0x39 => keysym as i32,
        0x20 => 0x20,
        0x3b => 0xba,
        0x3d => 0xbb,
        0x2c => 0xbc,
        0x2d => 0xbd,
        0x2e => 0xbe,
        0x2f => 0xbf,
        0x60 => 0xc0,
        0x5b => 0xdb,
        0x5c => 0xdc,
        0x5d => 0xdd,
        0x27 => 0xde,
        0xff08 => 0x08,
        0xff09 | 0xfe20 => 0x09,
        0xff0d | 0xff8d => 0x0d,
        0xff13 => 0x13,
        0xff14 => 0x91,
        0xff1b => 0x1b,
        0xff50 => 0x24,
        0xff51 => 0x25,
        0xff52 => 0x26,
        0xff53 => 0x27,
        0xff54 => 0x28,
        0xff55 => 0x21,
        0xff56 => 0x22,
        0xff57 => 0x23,
        0xff61 => 0x2c,
        0xff63 => 0x2d,
        0xff67 => 0x5d,
        0xff7f => 0x90,
        0xffaa => 0x6a,
        0xffab => 0x6b,
        0xffad => 0x6d,
        0xffae => 0x6e,
        0xffaf => 0x6f,
        0xffb0..=0xffb9 => (keysym - 0xffb0 + 0x60) as i32,
        0xffbe..=0xffd5 => (keysym - 0xffbe + 0x70) as i32,
        0xffe1 | 0xffe2 => 0x10,
        0xffe3 | 0xffe4 => 0x11,
        0xffe5 => 0x14,
        0xffe9 | 0xffea | 0xff7e => 0x12,
        0xffeb | 0xffec => 0x5b,
        0xffff => 0x2e,
        _ => return None,
    })
}

/// Unicode text for a keysym: Latin-1 keysyms are code points, and
/// `0x0100_0000 + code point` encodes the rest.
fn keysym_char(keysym: c_ulong) -> Option<u16> {
    let code = match keysym {
        0x20..=0x7e | 0xa0..=0xff => keysym as u32,
        0x0100_0000..=0x0100_ffff => (keysym - 0x0100_0000) as u32,
        _ => return None,
    };
    u16::try_from(code).ok().filter(|code| *code >= 0x20)
}

unsafe fn xinput_opcode(display: *mut c_void) -> c_int {
    let key = display as usize;
    if let Some(opcode) = STATE.lock().xinput.get(&key) {
        return *opcode;
    }
    let (mut opcode, mut event, mut error) = (0, 0, 0);
    let found = QUERY_EXTENSION.is_some_and(|query| unsafe {
        query(
            display,
            c"XInputExtension".as_ptr(),
            &mut opcode,
            &mut event,
            &mut error,
        ) != 0
    });
    let opcode = if found { opcode } else { -1 };
    STATE.lock().xinput.insert(key, opcode);
    opcode
}

/// Whether a key release goes to the editor rather than the game.
fn release_consumed(
    owner: KeyOwner,
    routed: Option<bool>,
    capturing: impl FnOnce() -> bool,
) -> bool {
    match owner {
        KeyOwner::Editor => true,
        KeyOwner::Game => false,
        KeyOwner::None => routed.unwrap_or_else(capturing),
    }
}

unsafe fn route_key(event: *mut c_void, down: bool) -> bool {
    let key = unsafe { &*event.cast::<InputEvent>() };
    let keycode = (key.detail & 0xff) as usize;
    let keysym = LOOKUP_KEYSYM.map_or(0, |lookup| unsafe { lookup(event, 0) });
    let mods = modifiers(key.state);
    // The host also observes releases of game-owned keys to pair shortcut
    // presses, so every mapped key is routed even when the game keeps it.
    let routed = keysym_to_vk(keysym).map(|vk| {
        route_x11(BrowserInput::Key {
            kind: if down { KeyKind::RawDown } else { KeyKind::Up },
            modifiers: mods,
            windows_key_code: vk,
            native_key_code: key.detail as i32,
            is_system_key: mods.alt,
            character: 0,
            unmodified_character: 0,
        })
    });
    if !down {
        // A release follows its press: a game-owned key must not stay held
        // in the game because editing started in between.
        let owner = std::mem::replace(&mut STATE.lock().owners[keycode], KeyOwner::None);
        return release_consumed(owner, routed, capturing_x11);
    }
    let consumed = routed.unwrap_or_else(capturing_x11);
    STATE.lock().owners[keycode] = if consumed {
        KeyOwner::Editor
    } else {
        KeyOwner::Game
    };
    if consumed && routed.is_some() && !mods.control && !mods.alt && !mods.meta {
        let mut text = [0 as c_char; 8];
        let mut shifted: c_ulong = 0;
        let length = LOOKUP_STRING.map_or(0, |lookup| unsafe {
            lookup(
                event,
                text.as_mut_ptr(),
                text.len() as c_int,
                &mut shifted,
                std::ptr::null_mut(),
            )
        });
        if let Some(character) = keysym_char(shifted).filter(|_| length > 0) {
            route_x11(BrowserInput::Key {
                kind: KeyKind::Character,
                modifiers: mods,
                windows_key_code: i32::from(character),
                native_key_code: key.detail as i32,
                is_system_key: false,
                character,
                unmodified_character: keysym_char(keysym).unwrap_or(character),
            });
        }
    }
    consumed
}

unsafe fn route_pointer(event: *mut c_void, kind: c_int) -> bool {
    if !capturing_x11() {
        return false;
    }
    let pointer = unsafe { &*event.cast::<InputEvent>() };
    let (x, y) = (pointer.x, pointer.y);
    let mut mods = modifiers(pointer.state);
    match kind {
        MOTION_NOTIFY => {
            route_x11(BrowserInput::Move {
                x,
                y,
                modifiers: mods,
                leave: false,
            });
        }
        _ => {
            let down = kind == BUTTON_PRESS;
            let (delta_x, delta_y) = match pointer.detail {
                4 => (0, 120),
                5 => (0, -120),
                6 => (120, 0),
                7 => (-120, 0),
                _ => (0, 0),
            };
            if delta_x != 0 || delta_y != 0 {
                if down {
                    route_x11(BrowserInput::Wheel {
                        x,
                        y,
                        modifiers: mods,
                        delta_x,
                        delta_y,
                    });
                }
                return true;
            }
            let button = match pointer.detail {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                _ => return true,
            };
            // `state` precedes the event; reflect this button's new state.
            match button {
                MouseButton::Left => mods.left_button = down,
                MouseButton::Middle => mods.middle_button = down,
                MouseButton::Right => mods.right_button = down,
            }
            let click_count = {
                let mut state = STATE.lock();
                let count = match state.last_click {
                    Some((last, time, count))
                        if down
                            && last == pointer.detail
                            && pointer.time.wrapping_sub(time) <= 500 =>
                    {
                        count + 1
                    }
                    Some((last, _, count)) if !down && last == pointer.detail => count,
                    _ => 1,
                };
                if down {
                    state.last_click = Some((pointer.detail, pointer.time, count));
                }
                count
            };
            route_x11(BrowserInput::Button {
                x,
                y,
                modifiers: mods,
                button,
                up: !down,
                click_count,
            });
        }
    }
    true
}

/// Whether the editor took this event, so the game must not see it.
unsafe fn consumed(event: *mut c_void) -> bool {
    let kind = unsafe { *event.cast::<c_int>() } & 0x7f;
    match kind {
        KEY_PRESS | KEY_RELEASE => unsafe { route_key(event, kind == KEY_PRESS) },
        BUTTON_PRESS | BUTTON_RELEASE | MOTION_NOTIFY => unsafe { route_pointer(event, kind) },
        GENERIC_EVENT => {
            let generic = unsafe { &*event.cast::<GenericEvent>() };
            let opcode = unsafe { xinput_opcode(generic.display) };
            opcode >= 0 && generic.extension == opcode && capturing_x11()
        }
        _ => false,
    }
}

/// # Safety
/// Xlib contract for `XCheckIfEvent`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn XCheckIfEvent(
    display: *mut c_void,
    event: *mut c_void,
    predicate: Option<Predicate>,
    argument: *mut c_char,
) -> c_int {
    let Some(check) = *CHECK_IF_EVENT else {
        return 0;
    };
    unsafe { sync_cursor(display) };
    loop {
        let found = unsafe { check(display, event, predicate, argument) };
        if found == 0 || !unsafe { consumed(event) } {
            return found;
        }
    }
}

/// # Safety
/// Xlib contract for `XNextEvent`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn XNextEvent(display: *mut c_void, event: *mut c_void) -> c_int {
    let Some(next) = *NEXT_EVENT else {
        return 0;
    };
    unsafe { sync_cursor(display) };
    loop {
        let result = unsafe { next(display, event) };
        if !unsafe { consumed(event) } {
            return result;
        }
    }
}

pub(crate) fn hooked(name: &[u8]) -> Option<*const c_void> {
    Some(match name {
        b"XCheckIfEvent" => XCheckIfEvent as _,
        b"XNextEvent" => XNextEvent as _,
        b"XDefineCursor" => XDefineCursor as _,
        _ => return None,
    })
}

/// Destroyed windows must not receive restored cursors.
pub(crate) fn forget_window(display: *mut c_void, window: c_ulong) {
    STATE.lock().cursors.remove(&(display as usize, window));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keysyms_to_the_virtual_keys_the_host_matches() {
        for (keysym, vk) in [
            (0x61, 0x41), // a
            (0x7a, 0x5a), // z
            (0x20, 0x20), // space
            (0x30, 0x30),
            (0xffbe, 0x70), // F1
            (0xffd5, 0x87), // F24
            (0xff1b, 0x1b), // Escape
            (0xffff, 0x2e), // Delete
            (0xffb5, 0x65), // KP_5
            (0x2f, 0xbf),
        ] {
            assert_eq!(keysym_to_vk(keysym), Some(vk), "{keysym:#x}");
        }
        assert_eq!(
            keysym_to_vk(0x41),
            None,
            "shifted keysyms are not looked up"
        );
    }

    #[test]
    fn decodes_latin1_and_unicode_keysyms() {
        assert_eq!(keysym_char(0x41), Some(0x41));
        assert_eq!(keysym_char(0xe9), Some(0xe9));
        assert_eq!(keysym_char(0x0100_20ac), Some(0x20ac));
        assert_eq!(keysym_char(0xff0d), None);
    }

    #[test]
    fn releases_follow_the_side_that_received_the_press() {
        // A key held in the game when editing starts must be released in the game.
        assert!(!release_consumed(KeyOwner::Game, Some(true), || true));
        assert!(release_consumed(KeyOwner::Editor, Some(false), || false));
        assert!(release_consumed(KeyOwner::None, None, || true));
        assert!(!release_consumed(KeyOwner::None, Some(false), || true));
    }
}
