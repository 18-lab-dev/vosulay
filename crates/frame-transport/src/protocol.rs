//! Fixed-size local control records. Pixel traffic never passes through this socket.
use std::io;
use vosulay_frame::{BrowserInput, InputModifiers, KeyKind, MouseButton};

pub const PACKET_SIZE: usize = 128;
pub const FORMAT_BGRA: u32 = 1;
pub const MAX_DIMENSION: u32 = 16384;
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
const VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Hello = 1,
    Surface = 2,
    SurfaceGone = 3,
    Ring = 4,
    Visible = 5,
    Capture = 6,
    Input = 7,
    Detach = 8,
    Ping = 9,
    Focus = 10,
    Configure = 12,
    Reload = 13,
}

impl TryFrom<u16> for Kind {
    type Error = io::Error;
    fn try_from(value: u16) -> io::Result<Self> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::Surface,
            3 => Self::SurfaceGone,
            4 => Self::Ring,
            5 => Self::Visible,
            6 => Self::Capture,
            7 => Self::Input,
            8 => Self::Detach,
            9 => Self::Ping,
            10 => Self::Focus,
            12 => Self::Configure,
            13 => Self::Reload,
            _ => return Err(invalid("unknown control record")),
        })
    }
}

/// Ring carries exactly one descriptor: the sealed memfd.
/// Surface: physical width/height, value[0] native window, value[1] millihertz,
/// value[2] focused. Focus: code 0/1.
/// Visible/Capture: code 0/1. Configure: code FPS cap (0 = display refresh).
/// Hello: value[0] PID, authenticated independently through SO_PEERCRED.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub kind: Kind,
    pub session: u64,
    pub surface: u64,
    pub generation: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub flags: u32,
    pub code: u32,
    pub value: [u64; 8],
}

