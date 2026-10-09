#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
    pub keypad: bool,
    pub left_key: bool,
    pub right_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    RawDown,
    Up,
    Character,
}

/// Browser-independent editor input. Coordinates are browser view pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserInput {
    Move {
        x: i32,
        y: i32,
        modifiers: InputModifiers,
        leave: bool,
    },
    Button {
        x: i32,
        y: i32,
        modifiers: InputModifiers,
        button: MouseButton,
        up: bool,
        click_count: i32,
    },
    Wheel {
        x: i32,
        y: i32,
        modifiers: InputModifiers,
        delta_x: i32,
        delta_y: i32,
    },
    Key {
        kind: KeyKind,
        modifiers: InputModifiers,
        windows_key_code: i32,
        native_key_code: i32,
        is_system_key: bool,
        character: u16,
        unmodified_character: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub refresh_hz: f64,
}

#[derive(Debug)]
pub enum PlatformEvent {
    Attached { pid: u32 },
    Detached,
    Viewport(Viewport),
    Input(BrowserInput),
    ToggleEdit,
    ShortcutToggleEdit,
    CaptureEnded,
    ToggleVisibility,
    Exit,
    Warning(String),
    Fatal(String),
}
