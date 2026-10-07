//! SDL2/SDL3 presentation and canonical input capture.
use crate::{hooks, sdl3};
use libc::{c_char, c_int, c_void};
use std::{mem, sync::LazyLock};
use vosulay_frame::{BrowserInput, InputModifiers, KeyKind, MouseButton};
const KEY_STATE_WINDOW_LIMIT: usize = 16;
const KEY_STATE_WORDS: usize = 8;
const KEY_STATE_BITS: usize = KEY_STATE_WORDS * u64::BITS as usize;

#[derive(Clone, Copy)]
struct WindowKeyState {
    window: usize,
    game_down: [u64; KEY_STATE_WORDS],
    overlay_down: [u64; KEY_STATE_WORDS],
    release_pending: [u64; KEY_STATE_WORDS],
}
impl WindowKeyState {
    const EMPTY: Self = Self {
        window: 0,
        game_down: [0; KEY_STATE_WORDS],
        overlay_down: [0; KEY_STATE_WORDS],
        release_pending: [0; KEY_STATE_WORDS],
    };

    fn has(&self, keys: &[u64; KEY_STATE_WORDS], native: usize) -> bool {
        keys[native / u64::BITS as usize] & (1 << (native % u64::BITS as usize)) != 0
    }

    fn set(keys: &mut [u64; KEY_STATE_WORDS], native: usize, active: bool) {
        let word = &mut keys[native / u64::BITS as usize];
        let bit = 1 << (native % u64::BITS as usize);
        if active {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }

    fn empty(&self) -> bool {
        self.game_down.iter().all(|word| *word == 0)
            && self.overlay_down.iter().all(|word| *word == 0)
            && self.release_pending.iter().all(|word| *word == 0)
    }
}

#[derive(Clone, Copy)]
struct KeyRecipients {
    game_down: bool,
    overlay_down: bool,
    release_pending: bool,
}

#[derive(Clone, Copy)]
enum KeyRecipient {
    Game,
    Overlay,
    ReleasePending,
}

static WINDOW_KEY_STATES: LazyLock<parking_lot::Mutex<[WindowKeyState; KEY_STATE_WINDOW_LIMIT]>> =
    LazyLock::new(|| parking_lot::Mutex::new([WindowKeyState::EMPTY; KEY_STATE_WINDOW_LIMIT]));

fn key_index(native: i32) -> Option<usize> {
    let native = usize::try_from(native).ok()?;
    (native < KEY_STATE_BITS).then_some(native)
}

fn key_recipients(window: usize, native: i32) -> KeyRecipients {
    let Some(native) = key_index(native) else {
        return KeyRecipients {
            game_down: false,
            overlay_down: false,
            release_pending: false,
        };
    };
    let states = WINDOW_KEY_STATES.lock();
    let Some(state) = states.iter().find(|state| state.window == window) else {
        return KeyRecipients {
            game_down: false,
            overlay_down: false,
            release_pending: false,
        };
    };
    KeyRecipients {
        game_down: state.has(&state.game_down, native),
        overlay_down: state.has(&state.overlay_down, native),
        release_pending: state.has(&state.release_pending, native),
    }
}

fn set_key_recipient(window: usize, native: i32, recipient: KeyRecipient, active: bool) {
    let Some(native) = key_index(native) else {
        return;
    };
    if window == 0 {
        return;
    }
    let mut states = WINDOW_KEY_STATES.lock();
    let index = states
        .iter()
        .position(|state| state.window == window)
        .or_else(|| {
            active
                .then(|| states.iter().position(|state| state.window == 0))
                .flatten()
        });
    let Some(index) = index else {
        return;
    };
    let state = &mut states[index];
    if state.window == 0 {
        state.window = window;
    }
    match recipient {
        KeyRecipient::Game => WindowKeyState::set(&mut state.game_down, native, active),
        KeyRecipient::Overlay => WindowKeyState::set(&mut state.overlay_down, native, active),
        KeyRecipient::ReleasePending => {
            WindowKeyState::set(&mut state.release_pending, native, active)
        }
    }
    if state.empty() {
        *state = WindowKeyState::EMPTY;
    }
}

fn clear_window_key_state(window: usize) {
    let mut states = WINDOW_KEY_STATES.lock();
    if let Some(state) = states.iter_mut().find(|state| state.window == window) {
        *state = WindowKeyState::EMPTY;
    }
}

fn key_event_consumed(consumed: bool, down: bool, recipients: KeyRecipients) -> bool {
    if down {
        consumed
    } else if recipients.game_down || recipients.release_pending {
        false
    } else if recipients.overlay_down {
        true
    } else {
        consumed
    }
}
unsafe fn queued_key_event(event: *const c_void, sdl3: bool) -> Option<(usize, i32, bool)> {
    if event.is_null() {
        return None;
    }
    let kind = unsafe { *event.cast::<u32>() };
    if kind != SDL_KEYDOWN && kind != SDL_KEYUP {
        return None;
    }
    let (id, native, down) = if sdl3 {
        unsafe { sdl3::queued_key_event(event)? }
    } else {
        let event = unsafe { &*event.cast::<KeyboardEvent>() };
        keycode_to_vk(event.keysym.keycode)?;
        (event.window_id, event.keysym.scancode, kind == SDL_KEYDOWN)
    };
    Some((window_identity(id)?, native, down))
}

fn note_filtered_key(event: *const c_void, sdl3: bool, accepted: bool) {
    if !accepted {
        return;
    }
    let Some((window, native, down)) = (unsafe { queued_key_event(event, sdl3) }) else {
        return;
    };
    if down {
        set_key_recipient(window, native, KeyRecipient::Game, true);
        set_key_recipient(window, native, KeyRecipient::ReleasePending, false);
        return;
    }
    let recipients = key_recipients(window, native);
    if recipients.game_down {
        set_key_recipient(window, native, KeyRecipient::Game, false);
        set_key_recipient(window, native, KeyRecipient::ReleasePending, true);
    }
    if recipients.overlay_down {
        set_key_recipient(window, native, KeyRecipient::Overlay, false);
    }
}

fn note_queued_key_delivery(window: usize, native: i32, down: bool) {
    if down {
        set_key_recipient(window, native, KeyRecipient::Game, true);
        set_key_recipient(window, native, KeyRecipient::ReleasePending, false);
    } else {
        set_key_recipient(window, native, KeyRecipient::Game, false);
        set_key_recipient(window, native, KeyRecipient::Overlay, false);
        set_key_recipient(window, native, KeyRecipient::ReleasePending, false);
    }
}

#[derive(Clone, Copy)]
struct CapturedMotion {
    window: usize,
    dx: f64,
    dy: f64,
}
impl CapturedMotion {
    const EMPTY: Self = Self {
        window: 0,
        dx: 0.0,
        dy: 0.0,
    };
}
static CAPTURED_MOTION: LazyLock<parking_lot::Mutex<[CapturedMotion; 16]>> =
    LazyLock::new(|| parking_lot::Mutex::new([CapturedMotion::EMPTY; 16]));

fn restore_relative_motion(
    states: &mut [CapturedMotion],
    window: usize,
    captured: bool,
    dx: f64,
    dy: f64,
) -> (f64, f64) {
    if window == 0 {
        return (dx, dy);
    }
    let index = states
        .iter()
        .position(|state| state.window == window)
        .or_else(|| {
            captured
                .then(|| states.iter().position(|state| state.window == 0))
                .flatten()
        });
    let Some(index) = index else {
        return (dx, dy);
    };
    let state = &mut states[index];
    if captured {
        state.window = window;
        state.dx += dx;
        state.dy += dy;
        (dx, dy)
    } else {
        // Absolute SDL coordinates move during editing; relative-input games
        // never see those suppressed deltas. Reconcile once, without replaying
        // editor buttons or forwarding a second motion event.
        let pending = mem::replace(state, CapturedMotion::EMPTY);
        (dx + pending.dx, dy + pending.dy)
    }
}

pub(crate) fn resume_mouse_motion(window: usize, captured: bool, dx: f64, dy: f64) -> (f64, f64) {
    restore_relative_motion(&mut *CAPTURED_MOTION.lock(), window, captured, dx, dy)
}
const SDL_WINDOWEVENT: u32 = 0x200;
const SDL_KEYDOWN: u32 = 0x300;
const SDL_KEYUP: u32 = 0x301;
const SDL_TEXTINPUT: u32 = 0x303;
const SDL_MOUSEMOTION: u32 = 0x400;
const SDL_MOUSEBUTTONDOWN: u32 = 0x401;
const SDL_MOUSEBUTTONUP: u32 = 0x402;
const SDL_MOUSEWHEEL: u32 = 0x403;
const WINDOW_LEAVE: u8 = 11;
const WINDOW_FOCUS_GAINED: u8 = 12;
const WINDOW_FOCUS_LOST: u8 = 13;
const SYSWM_X11: u32 = 2;
const SYSWM_WAYLAND: u32 = 6;
const KMOD_SHIFT: u16 = 0x0003;
const KMOD_CTRL: u16 = 0x00c0;
const KMOD_ALT: u16 = 0x0300;
const KMOD_NUM: u16 = 0x1000;
const KMOD_CAPS: u16 = 0x2000;
const KMOD_GUI: u16 = 0x0c00;
const SUPPRESSION_BUDGET: usize = 64;
const SDL_SCANCODE_MASK: i32 = 1 << 30;
const fn sdl_scancode(scancode: i32) -> i32 {
    SDL_SCANCODE_MASK | scancode
}
const SDLK_CAPSLOCK: i32 = sdl_scancode(57);
const SDLK_F1: i32 = sdl_scancode(58);
const SDLK_F12: i32 = sdl_scancode(69);
const SDLK_F13: i32 = sdl_scancode(104);
const SDLK_F24: i32 = sdl_scancode(115);
const SDLK_PRINTSCREEN: i32 = sdl_scancode(70);
const SDLK_SCROLLLOCK: i32 = sdl_scancode(71);
const SDLK_INSERT: i32 = sdl_scancode(73);
const SDLK_HOME: i32 = sdl_scancode(74);
const SDLK_PAGEUP: i32 = sdl_scancode(75);
const SDLK_END: i32 = sdl_scancode(77);
const SDLK_PAGEDOWN: i32 = sdl_scancode(78);
const SDLK_RIGHT: i32 = sdl_scancode(79);
const SDLK_LEFT: i32 = sdl_scancode(80);
const SDLK_DOWN: i32 = sdl_scancode(81);
const SDLK_UP: i32 = sdl_scancode(82);
const SDLK_NUMLOCKCLEAR: i32 = sdl_scancode(83);
const SDLK_KP_DIVIDE: i32 = sdl_scancode(84);
const SDLK_KP_MULTIPLY: i32 = sdl_scancode(85);
const SDLK_KP_MINUS: i32 = sdl_scancode(86);
const SDLK_KP_PLUS: i32 = sdl_scancode(87);
const SDLK_KP_ENTER: i32 = sdl_scancode(88);
const SDLK_KP_1: i32 = sdl_scancode(89);
const SDLK_KP_9: i32 = sdl_scancode(97);
const SDLK_KP_0: i32 = sdl_scancode(98);
const SDLK_KP_PERIOD: i32 = sdl_scancode(99);
const SDLK_APPLICATION: i32 = sdl_scancode(101);
const SDLK_MUTE: i32 = sdl_scancode(127);
const SDLK_VOLUMEUP: i32 = sdl_scancode(128);
const SDLK_VOLUMEDOWN: i32 = sdl_scancode(129);
const SDLK_KP_COMMA: i32 = sdl_scancode(133);
const SDLK_LCTRL: i32 = sdl_scancode(224);
const SDLK_LSHIFT: i32 = sdl_scancode(225);
const SDLK_LALT: i32 = sdl_scancode(226);
const SDLK_LGUI: i32 = sdl_scancode(227);
const SDLK_RCTRL: i32 = sdl_scancode(228);
const SDLK_RSHIFT: i32 = sdl_scancode(229);
const SDLK_RALT: i32 = sdl_scancode(230);
const SDLK_RGUI: i32 = sdl_scancode(231);
const SDLK_MODE: i32 = sdl_scancode(257);
const SDLK_AUDIONEXT: i32 = sdl_scancode(258);
const SDLK_AUDIOPREV: i32 = sdl_scancode(259);
const SDLK_AUDIOSTOP: i32 = sdl_scancode(260);
const SDLK_AUDIOPLAY: i32 = sdl_scancode(261);
const SDLK_AUDIOMUTE: i32 = sdl_scancode(262);

#[repr(C)]
#[derive(Clone, Copy)]
struct WindowEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    event: u8,
    padding: [u8; 3],
    data1: i32,
    data2: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeySym {
    scancode: i32,
    keycode: i32,
    modifiers: u16,
    unused: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    state: u8,
    repeat: u8,
    padding: [u8; 2],
    keysym: KeySym,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TextEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    text: [c_char; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MotionEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    which: u32,
    state: u32,
    x: i32,
    y: i32,
    x_relative: i32,
    y_relative: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ButtonEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    which: u32,
    button: u8,
    state: u8,
    clicks: u8,
    padding: u8,
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WheelEvent {
    kind: u32,
    timestamp: u32,
    window_id: u32,
    which: u32,
    x: i32,
    y: i32,
    direction: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Version {
    major: u8,
    minor: u8,
    patch: u8,
}

#[repr(C)]
struct WmInfo {
    version: Version,
    padding: u8,
    subsystem: u32,
    information: [usize; 8],
}

type Poll = unsafe extern "C" fn(*mut c_void) -> c_int;
type GetWindowFromId = unsafe extern "C" fn(u32) -> *mut c_void;
type GetWindowWmInfo = unsafe extern "C" fn(*mut c_void, *mut WmInfo) -> c_int;
type GetVersion = unsafe extern "C" fn(*mut Version);
type GetCurrentWindow = unsafe extern "C" fn() -> *mut c_void;
pub(crate) fn keycode_to_vk(keycode: i32) -> Option<u16> {
    Some(match keycode {
        97..=122 => (keycode as u8).to_ascii_uppercase() as u16,
        65..=90 | 48..=57 => keycode as u16,
        8 => 0x08,
        9 => 0x09,
        13 | SDLK_KP_ENTER => 0x0d,
        27 => 0x1b,
        32 => 0x20,
        127 => 0x2e,
        59 => 0xba,
        61 => 0xbb,
        44 => 0xbc,
        45 => 0xbd,
        46 => 0xbe,
        47 => 0xbf,
        96 => 0xc0,
        91 => 0xdb,
        92 => 0xdc,
        93 => 0xdd,
        39 => 0xde,
        SDLK_CAPSLOCK => 0x14,
        SDLK_PRINTSCREEN => 0x2c,
        SDLK_SCROLLLOCK => 0x91,
        SDLK_INSERT => 0x2d,
        SDLK_HOME => 0x24,
        SDLK_PAGEUP => 0x21,
        SDLK_END => 0x23,
        SDLK_PAGEDOWN => 0x22,
        SDLK_RIGHT => 0x27,
        SDLK_LEFT => 0x25,
        SDLK_DOWN => 0x28,
        SDLK_UP => 0x26,
        SDLK_NUMLOCKCLEAR => 0x90,
        SDLK_KP_DIVIDE => 0x6f,
        SDLK_KP_MULTIPLY => 0x6a,
        SDLK_KP_MINUS => 0x6d,
        SDLK_KP_PLUS => 0x6b,
        value @ SDLK_KP_1..=SDLK_KP_9 => 0x61 + (value - SDLK_KP_1) as u16,
        SDLK_KP_0 => 0x60,
        SDLK_KP_PERIOD => 0x6e,
        SDLK_KP_COMMA => 0x6c,
        SDLK_APPLICATION => 0x5d,
        value @ SDLK_F1..=SDLK_F12 => 0x70 + (value - SDLK_F1) as u16,
        value @ SDLK_F13..=SDLK_F24 => 0x7c + (value - SDLK_F13) as u16,
        SDLK_LSHIFT | SDLK_RSHIFT => 0x10,
        SDLK_LCTRL | SDLK_RCTRL => 0x11,
        SDLK_LALT | SDLK_RALT => 0x12,
        SDLK_LGUI => 0x5b,
        SDLK_RGUI => 0x5c,
        SDLK_MODE => 0xe1,
        SDLK_AUDIONEXT => 0xb0,
        SDLK_AUDIOPREV => 0xb1,
        SDLK_AUDIOSTOP => 0xb2,
        SDLK_AUDIOPLAY => 0xb3,
        SDLK_MUTE | SDLK_AUDIOMUTE => 0xb5,
        SDLK_VOLUMEDOWN => 0xb6,
        SDLK_VOLUMEUP => 0xb7,
        _ => return None,
    })
}

struct Api {
    poll: usize,
    swap: usize,
    destroy: usize,
    size: usize,
    current: usize,
    context: usize,
    proc: usize,
    focus: usize,
    from_id: usize,
    mod_state: usize,
    keyboard: usize,
    mouse: usize,
    relative: usize,
    global: usize,
    wm: usize,
    version: usize,
    properties: usize,
    pointer: usize,
    number: usize,
    display: usize,
    mode: usize,
    sdl3: bool,
    logical: usize,
    logical_sdl3: bool,
    pixels: usize,
    pixels_sdl3: bool,
}
fn symbol(name: &[u8]) -> usize {
    unsafe { hooks::real_symbol(name) as usize }
}
static API: LazyLock<Api> = LazyLock::new(|| {
    let poll = symbol(b"SDL_PollEvent\0");
    let properties = symbol(b"SDL_GetWindowProperties\0");
    let mut a: libc::Dl_info = unsafe { mem::zeroed() };
    let mut b: libc::Dl_info = unsafe { mem::zeroed() };
    let sdl3 = poll != 0
        && properties != 0
        && unsafe {
            libc::dladdr(poll as _, &mut a) != 0
                && libc::dladdr(properties as _, &mut b) != 0
                && a.dli_fbase == b.dli_fbase
        };
    let logical = symbol(b"SDL_GetWindowSize\0");
    let pixels = symbol(b"SDL_GetWindowSizeInPixels\0");
    Api {
        poll,
        properties,
        sdl3,
        swap: symbol(b"SDL_GL_SwapWindow\0"),
        destroy: symbol(b"SDL_DestroyWindow\0"),
        logical,
        logical_sdl3: provider_sdl3(logical, properties),
        pixels,
        pixels_sdl3: provider_sdl3(pixels, properties),
        size: symbol(if sdl3 {
            b"SDL_GetWindowSizeInPixels\0"
        } else {
            b"SDL_GL_GetDrawableSize\0"
        }),
        current: symbol(b"SDL_GL_GetCurrentWindow\0"),
        context: symbol(b"SDL_GL_GetCurrentContext\0"),
        proc: symbol(b"SDL_GL_GetProcAddress\0"),
        focus: symbol(b"SDL_GetKeyboardFocus\0"),
        from_id: symbol(b"SDL_GetWindowFromID\0"),
        mod_state: symbol(b"SDL_GetModState\0"),
        keyboard: symbol(b"SDL_GetKeyboardState\0"),
        mouse: symbol(b"SDL_GetMouseState\0"),
        relative: symbol(b"SDL_GetRelativeMouseState\0"),
        global: symbol(b"SDL_GetGlobalMouseState\0"),
        wm: symbol(b"SDL_GetWindowWMInfo\0"),
        version: symbol(b"SDL_GetVersion\0"),
        pointer: symbol(b"SDL_GetPointerProperty\0"),
        number: symbol(b"SDL_GetNumberProperty\0"),
        display: symbol(if sdl3 {
            b"SDL_GetDisplayForWindow\0"
        } else {
            b"SDL_GetWindowDisplayIndex\0"
        }),
        mode: symbol(b"SDL_GetCurrentDisplayMode\0"),
    }
});
unsafe fn function<T: Copy>(p: usize) -> Option<T> {
    if p == 0 {
        None
    } else {
        Some(unsafe { mem::transmute_copy(&p) })
    }
}
pub(crate) fn current_window() -> usize {
    if !SDL_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return 0;
    }
    unsafe {
        function::<GetCurrentWindow>(API.current)
            .map(|f| f() as usize)
            .unwrap_or(0)
    }
}
pub(crate) fn focused(window: usize) -> bool {
    window != 0
        && unsafe {
            function::<GetCurrentWindow>(API.focus)
                .map(|f| f() as usize == window)
                .unwrap_or(false)
        }
}
pub(crate) unsafe fn current_context() -> usize {
    unsafe {
        function::<GetCurrentWindow>(API.context)
            .map(|f| f() as usize)
            .unwrap_or(0)
    }
}
pub(crate) unsafe fn gl_proc(name: &[u8]) -> *const c_void {
    unsafe {
        function::<unsafe extern "C" fn(*const c_char) -> *const c_void>(API.proc)
            .map(|f| f(name.as_ptr().cast()))
            .unwrap_or(std::ptr::null())
    }
}
pub(crate) fn native_window(window: usize) -> Option<u64> {
    if window == 0 {
        return None;
    }
    unsafe {
        if API.sdl3 {
            let get = function::<unsafe extern "C" fn(*mut c_void) -> u32>(API.properties)?;
            let pointer = function::<
                unsafe extern "C" fn(u32, *const c_char, *mut c_void) -> *mut c_void,
            >(API.pointer)?;
            let number =
                function::<unsafe extern "C" fn(u32, *const c_char, i64) -> i64>(API.number)?;
            let props = get(window as _);
            if props == 0 {
                return None;
            }
            let surface = pointer(
                props,
                c"SDL.window.wayland.surface".as_ptr(),
                std::ptr::null_mut(),
            );
            if !surface.is_null() {
                return Some(surface as u64);
            }
            let x = number(props, c"SDL.window.x11.window".as_ptr(), 0);
            return (x != 0).then_some(x as u64);
        }
        let get = function::<GetWindowWmInfo>(API.wm)?;
        let version = function::<GetVersion>(API.version)?;
        let mut info: WmInfo = mem::zeroed();
        version(&mut info.version);
        if get(window as _, &mut info) == 0 {
            return None;
        }
        matches!(info.subsystem, SYSWM_X11 | SYSWM_WAYLAND)
            .then_some(info.information[1] as u64)
            .filter(|w| *w != 0)
    }
}
#[repr(C)]
struct DisplayMode2 {
    format: u32,
    w: i32,
    h: i32,
    refresh: i32,
    driver: *mut c_void,
}
#[repr(C)]
struct DisplayMode3 {
    display: u32,
    format: u32,
    w: i32,
    h: i32,
    density: f32,
    refresh: f32,
    numerator: i32,
    denominator: i32,
    internal: *mut c_void,
}
pub(crate) fn refresh_hz(window: usize) -> f64 {
    unsafe {
        let Some(display) = function::<unsafe extern "C" fn(*mut c_void) -> u32>(API.display)
        else {
            return 0.0;
        };
        if window == 0 {
            return 0.0;
        }
        let display = display(window as _);
        if API.sdl3 {
            let Some(mode) = function::<unsafe extern "C" fn(u32) -> *const DisplayMode3>(API.mode)
            else {
                return 0.0;
            };
            let p = mode(display);
            if p.is_null() {
                0.0
            } else {
                (*p).refresh as f64
            }
        } else {
            let Some(mode) =
                function::<unsafe extern "C" fn(i32, *mut DisplayMode2) -> i32>(API.mode)
            else {
                return 0.0;
            };
            let mut m: DisplayMode2 = mem::zeroed();
            if mode(display as i32, &mut m) == 0 {
                m.refresh.max(0) as f64
            } else {
                0.0
            }
        }
    }
}
pub(crate) fn native_refresh_hz(surface: u64) -> f64 {
    let w = current_window();
    if native_window(w) == Some(surface) {
        refresh_hz(w)
    } else {
        0.0
    }
}
pub(crate) fn window_identity(id: u32) -> Option<usize> {
    let w = unsafe {
        function::<GetWindowFromId>(API.from_id)
            .map(|f| f(id) as usize)
            .filter(|w| *w != 0)
    };
    if let Some(w) = w {
        remember_window(w);
    }
    w
}
pub(crate) fn modifiers(value: u16) -> InputModifiers {
    InputModifiers {
        shift: value & KMOD_SHIFT != 0,
        control: value & KMOD_CTRL != 0,
        alt: value & KMOD_ALT != 0,
        meta: value & KMOD_GUI != 0,
        caps_lock: value & KMOD_CAPS != 0,
        num_lock: value & KMOD_NUM != 0,
        ..Default::default()
    }
}
pub(crate) fn current_modifiers() -> InputModifiers {
    unsafe {
        modifiers(
            function::<unsafe extern "C" fn() -> u16>(API.mod_state)
                .map(|f| f())
                .unwrap_or(0),
        )
    }
}
pub(crate) fn send(window: usize, input: BrowserInput) -> bool {
    if let Some(result) = hooks::route(window, input) {
        return result;
    }
    native_window(window)
        .and_then(|surface| crate::vulkan::route_surface_input(surface, input))
        .unwrap_or(false)
}
pub(crate) fn capture(window: usize) -> bool {
    let capturing = focused(window)
        && (hooks::capturing(window)
            || native_window(window)
                .and_then(crate::vulkan::capturing_surface)
                .unwrap_or(false));
    sync_text_capture(window, capturing);
    capturing
}
pub(crate) fn set_focus(window: usize, value: bool) {
    hooks::focus(window, value);
    if let Some(surface) = native_window(window) {
        crate::vulkan::focus(surface, value);
    }
}
pub(crate) fn key(window: usize, vk: i32, native: i32, mods: InputModifiers, down: bool) -> bool {
    let recipients = key_recipients(window, native);
    // The host tracks shortcut press/release pairs even when the game owns
    // the key-up. Recipient ownership controls capture, not observation.
    let consumed = send(
        window,
        BrowserInput::Key {
            kind: if down { KeyKind::RawDown } else { KeyKind::Up },
            modifiers: mods,
            windows_key_code: vk,
            native_key_code: native,
            is_system_key: mods.alt,
            character: 0,
            unmodified_character: 0,
        },
    );
    if down && consumed {
        set_key_recipient(window, native, KeyRecipient::Overlay, true);
    } else if !down && recipients.overlay_down {
        set_key_recipient(window, native, KeyRecipient::Overlay, false);
    }
    key_event_consumed(consumed, down, recipients)
}
pub(crate) fn text(window: usize, text: &str) -> bool {
    let mut swallowed = capture(window);
    for character in text.encode_utf16() {
        swallowed &= send(
            window,
            BrowserInput::Key {
                kind: KeyKind::Character,
                modifiers: current_modifiers(),
                windows_key_code: character as i32,
                native_key_code: 0,
                is_system_key: false,
                character,
                unmodified_character: character,
            },
        );
    }
    swallowed
}
pub(crate) fn motion(window: usize, x: i32, y: i32, state: u32) -> bool {
    let (x, y) = pixel_coordinates(window, x, y);
    let mut m = current_modifiers();
    m.left_button = state & 1 != 0;
    m.middle_button = state & 2 != 0;
    m.right_button = state & 4 != 0;
    send(
        window,
        BrowserInput::Move {
            x,
            y,
            modifiers: m,
            leave: false,
        },
    )
}
pub(crate) fn button(window: usize, x: i32, y: i32, b: u8, down: bool, clicks: u8) -> bool {
    let (x, y) = pixel_coordinates(window, x, y);
    let button = match b {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        _ => return capture(window),
    };
    let mut modifiers = pointer_modifiers();
    match button {
        MouseButton::Left => modifiers.left_button = down,
        MouseButton::Middle => modifiers.middle_button = down,
        MouseButton::Right => modifiers.right_button = down,
    };
    send(
        window,
        BrowserInput::Button {
            x,
            y,
            modifiers,
            button,
            up: !down,
            click_count: clicks.max(1) as i32,
        },
    )
}
pub(crate) fn wheel(window: usize, x: i32, y: i32, dx: i32, dy: i32) -> bool {
    let (x, y) = pixel_coordinates(window, x, y);
    send(
        window,
        BrowserInput::Wheel {
            x,
            y,
            modifiers: pointer_modifiers(),
            delta_x: dx,
            delta_y: dy,
        },
    )
}
unsafe fn route_event(event: *mut c_void) -> bool {
    let kind = unsafe { *event.cast::<u32>() };
    if !matches!(
        kind,
        SDL_WINDOWEVENT
            | SDL_KEYDOWN
            | SDL_KEYUP
            | SDL_TEXTINPUT
            | SDL_MOUSEMOTION
            | SDL_MOUSEBUTTONDOWN
            | SDL_MOUSEBUTTONUP
            | SDL_MOUSEWHEEL
    ) {
        return false;
    }
    let id = unsafe { *event.cast::<u8>().add(8).cast::<u32>() };
    let Some(window) = window_identity(id) else {
        return false;
    };
    match kind {
        SDL_KEYDOWN | SDL_KEYUP => {
            let e = unsafe { &*event.cast::<KeyboardEvent>() };
            let mut m = modifiers(e.keysym.modifiers);
            m.keypad = (83..=99).contains(&e.keysym.scancode);
            m.left_key = (224..=227).contains(&e.keysym.scancode);
            m.right_key = (228..=231).contains(&e.keysym.scancode);
            if let Some(vk) = keycode_to_vk(e.keysym.keycode) {
                key(window, vk as i32, e.keysym.scancode, m, kind == SDL_KEYDOWN)
            } else {
                capture(window)
            }
        }
        SDL_TEXTINPUT => {
            let e = unsafe { &*event.cast::<TextEvent>() };
            let n = e.text.iter().position(|c| *c == 0).unwrap_or(32);
            let bytes = unsafe { std::slice::from_raw_parts(e.text.as_ptr().cast::<u8>(), n) };
            std::str::from_utf8(bytes)
                .map(|s| text(window, s))
                .unwrap_or(false)
        }
        SDL_MOUSEMOTION => {
            let e = unsafe { &mut *event.cast::<MotionEvent>() };
            let captured = motion(window, e.x, e.y, e.state);
            let (dx, dy) =
                resume_mouse_motion(window, captured, e.x_relative as f64, e.y_relative as f64);
            e.x_relative = dx as i32;
            e.y_relative = dy as i32;
            captured
        }
        SDL_MOUSEBUTTONDOWN | SDL_MOUSEBUTTONUP => {
            let e = unsafe { &*event.cast::<ButtonEvent>() };
            button(
                window,
                e.x,
                e.y,
                e.button,
                kind == SDL_MOUSEBUTTONDOWN,
                e.clicks,
            )
        }
        SDL_MOUSEWHEEL => {
            let e = unsafe { &*event.cast::<WheelEvent>() };
            let sign = if e.direction == 1 { -120 } else { 120 };
            let (x, y) = mouse_position();
            wheel(
                window,
                x,
                y,
                e.x.saturating_mul(sign),
                e.y.saturating_mul(sign),
            )
        }
        SDL_WINDOWEVENT => {
            let e = unsafe { &*event.cast::<WindowEvent>() };
            match e.event {
                WINDOW_FOCUS_GAINED => set_focus(window, true),
                WINDOW_FOCUS_LOST => set_focus(window, false),
                WINDOW_LEAVE => {
                    send(
                        window,
                        BrowserInput::Move {
                            x: 0,
                            y: 0,
                            modifiers: current_modifiers(),
                            leave: true,
                        },
                    );
                }
                _ => {}
            }
            false
        }
        _ => false,
    }
}
unsafe fn queued_event_captured(event: *mut c_void, sdl3: bool, consumed: bool) -> bool {
    // The permanent filter already forwarded accepted events. Forwarding their
    // queued copies again turns a complete key chord into two edit toggles.
    if consumed && !FILTER_INSTALLED.load(std::sync::atomic::Ordering::Acquire) {
        let routed = if sdl3 {
            unsafe { sdl3::route_event(event) }
        } else {
            unsafe { route_event(event) }
        };
        if let Some((window, native, down)) = unsafe { queued_key_event(event, sdl3) }
            && !routed
        {
            note_queued_key_delivery(window, native, down);
        }
        return routed;
    }
    let kind = unsafe { *event.cast::<u32>() };
    if matches!(
        kind,
        SDL_KEYDOWN
            | SDL_KEYUP
            | SDL_TEXTINPUT
            | SDL_MOUSEMOTION
            | SDL_MOUSEBUTTONDOWN
            | SDL_MOUSEBUTTONUP
            | SDL_MOUSEWHEEL
    ) {
        let offset = if sdl3 { 16 } else { 8 };
        let id = unsafe { *event.cast::<u8>().add(offset).cast::<u32>() };
        let Some(window) = window_identity(id) else {
            return false;
        };
        if let Some((window, native, down)) = unsafe { queued_key_event(event, sdl3) } {
            let recipients = key_recipients(window, native);
            let swallowed = key_event_consumed(capture(window), down, recipients);
            if swallowed && down {
                set_key_recipient(window, native, KeyRecipient::Overlay, true);
            } else if !swallowed {
                note_queued_key_delivery(window, native, down);
            }
            swallowed
        } else {
            capture(window)
        }
    } else {
        false
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_PollEvent(event: *mut c_void) -> c_int {
    activate();
    let Some(_guard) = hooks::Guard::enter() else {
        return unsafe { poll_real(event) };
    };
    capture(unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    });
    for _ in 0..SUPPRESSION_BUDGET {
        let result = unsafe { poll_real(event) };
        if result <= 0 || event.is_null() {
            return result;
        }
        let swallowed = unsafe { queued_event_captured(event, API.sdl3, true) };
        if !swallowed {
            return result;
        }
    }
    0
}
unsafe fn poll_real(event: *mut c_void) -> c_int {
    if API.sdl3 {
        unsafe {
            function::<unsafe extern "C" fn(*mut c_void) -> bool>(API.poll)
                .map(|f| i32::from(f(event)))
                .unwrap_or(0)
        }
    } else {
        unsafe { function::<Poll>(API.poll).map(|f| f(event)).unwrap_or(0) }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GL_SwapWindow(window: *mut c_void) -> bool {
    activate();
    remember_window(window as usize);
    if let Some(_guard) = hooks::Guard::enter() {
        let (mut w, mut h) = (0, 0);
        unsafe {
            if API.sdl3 {
                if let Some(f) = function::<
                    unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> bool,
                >(API.size)
                {
                    f(window, &mut w, &mut h);
                }
            } else if let Some(f) =
                function::<unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32)>(API.size)
            {
                f(window, &mut w, &mut h);
            }
        }
        hooks::present_sdl(
            window as usize,
            w.max(0) as u32,
            h.max(0) as u32,
            focused(window as usize),
        );
        capture(window as usize);
        unsafe { swap_real(window) }
    } else {
        unsafe { swap_real(window) }
    }
}
unsafe fn swap_real(window: *mut c_void) -> bool {
    if API.sdl3 {
        unsafe {
            function::<unsafe extern "C" fn(*mut c_void) -> bool>(API.swap)
                .map(|f| f(window))
                .unwrap_or(false)
        }
    } else {
        unsafe {
            if let Some(f) = function::<unsafe extern "C" fn(*mut c_void)>(API.swap) {
                f(window);
                true
            } else {
                false
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_DestroyWindow(window: *mut c_void) {
    activate();
    sync_text_capture(window as usize, false);
    clear_window_key_state(window as usize);
    if let Some(state) = CAPTURED_MOTION
        .lock()
        .iter_mut()
        .find(|state| state.window == window as usize)
    {
        *state = CapturedMotion::EMPTY;
    }
    if let Some(mut windows) = WINDOWS.try_lock()
        && let Some(slot) = windows.iter_mut().find(|w| **w == window as usize)
    {
        *slot = 0;
    }
    hooks::remove(hooks::Backend::Sdl, 0, window as usize);
    unsafe {
        if let Some(f) = function::<unsafe extern "C" fn(*mut c_void)>(API.destroy) {
            f(window);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GL_GetProcAddress(name: *const c_char) -> *const c_void {
    activate();
    if name.is_null() {
        return std::ptr::null();
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name) };
    let provider = unsafe { gl_proc(name.to_bytes_with_nul()) };
    if provider.is_null() {
        provider
    } else {
        hooks::hooked(name.to_bytes()).unwrap_or(provider)
    }
}
static ZERO_KEYS: [u8; 512] = [0; 512];
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GetKeyboardState(count: *mut c_int) -> *const u8 {
    activate();
    let Some(f) =
        (unsafe { function::<unsafe extern "C" fn(*mut c_int) -> *const u8>(API.keyboard) })
    else {
        return std::ptr::null();
    };
    if !capture(unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    }) {
        return unsafe { f(count) };
    }
    let mut n = 0;
    unsafe {
        f(&mut n);
    }
    if n < 0 || n as usize > ZERO_KEYS.len() {
        return unsafe { f(count) };
    }
    if !count.is_null() {
        unsafe {
            *count = n;
        }
    }
    ZERO_KEYS.as_ptr()
}
fn capture_focus() -> bool {
    let w = unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    };
    capture(w)
}
unsafe fn mouse_call(p: usize, x: *mut c_void, y: *mut c_void, hide: bool) -> u32 {
    if API.sdl3 {
        let Some(f) = (unsafe { function::<unsafe extern "C" fn(*mut f32, *mut f32) -> u32>(p) })
        else {
            return 0;
        };
        let state = unsafe { f(x.cast(), y.cast()) };
        if hide {
            unsafe {
                if !x.is_null() {
                    *x.cast::<f32>() = 0.0;
                }
                if !y.is_null() {
                    *y.cast::<f32>() = 0.0;
                }
            }
            0
        } else {
            state
        }
    } else {
        let Some(f) = (unsafe { function::<unsafe extern "C" fn(*mut i32, *mut i32) -> u32>(p) })
        else {
            return 0;
        };
        let state = unsafe { f(x.cast(), y.cast()) };
        if hide {
            unsafe {
                if !x.is_null() {
                    *x.cast::<i32>() = 0;
                }
                if !y.is_null() {
                    *y.cast::<i32>() = 0;
                }
            }
            0
        } else {
            state
        }
    }
}
fn mouse_position() -> (i32, i32) {
    unsafe {
        if API.sdl3 {
            let (mut x, mut y) = (0.0f32, 0.0f32);
            mouse_call(
                API.mouse,
                (&mut x as *mut f32).cast(),
                (&mut y as *mut f32).cast(),
                false,
            );
            (x.round() as i32, y.round() as i32)
        } else {
            let (mut x, mut y) = (0i32, 0i32);
            mouse_call(
                API.mouse,
                (&mut x as *mut i32).cast(),
                (&mut y as *mut i32).cast(),
                false,
            );
            (x, y)
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GetMouseState(x: *mut c_void, y: *mut c_void) -> u32 {
    activate();
    unsafe { mouse_call(API.mouse, x, y, capture_focus()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GetRelativeMouseState(x: *mut c_void, y: *mut c_void) -> u32 {
    activate();
    unsafe { mouse_call(API.relative, x, y, capture_focus()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GetGlobalMouseState(x: *mut c_void, y: *mut c_void) -> u32 {
    activate();
    unsafe { mouse_call(API.global, x, y, capture_focus()) }
}
pub(crate) fn hooked(name: &[u8]) -> Option<*const c_void> {
    Some(match name {
        b"SDL_GL_SwapWindow" => SDL_GL_SwapWindow as _,
        b"SDL_PollEvent" => SDL_PollEvent as _,
        b"SDL_PeepEvents" => SDL_PeepEvents as _,
        b"SDL_SetEventFilter" => SDL_SetEventFilter as _,
        b"SDL_GetEventFilter" => SDL_GetEventFilter as _,
        b"SDL_CreateWindow" => SDL_CreateWindow as _,
        b"SDL_DestroyWindow" => SDL_DestroyWindow as _,
        b"SDL_GL_GetProcAddress" => SDL_GL_GetProcAddress as _,
        b"SDL_GL_DeleteContext" => SDL_GL_DeleteContext as _,
        b"SDL_GL_DestroyContext" => SDL_GL_DestroyContext as _,
        b"SDL_GetKeyboardState" => SDL_GetKeyboardState as _,
        b"SDL_GetMouseState" => SDL_GetMouseState as _,
        b"SDL_GetRelativeMouseState" => SDL_GetRelativeMouseState as _,
        b"SDL_GetGlobalMouseState" => SDL_GetGlobalMouseState as _,
        _ => return None,
    })
}
type Filter2 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type Filter3 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool;
static FILTER_STATE: LazyLock<parking_lot::Mutex<(usize, usize)>> = LazyLock::new(|| {
    let mut callback = std::ptr::null_mut();
    let mut userdata = std::ptr::null_mut();
    // Adopt an app filter installed before live injection as well as preload.
    let getter = symbol(b"SDL_GetEventFilter\0");
    unsafe {
        if *SET_EVENT_FILTER_SDL3 {
            if let Some(get) =
                function::<unsafe extern "C" fn(*mut *mut c_void, *mut *mut c_void) -> bool>(getter)
            {
                get(&mut callback, &mut userdata);
            }
        } else if let Some(get) =
            function::<unsafe extern "C" fn(*mut *mut c_void, *mut *mut c_void) -> c_int>(getter)
        {
            get(&mut callback, &mut userdata);
        }
    }
    parking_lot::Mutex::new((callback as usize, userdata as usize))
});
static SET_EVENT_FILTER: LazyLock<usize> = LazyLock::new(|| symbol(b"SDL_SetEventFilter\0"));
static SET_EVENT_FILTER_SDL3: LazyLock<bool> =
    LazyLock::new(|| provider_sdl3(*SET_EVENT_FILTER, API.properties));
static FILTER_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn event_filter2(_userdata: *mut c_void, event: *mut c_void) -> c_int {
    let (filter, userdata) = *FILTER_STATE.lock();
    if !event.is_null() && unsafe { route_event(event) } {
        return 0;
    }
    let accepted = if filter == 0 {
        true
    } else {
        unsafe { function::<Filter2>(filter).unwrap()(userdata as *mut c_void, event) != 0 }
    };
    note_filtered_key(event, false, accepted);
    if accepted { 1 } else { 0 }
}

unsafe extern "C" fn event_filter3(_userdata: *mut c_void, event: *mut c_void) -> bool {
    let (filter, userdata) = *FILTER_STATE.lock();
    if !event.is_null() && unsafe { sdl3::route_event(event) } {
        return false;
    }
    let accepted = if filter == 0 {
        true
    } else {
        unsafe { function::<Filter3>(filter).unwrap()(userdata as *mut c_void, event) }
    };
    note_filtered_key(event, true, accepted);
    accepted
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_SetEventFilter(filter: *mut c_void, userdata: *mut c_void) {
    activate();
    let filter = filter as usize;
    *FILTER_STATE.lock() = (filter, userdata as usize);
    install_event_filter();
}

fn install_event_filter() {
    // A single permanent proxy keeps overlay input interception alive when the
    // app removes its own filter. Concurrent/reentrant setters all install the
    // same callback; fn+userdata is one atomic snapshot, never mixed generations.
    LazyLock::force(&FILTER_STATE);
    unsafe {
        if let Some(set) =
            function::<unsafe extern "C" fn(*mut c_void, *mut c_void)>(*SET_EVENT_FILTER)
        {
            let callback = if *SET_EVENT_FILTER_SDL3 {
                event_filter3 as *const () as *mut c_void
            } else {
                event_filter2 as *const () as *mut c_void
            };
            set(callback, std::ptr::null_mut());
            FILTER_INSTALLED.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GetEventFilter(
    filter: *mut *mut c_void,
    userdata: *mut *mut c_void,
) -> usize {
    activate();
    let (callback, data) = *FILTER_STATE.lock();
    if !filter.is_null() {
        unsafe {
            *filter = callback as *mut c_void;
        }
    }
    if !userdata.is_null() {
        unsafe {
            *userdata = data as *mut c_void;
        }
    }
    usize::from(callback != 0)
}
static WINDOWS: LazyLock<parking_lot::Mutex<[usize; 16]>> =
    LazyLock::new(|| parking_lot::Mutex::new([0; 16]));
fn remember_window(window: usize) {
    if window == 0 {
        return;
    }
    if let Some(mut windows) = WINDOWS.try_lock()
        && !windows.contains(&window)
        && let Some(slot) = windows.iter_mut().find(|w| **w == 0)
    {
        *slot = window;
    }
}
pub(crate) fn surface_status(native: u64) -> Option<(bool, f64)> {
    if !SDL_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let focus = unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    };
    if native_window(focus) == Some(native) {
        remember_window(focus);
        return Some((true, refresh_hz(focus)));
    }
    let windows = WINDOWS.try_lock()?;
    let window = windows
        .iter()
        .copied()
        .find(|w| native_window(*w) == Some(native))?;
    Some((false, refresh_hz(window)))
}
static CREATE_WINDOW: LazyLock<usize> = LazyLock::new(|| symbol(b"SDL_CreateWindow\0"));
// SDL2 and SDL3 share this symbol but not its prototype. The six ABI argument
// registers are forwarded to the correctly typed provider, not reinterpreted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_CreateWindow(
    title: *const c_char,
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    e: usize,
) -> *mut c_void {
    activate();
    let window = unsafe {
        if API.sdl3 {
            function::<unsafe extern "C" fn(*const c_char, i32, i32, u64) -> *mut c_void>(
                *CREATE_WINDOW,
            )
            .map(|f| f(title, a as i32, b as i32, c as u64))
        } else {
            function::<unsafe extern "C" fn(*const c_char, i32, i32, i32, i32, u32) -> *mut c_void>(
                *CREATE_WINDOW,
            )
            .map(|f| f(title, a as i32, b as i32, c as i32, d as i32, e as u32))
        }
    }
    .unwrap_or(std::ptr::null_mut());
    remember_window(window as usize);
    window
}
static DELETE_CONTEXT: LazyLock<usize> = LazyLock::new(|| symbol(b"SDL_GL_DeleteContext\0"));
static DESTROY_CONTEXT: LazyLock<usize> = LazyLock::new(|| symbol(b"SDL_GL_DestroyContext\0"));
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GL_DeleteContext(context: *mut c_void) {
    activate();
    hooks::destroy_context(hooks::Backend::Sdl, context as usize);
    unsafe {
        if let Some(f) = function::<unsafe extern "C" fn(*mut c_void)>(*DELETE_CONTEXT) {
            f(context);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GL_DestroyContext(context: *mut c_void) -> bool {
    activate();
    hooks::destroy_context(hooks::Backend::Sdl, context as usize);
    unsafe {
        function::<unsafe extern "C" fn(*mut c_void) -> bool>(*DESTROY_CONTEXT)
            .map(|f| f(context))
            .unwrap_or(false)
    }
}
fn provider_sdl3(function: usize, properties: usize) -> bool {
    if function == 0 || properties == 0 {
        return false;
    }
    let mut a: libc::Dl_info = unsafe { mem::zeroed() };
    let mut b: libc::Dl_info = unsafe { mem::zeroed() };
    unsafe {
        libc::dladdr(function as _, &mut a) != 0
            && libc::dladdr(properties as _, &mut b) != 0
            && a.dli_fbase == b.dli_fbase
    }
}
fn pixel_coordinates(window: usize, x: i32, y: i32) -> (i32, i32) {
    let (mut lw, mut lh, mut pw, mut ph) = (0, 0, 0, 0);
    unsafe {
        if API.logical_sdl3 {
            if let Some(f) = function::<unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> bool>(
                API.logical,
            ) {
                f(window as _, &mut lw, &mut lh);
            }
        } else if let Some(f) =
            function::<unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32)>(API.logical)
        {
            f(window as _, &mut lw, &mut lh);
        }
        if API.pixels != 0 && API.pixels_sdl3 == API.sdl3 {
            if API.pixels_sdl3 {
                if let Some(f) = function::<
                    unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> bool,
                >(API.pixels)
                {
                    f(window as _, &mut pw, &mut ph);
                }
            } else if let Some(f) =
                function::<unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32)>(API.pixels)
            {
                f(window as _, &mut pw, &mut ph);
            }
        } else if !API.sdl3
            && let Some(f) =
                function::<unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32)>(API.size)
        {
            f(window as _, &mut pw, &mut ph);
        }
    }
    if lw <= 0 || lh <= 0 || pw <= 0 || ph <= 0 {
        return (x, y);
    }
    (
        ((x as i64 * pw as i64) / lw as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        ((y as i64 * ph as i64) / lh as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
    )
}
type Peep = unsafe extern "C" fn(*mut c_void, c_int, c_int, u32, u32) -> c_int;
static PEEP: LazyLock<usize> = LazyLock::new(|| symbol(b"SDL_PeepEvents\0"));
static PEEP_SDL3: LazyLock<bool> = LazyLock::new(|| provider_sdl3(*PEEP, API.properties));
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_PeepEvents(
    events: *mut c_void,
    count: c_int,
    action: c_int,
    min: u32,
    max: u32,
) -> c_int {
    activate();
    let Some(real) = (unsafe { function::<Peep>(*PEEP) }) else {
        return -1;
    };
    let Some(_guard) = hooks::Guard::enter() else {
        return unsafe { real(events, count, action, min, max) };
    };
    capture(unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    });
    if events.is_null() || count <= 0 || action == 0 {
        return unsafe { real(events, count, action, min, max) };
    }
    let result = unsafe {
        real(
            events,
            count.min(SUPPRESSION_BUDGET as i32),
            action,
            min,
            max,
        )
    };
    if result <= 0 {
        return result;
    }
    let stride = if *PEEP_SDL3 { 128 } else { 56 };
    let mut kept = 0;
    for index in 0..result as usize {
        let event = unsafe { events.cast::<u8>().add(index * stride) };
        let swallowed = unsafe { queued_event_captured(event.cast(), *PEEP_SDL3, action == 2) };
        if !swallowed {
            if kept != index {
                unsafe {
                    std::ptr::copy(event, events.cast::<u8>().add(kept * stride), stride);
                }
            }
            kept += 1;
        }
    }
    kept as i32
}
fn pointer_modifiers() -> InputModifiers {
    let state = unsafe { mouse_call(API.mouse, std::ptr::null_mut(), std::ptr::null_mut(), false) };
    let mut modifiers = current_modifiers();
    modifiers.left_button = state & 1 != 0;
    modifiers.middle_button = state & 2 != 0;
    modifiers.right_button = state & 4 != 0;
    modifiers
}
#[derive(Clone, Copy)]
struct TextCapture {
    window: usize,
    owned: bool,
}
static TEXT_CAPTURES: LazyLock<parking_lot::Mutex<[Option<TextCapture>; 16]>> =
    LazyLock::new(|| parking_lot::Mutex::new([None; 16]));
static TEXT_API: LazyLock<[usize; 3]> = LazyLock::new(|| {
    [
        symbol(b"SDL_IsTextInputActive\0"),
        symbol(b"SDL_StartTextInput\0"),
        symbol(b"SDL_StopTextInput\0"),
    ]
});
fn sync_text_capture(window: usize, capturing: bool) {
    let Some(mut captures) = TEXT_CAPTURES.try_lock() else {
        return;
    };
    let focus = unsafe {
        function::<GetCurrentWindow>(API.focus)
            .map(|f| f() as usize)
            .unwrap_or(0)
    };
    for capture in captures.iter_mut() {
        if capture
            .is_some_and(|state| state.window != focus || (!capturing && state.window == window))
            && let Some(state) = capture.take()
            && state.owned
        {
            stop_owned_text(state.window);
        }
    }
    if window == 0 || !capturing {
        return;
    }
    let Some(index) = captures
        .iter()
        .position(|v| v.is_some_and(|v| v.window == window))
        .or_else(|| captures.iter().position(Option::is_none))
    else {
        return;
    };
    let active = unsafe {
        if API.sdl3 {
            function::<unsafe extern "C" fn(*mut c_void) -> bool>(TEXT_API[0])
                .map(|f| f(window as _))
        } else {
            function::<unsafe extern "C" fn() -> c_int>(TEXT_API[0]).map(|f| f() != 0)
        }
    };
    if active != Some(false) {
        return;
    }
    let started = unsafe {
        if API.sdl3 {
            function::<unsafe extern "C" fn(*mut c_void) -> bool>(TEXT_API[1])
                .map(|f| f(window as _))
                .unwrap_or(false)
        } else {
            if let Some(start) = function::<unsafe extern "C" fn()>(TEXT_API[1]) {
                start();
                true
            } else {
                false
            }
        }
    };
    if started {
        captures[index] = Some(TextCapture {
            window,
            owned: true,
        });
    }
}
fn stop_owned_text(window: usize) {
    unsafe {
        if API.sdl3 {
            if let Some(stop) = function::<unsafe extern "C" fn(*mut c_void) -> bool>(TEXT_API[2]) {
                stop(window as _);
            }
        } else if let Some(stop) = function::<unsafe extern "C" fn()>(TEXT_API[2]) {
            stop();
        }
    }
}
// Native GL/Vulkan may run before SDL is dynamically loaded. Do not freeze an
// empty SDL dispatch table merely while checking native presentation focus.
static SDL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn activate() {
    use std::sync::atomic::Ordering;
    if SDL_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        install_event_filter();
    }
}

#[cfg(test)]
mod key_state_tests {
    use super::{KeyRecipients, WindowKeyState, key_event_consumed};

    #[test]
    fn key_up_follows_the_key_down_recipient() {
        let mut game_down = WindowKeyState::EMPTY;
        WindowKeyState::set(&mut game_down.game_down, 42, true);
        assert!(!key_event_consumed(
            true,
            false,
            KeyRecipients {
                game_down: game_down.has(&game_down.game_down, 42),
                overlay_down: false,
                release_pending: false,
            },
        ));

        let mut release_pending = WindowKeyState::EMPTY;
        WindowKeyState::set(&mut release_pending.release_pending, 42, true);
        assert!(!key_event_consumed(
            true,
            false,
            KeyRecipients {
                game_down: false,
                overlay_down: false,
                release_pending: release_pending.has(&release_pending.release_pending, 42),
            },
        ));

        let mut overlay_down = WindowKeyState::EMPTY;
        WindowKeyState::set(&mut overlay_down.overlay_down, 42, true);
        assert!(key_event_consumed(
            false,
            false,
            KeyRecipients {
                game_down: false,
                overlay_down: overlay_down.has(&overlay_down.overlay_down, 42),
                release_pending: false,
            },
        ));
    }
}

#[cfg(test)]
mod motion_state_tests {
    use super::{CapturedMotion, restore_relative_motion};

    #[test]
    fn relative_cursor_rejoins_absolute_cursor_after_editor_drag() {
        let mut states = [CapturedMotion::EMPTY; 2];
        restore_relative_motion(&mut states, 1, true, -18.0, -222.0);
        restore_relative_motion(&mut states, 1, true, 60.0, 40.0);
        restore_relative_motion(&mut states, 2, true, 0.5, -0.25);
        let (dx, dy) = restore_relative_motion(&mut states, 1, false, 1.0, 1.0);
        assert_eq!((368.0 + dx, 322.0 + dy), (411.0, 141.0));
        assert_eq!(
            restore_relative_motion(&mut states, 1, false, 267.0, 233.0),
            (267.0, 233.0)
        );
        assert_eq!(
            restore_relative_motion(&mut states, 2, false, 0.25, 1.0),
            (0.75, 0.75)
        );
    }
}