impl Packet {
    pub const fn new(kind: Kind) -> Self {
        Self {
            kind,
            session: 0,
            surface: 0,
            generation: 0,
            sequence: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            flags: 0,
            code: 0,
            value: [0; 8],
        }
    }
    pub fn encode(&self) -> [u8; PACKET_SIZE] {
        let mut bytes = [0; PACKET_SIZE];
        bytes[..4].copy_from_slice(b"VLAY");
        bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(self.kind as u16).to_le_bytes());
        for (i, value) in [self.session, self.surface, self.generation, self.sequence]
            .iter()
            .enumerate()
        {
            bytes[8 + i * 8..16 + i * 8].copy_from_slice(&value.to_le_bytes());
        }
        for (i, value) in [
            self.width,
            self.height,
            self.stride,
            self.format,
            self.flags,
            self.code,
        ]
        .iter()
        .enumerate()
        {
            bytes[40 + i * 4..44 + i * 4].copy_from_slice(&value.to_le_bytes());
        }
        for (i, value) in self.value.iter().enumerate() {
            bytes[64 + i * 8..72 + i * 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != PACKET_SIZE
            || &bytes[..4] != b"VLAY"
            || u16::from_le_bytes([bytes[4], bytes[5]]) != VERSION
        {
            return Err(invalid("invalid control record header"));
        }
        let u64_at =
            |n| u64::from_le_bytes(bytes[n..n + 8].try_into().expect("checked record size"));
        let u32_at =
            |n| u32::from_le_bytes(bytes[n..n + 4].try_into().expect("checked record size"));
        let mut value = [0; 8];
        for (i, entry) in value.iter_mut().enumerate() {
            *entry = u64_at(64 + i * 8);
        }
        let packet = Self {
            kind: Kind::try_from(u16::from_le_bytes([bytes[6], bytes[7]]))?,
            session: u64_at(8),
            surface: u64_at(16),
            generation: u64_at(24),
            sequence: u64_at(32),
            width: u32_at(40),
            height: u32_at(44),
            stride: u32_at(48),
            format: u32_at(52),
            flags: u32_at(56),
            code: u32_at(60),
            value,
        };
        if (packet.kind == Kind::Input && packet.flags & !0xfff != 0)
            || (packet.kind != Kind::Input && packet.flags != 0)
        {
            return Err(invalid("invalid control flags"));
        }
        Ok(packet)
    }
    pub fn input(surface: u64, input: BrowserInput) -> Self {
        let mut packet = Self::new(Kind::Input);
        packet.surface = surface;
        let modifiers = match input {
            BrowserInput::Move {
                x,
                y,
                modifiers,
                leave,
            } => {
                packet.code = 1;
                packet.value[..3].copy_from_slice(&[signed(x), signed(y), u64::from(leave)]);
                modifiers
            }
            BrowserInput::Button {
                x,
                y,
                modifiers,
                button,
                up,
                click_count,
            } => {
                packet.code = 2;
                let button = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                };
                packet.value[..5].copy_from_slice(&[
                    signed(x),
                    signed(y),
                    button,
                    u64::from(up),
                    signed(click_count),
                ]);
                modifiers
            }
            BrowserInput::Wheel {
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            } => {
                packet.code = 3;
                packet.value[..4].copy_from_slice(&[
                    signed(x),
                    signed(y),
                    signed(delta_x),
                    signed(delta_y),
                ]);
                modifiers
            }
            BrowserInput::Key {
                kind,
                modifiers,
                windows_key_code,
                native_key_code,
                is_system_key,
                character,
                unmodified_character,
            } => {
                packet.code = 4;
                let kind = match kind {
                    KeyKind::RawDown => 0,
                    KeyKind::Up => 1,
                    KeyKind::Character => 2,
                };
                packet.value[..6].copy_from_slice(&[
                    kind,
                    signed(windows_key_code),
                    signed(native_key_code),
                    u64::from(is_system_key),
                    u64::from(character),
                    u64::from(unmodified_character),
                ]);
                modifiers
            }
        };
        let flags = [
            modifiers.shift,
            modifiers.control,
            modifiers.alt,
            modifiers.caps_lock,
            modifiers.num_lock,
            modifiers.left_button,
            modifiers.middle_button,
            modifiers.right_button,
            modifiers.keypad,
            modifiers.left_key,
            modifiers.right_key,
            modifiers.meta,
        ];
        for (bit, value) in flags.iter().enumerate() {
            packet.flags |= u32::from(*value) << bit;
        }
        packet
    }
    pub fn browser_input(&self) -> io::Result<BrowserInput> {
        if self.kind != Kind::Input || self.flags & !0xfff != 0 {
            return Err(invalid("invalid input record"));
        }
        let flag = |bit: u32| (self.flags & (1_u32 << bit)) != 0_u32;
        let modifiers = InputModifiers {
            shift: flag(0),
            control: flag(1),
            alt: flag(2),
            caps_lock: flag(3),
            num_lock: flag(4),
            left_button: flag(5),
            middle_button: flag(6),
            right_button: flag(7),
            keypad: flag(8),
            left_key: flag(9),
            right_key: flag(10),
            meta: flag(11),
        };
        let value = &self.value;
        Ok(match self.code {
            1 => BrowserInput::Move {
                x: integer(value[0])?,
                y: integer(value[1])?,
                modifiers,
                leave: boolean(value[2])?,
            },
            2 => BrowserInput::Button {
                x: integer(value[0])?,
                y: integer(value[1])?,
                modifiers,
                button: match value[2] {
                    0 => MouseButton::Left,
                    1 => MouseButton::Middle,
                    2 => MouseButton::Right,
                    _ => return Err(invalid("invalid mouse button")),
                },
                up: boolean(value[3])?,
                click_count: integer(value[4])?,
            },
            3 => BrowserInput::Wheel {
                x: integer(value[0])?,
                y: integer(value[1])?,
                modifiers,
                delta_x: integer(value[2])?,
                delta_y: integer(value[3])?,
            },
            4 => BrowserInput::Key {
                kind: match value[0] {
                    0 => KeyKind::RawDown,
                    1 => KeyKind::Up,
                    2 => KeyKind::Character,
                    _ => return Err(invalid("invalid key kind")),
                },
                modifiers,
                windows_key_code: integer(value[1])?,
                native_key_code: integer(value[2])?,
                is_system_key: boolean(value[3])?,
                character: u16::try_from(value[4]).map_err(|_| invalid("invalid character"))?,
                unmodified_character: u16::try_from(value[5])
                    .map_err(|_| invalid("invalid character"))?,
            },
            _ => return Err(invalid("unknown browser input")),
        })
    }
}

fn signed(value: i32) -> u64 {
    u64::from(value as u32)
}
fn integer(value: u64) -> io::Result<i32> {
    u32::try_from(value)
        .map(|value| value as i32)
        .map_err(|_| invalid("invalid input integer"))
}
fn boolean(value: u64) -> io::Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("invalid input boolean")),
    }
}
pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn pixel_bytes(width: u32, height: u32) -> io::Result<usize> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid BGRA extent",
        ));
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > MAX_FRAME_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid BGRA extent",
        ));
    }
    Ok(bytes as usize)
}
