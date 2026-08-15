//! Bounded wire protocol and Linux abstract `SOCK_SEQPACKET` transport.
//! The wire format is intentionally explicit and little endian; no Rust layout is
//! ever exposed on the socket.

use std::{
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr,
    time::Duration,
};

use thiserror::Error;

pub const MAGIC: [u8; 8] = *b"VOSUOVLY";
pub const MAJOR_VERSION: u16 = 1;
pub const MINOR_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 28;
pub const MAX_PACKET: usize = 64 * 1024;
pub const MAX_PAYLOAD: usize = MAX_PACKET - HEADER_LEN;
pub const MAX_FDS: usize = 4;
pub const COOKIE_LEN: usize = 32;
pub const SOCKET_SUFFIX_LEN: usize = 16;
pub const MAX_TEXTURE_DIMENSION: u32 = 16_384;
pub const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const DRM_FORMAT_ABGR8888: u32 = u32::from_le_bytes(*b"AB24");

/// Shared-memory frame transport. Backends that can always upload a CPU buffer
/// advertise it; it is not universally mandatory, because a backend may only be
/// able to consume zero-copy buffers.
pub const CAP_SHM: u64 = 1 << 0;
/// DMA-BUF frame transport. Only targets whose current graphics context can
/// actually sample an imported buffer advertise it.
pub const CAP_DMABUF: u64 = 1 << 1;
/// Shared-memory frames in Chromium's native BGRA order, avoiding a per-frame
/// channel swizzle on the host.
pub const CAP_SHM_BGRA: u64 = 1 << 2;
/// The presenting backend can drive offscreen rendering from game presents.
/// Without this bit the host keeps its legacy fixed-rate renderer running.
pub const CAP_FRAME_REQUEST: u64 = 1 << 3;
/// Every capability that can carry frame pixels. A usable session must grant at
/// least one of these, but which one is backend-specific: the GL agent accepts
/// either, while the Vulkan layer can only consume DMA-BUF.
pub const CAP_TRANSPORT_MASK: u64 = CAP_SHM | CAP_DMABUF;

pub const TEXTURE_FORMAT_STRAIGHT_RGBA8: u32 = 0;
pub const TEXTURE_FORMAT_PREMULTIPLIED_RGBA8: u32 = 1;
pub const TEXTURE_FORMAT_PREMULTIPLIED_BGRA8: u32 = 2;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("packet is truncated")]
    Truncated,
    #[error("bad protocol magic")]
    BadMagic,
    #[error("unsupported major protocol version {0}")]
    MajorVersion(u16),
    #[error("payload length {0} exceeds limit")]
    PayloadTooLarge(usize),
    #[error("invalid message kind {0}")]
    MessageKind(u16),
    #[error("invalid message payload")]
    InvalidPayload,
    #[error("too many file descriptors")]
    TooManyFds,
    #[error("unexpected number of file descriptors: expected {expected}, got {actual}")]
    FdCount { expected: usize, actual: usize },
    #[error("unexpected ancillary data")]
    Ancillary,
    #[error("credential mismatch")]
    Credentials,
    #[error("cookie mismatch")]
    Cookie,
    #[error("target pid mismatch")]
    TargetPid,
    #[error("io: {0}")]
    Io(#[from] io::Error),
}
impl PartialEq for ProtocolError {
    fn eq(&self, other: &Self) -> bool {
        use ProtocolError::*;
        match (self, other) {
            (Truncated, Truncated)
            | (BadMagic, BadMagic)
            | (InvalidPayload, InvalidPayload)
            | (TooManyFds, TooManyFds)
            | (Ancillary, Ancillary)
            | (Credentials, Credentials)
            | (Cookie, Cookie)
            | (TargetPid, TargetPid)
            | (Io(_), Io(_)) => true,
            (MajorVersion(a), MajorVersion(b)) => a == b,
            (PayloadTooLarge(a), PayloadTooLarge(b)) => a == b,
            (MessageKind(a), MessageKind(b)) => a == b,
            (
                FdCount {
                    expected: a,
                    actual: b,
                },
                FdCount {
                    expected: c,
                    actual: d,
                },
            ) => a == c && b == d,
            _ => false,
        }
    }
}
impl Eq for ProtocolError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageKind {
    Hello = 1,
    Welcome = 2,
    TargetCreate = 3,
    TargetDestroy = 4,
    Dmabuf = 5,
    Shm = 6,
    Consumed = 7,
    Clear = 8,
    Visibility = 9,
    CaptureInput = 10,
    Input = 11,
    End = 12,
    Ping = 13,
    Pong = 14,
    Shutdown = 15,
    Error = 16,
    /// Agent to host: the agent has authoritatively chosen which registered
    /// target it will composite into. The host must follow this selection
    /// instead of inferring one from target-creation order.
    SelectTarget = 17,
    /// Agent to host: one or more game presents occurred for the selected
    /// target. Requests are latest-wins and may be coalesced by every hop.
    FrameRequest = 18,
}

impl TryFrom<u16> for MessageKind {
    type Error = ProtocolError;
    fn try_from(value: u16) -> Result<Self, ProtocolError> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::Welcome,
            3 => Self::TargetCreate,
            4 => Self::TargetDestroy,
            5 => Self::Dmabuf,
            6 => Self::Shm,
            7 => Self::Consumed,
            8 => Self::Clear,
            9 => Self::Visibility,
            10 => Self::CaptureInput,
            11 => Self::Input,
            12 => Self::End,
            13 => Self::Ping,
            14 => Self::Pong,
            15 => Self::Shutdown,
            16 => Self::Error,
            17 => Self::SelectTarget,
            18 => Self::FrameRequest,
            other => return Err(ProtocolError::MessageKind(other)),
        })
    }
}

/// Header occupies exactly 28 bytes on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageHeader {
    pub major: u16,
    pub minor: u16,
    pub kind: MessageKind,
    pub flags: u16,
    pub payload_len: u32,
    pub request_id: u64,
}

impl MessageHeader {
    pub const fn new(kind: MessageKind, payload_len: usize, request_id: u64) -> Self {
        Self {
            major: MAJOR_VERSION,
            minor: MINOR_VERSION,
            kind,
            flags: 0,
            payload_len: payload_len as u32,
            request_id,
        }
    }
    pub fn encode(self) -> Result<[u8; HEADER_LEN], ProtocolError> {
        if self.payload_len as usize > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(self.payload_len as usize));
        }
        let mut out = [0u8; HEADER_LEN];
        out[..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.major.to_le_bytes());
        out[10..12].copy_from_slice(&self.minor.to_le_bytes());
        out[12..14].copy_from_slice(&(self.kind as u16).to_le_bytes());
        out[14..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16..20].copy_from_slice(&self.payload_len.to_le_bytes());
        out[20..28].copy_from_slice(&self.request_id.to_le_bytes());
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < HEADER_LEN {
            return Err(ProtocolError::Truncated);
        }
        if bytes[..8] != MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let major = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        if major != MAJOR_VERSION {
            return Err(ProtocolError::MajorVersion(major));
        }
        let minor = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
        let kind = MessageKind::try_from(u16::from_le_bytes(bytes[12..14].try_into().unwrap()))?;
        let flags = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
        let payload_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        if payload_len as usize > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload_len as usize));
        }
        let request_id = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
        Ok(Self {
            major,
            minor,
            kind,
            flags,
            payload_len,
            request_id,
        })
    }
}

#[derive(Debug)]
pub struct Packet {
    pub header: MessageHeader,
    pub payload: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

impl Packet {
    pub fn new(
        kind: MessageKind,
        request_id: u64,
        payload: Vec<u8>,
        fds: Vec<OwnedFd>,
    ) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }
        if fds.len() > MAX_FDS {
            return Err(ProtocolError::TooManyFds);
        }
        if !valid_fd_count(kind, fds.len()) {
            return Err(ProtocolError::FdCount {
                expected: expected_fd_count(kind),
                actual: fds.len(),
            });
        }
        Ok(Self {
            header: MessageHeader::new(kind, payload.len(), request_id),
            payload,
            fds,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(self.payload.len()));
        }
        if self.header.payload_len as usize != self.payload.len() {
            return Err(ProtocolError::InvalidPayload);
        }
        if !valid_fd_count(self.header.kind, self.fds.len()) {
            return Err(ProtocolError::FdCount {
                expected: expected_fd_count(self.header.kind),
                actual: self.fds.len(),
            });
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.header.encode()?);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
    pub fn decode(bytes: &[u8], fds: Vec<OwnedFd>) -> Result<Self, ProtocolError> {
        let header = MessageHeader::decode(bytes)?;
        let payload = bytes.get(HEADER_LEN..).ok_or(ProtocolError::Truncated)?;
        if payload.len() != header.payload_len as usize {
            return Err(ProtocolError::Truncated);
        }
        if fds.len() > MAX_FDS {
            return Err(ProtocolError::TooManyFds);
        }
        if !valid_fd_count(header.kind, fds.len()) {
            return Err(ProtocolError::FdCount {
                expected: expected_fd_count(header.kind),
                actual: fds.len(),
            });
        }
        Ok(Self {
            header,
            payload: payload.to_vec(),
            fds,
        })
    }
}

pub const fn expected_fd_count(kind: MessageKind) -> usize {
    match kind {
        MessageKind::Dmabuf | MessageKind::Shm => 1,
        _ => 0,
    }
}
const fn valid_fd_count(kind: MessageKind, count: usize) -> bool {
    match kind {
        MessageKind::Dmabuf => count >= 1 && count <= MAX_FDS,
        MessageKind::Shm => count == 1,
        _ => count == 0,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub cookie: [u8; COOKIE_LEN],
    pub pid: u32,
    pub uid: u32,
    pub capabilities: u64,
}
impl Hello {
    pub const LEN: usize = COOKIE_LEN + 4 + 4 + 8;
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[..COOKIE_LEN].copy_from_slice(&self.cookie);
        out[COOKIE_LEN..COOKIE_LEN + 4].copy_from_slice(&self.pid.to_le_bytes());
        out[COOKIE_LEN + 4..COOKIE_LEN + 8].copy_from_slice(&self.uid.to_le_bytes());
        out[COOKIE_LEN + 8..].copy_from_slice(&self.capabilities.to_le_bytes());
        out
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self {
            cookie: p[..COOKIE_LEN]
                .try_into()
                .map_err(|_| ProtocolError::InvalidPayload)?,
            pid: u32::from_le_bytes(p[COOKIE_LEN..COOKIE_LEN + 4].try_into().unwrap()),
            uid: u32::from_le_bytes(p[COOKIE_LEN + 4..COOKIE_LEN + 8].try_into().unwrap()),
            capabilities: u64::from_le_bytes(p[COOKIE_LEN + 8..].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub target_id: u64,
    pub width: u32,
    pub height: u32,
}
impl Target {
    pub const LEN: usize = 16;
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut b = [0; Self::LEN];
        b[..8].copy_from_slice(&self.target_id.to_le_bytes());
        b[8..12].copy_from_slice(&self.width.to_le_bytes());
        b[12..].copy_from_slice(&self.height.to_le_bytes());
        b
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        };
        Ok(Self {
            target_id: u64::from_le_bytes(p[..8].try_into().unwrap()),
            width: u32::from_le_bytes(p[8..12].try_into().unwrap()),
            height: u32::from_le_bytes(p[12..].try_into().unwrap()),
        })
    }
}

/// Authoritative composition target announced by the agent. `generation`
/// increases on every selection change, so a host that observes selections out
/// of order can discard stale ones instead of ping-ponging between targets.
/// `target_id` is zero when the agent currently has no drawable to composite
/// into, which the host treats as "stop producing frames".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedTarget {
    pub target_id: u64,
    pub generation: u64,
}
impl SelectedTarget {
    pub const LEN: usize = 16;
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut b = [0; Self::LEN];
        b[..8].copy_from_slice(&self.target_id.to_le_bytes());
        b[8..].copy_from_slice(&self.generation.to_le_bytes());
        b
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self {
            target_id: u64::from_le_bytes(p[..8].try_into().unwrap()),
            generation: u64::from_le_bytes(p[8..].try_into().unwrap()),
        })
    }
}

/// Coalescible demand signal emitted by the presenting backend. `sequence`
/// advances whenever the game presents the selected target; the host only
/// needs the newest value and never treats a request as a resource release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRequest {
    pub target_id: u64,
    pub sequence: u64,
}
impl FrameRequest {
    pub const LEN: usize = 16;
    pub fn encode(self) -> Result<[u8; Self::LEN], ProtocolError> {
        self.validate()?;
        let mut bytes = [0u8; Self::LEN];
        bytes[..8].copy_from_slice(&self.target_id.to_le_bytes());
        bytes[8..].copy_from_slice(&self.sequence.to_le_bytes());
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        let request = Self {
            target_id: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            sequence: u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        };
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.target_id == 0 || self.sequence == 0 {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDescriptor {
    pub target_id: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub offset: u64,
    pub size: u64,
    pub format: u32,
}
impl TextureDescriptor {
    pub const LEN: usize = 40;
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let min_stride = (self.width as u64)
            .checked_mul(4)
            .ok_or(ProtocolError::InvalidPayload)?;
        if self.width == 0
            || self.height == 0
            || self.width > MAX_TEXTURE_DIMENSION
            || self.height > MAX_TEXTURE_DIMENSION
            || (self.stride as u64) < min_stride
        {
            return Err(ProtocolError::InvalidPayload);
        }
        let rows = self.height as u64 - 1;
        let row_bytes = rows
            .checked_mul(self.stride as u64)
            .ok_or(ProtocolError::InvalidPayload)?;
        let pixels = min_stride;
        let end = self
            .offset
            .checked_add(row_bytes)
            .and_then(|x| x.checked_add(pixels))
            .ok_or(ProtocolError::InvalidPayload)?;
        if end > self.size {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(())
    }
    pub fn encode(self) -> Result<[u8; Self::LEN], ProtocolError> {
        self.validate()?;
        let mut b = [0; Self::LEN];
        b[..8].copy_from_slice(&self.target_id.to_le_bytes());
        b[8..12].copy_from_slice(&self.width.to_le_bytes());
        b[12..16].copy_from_slice(&self.height.to_le_bytes());
        b[16..20].copy_from_slice(&self.stride.to_le_bytes());
        b[20..28].copy_from_slice(&self.offset.to_le_bytes());
        b[28..36].copy_from_slice(&self.size.to_le_bytes());
        b[36..40].copy_from_slice(&self.format.to_le_bytes());
        Ok(b)
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        };
        let d = Self {
            target_id: u64::from_le_bytes(p[..8].try_into().unwrap()),
            width: u32::from_le_bytes(p[8..12].try_into().unwrap()),
            height: u32::from_le_bytes(p[12..16].try_into().unwrap()),
            stride: u32::from_le_bytes(p[16..20].try_into().unwrap()),
            offset: u64::from_le_bytes(p[20..28].try_into().unwrap()),
            size: u64::from_le_bytes(p[28..36].try_into().unwrap()),
            format: u32::from_le_bytes(p[36..40].try_into().unwrap()),
        };
        d.validate()?;
        Ok(d)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmabufPlane {
    pub stride: u32,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmabufDescriptor {
    pub target_id: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
}

impl DmabufDescriptor {
    const HEADER_LEN: usize = 40;
    const PLANE_LEN: usize = 24;

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.target_id == 0
            || self.sequence == 0
            || self.width == 0
            || self.height == 0
            || self.width > MAX_TEXTURE_DIMENSION
            || self.height > MAX_TEXTURE_DIMENSION
            || self.planes.len() != 1
            || !matches!(self.fourcc, DRM_FORMAT_ARGB8888 | DRM_FORMAT_ABGR8888)
        {
            return Err(ProtocolError::InvalidPayload);
        }
        let plane = self.planes[0];
        let row_bytes = (self.width as u64)
            .checked_mul(4)
            .ok_or(ProtocolError::InvalidPayload)?;
        if (plane.stride as u64) < row_bytes {
            return Err(ProtocolError::InvalidPayload);
        }
        let end = plane
            .offset
            .checked_add(
                (self.height as u64 - 1)
                    .checked_mul(plane.stride as u64)
                    .ok_or(ProtocolError::InvalidPayload)?,
            )
            .and_then(|value| value.checked_add(row_bytes))
            .ok_or(ProtocolError::InvalidPayload)?;
        if end > plane.size {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(Self::HEADER_LEN + self.planes.len() * Self::PLANE_LEN);
        bytes.extend_from_slice(&self.target_id.to_le_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&self.fourcc.to_le_bytes());
        bytes.extend_from_slice(&(self.planes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.modifier.to_le_bytes());
        for plane in &self.planes {
            bytes.extend_from_slice(&plane.stride.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&plane.offset.to_le_bytes());
            bytes.extend_from_slice(&plane.size.to_le_bytes());
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < Self::HEADER_LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        let plane_count = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        if plane_count == 0
            || plane_count > MAX_FDS
            || bytes.len() != Self::HEADER_LEN + plane_count * Self::PLANE_LEN
        {
            return Err(ProtocolError::InvalidPayload);
        }
        let mut planes = Vec::with_capacity(plane_count);
        for chunk in bytes[Self::HEADER_LEN..].chunks_exact(Self::PLANE_LEN) {
            if u32::from_le_bytes(chunk[4..8].try_into().unwrap()) != 0 {
                return Err(ProtocolError::InvalidPayload);
            }
            planes.push(DmabufPlane {
                stride: u32::from_le_bytes(chunk[..4].try_into().unwrap()),
                offset: u64::from_le_bytes(chunk[8..16].try_into().unwrap()),
                size: u64::from_le_bytes(chunk[16..24].try_into().unwrap()),
            });
        }
        let descriptor = Self {
            target_id: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            sequence: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            width: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            height: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            fourcc: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            modifier: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            planes,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameConsumed {
    pub target_id: u64,
    pub sequence: u64,
}
impl FrameConsumed {
    pub const LEN: usize = 16;
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[..8].copy_from_slice(&self.target_id.to_le_bytes());
        bytes[8..].copy_from_slice(&self.sequence.to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        let value = Self {
            target_id: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            sequence: u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        };
        if value.target_id == 0 || value.sequence == 0 {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoolValue(pub bool);
impl BoolValue {
    pub fn encode(self) -> [u8; 1] {
        [self.0 as u8]
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != 1 || p[0] > 1 {
            return Err(ProtocolError::InvalidPayload);
        };
        Ok(Self(p[0] != 0))
    }
}

pub const INPUT_KEY_DOWN: u16 = 1;
pub const INPUT_KEY_UP: u16 = 2;
pub const INPUT_TEXT: u16 = 3;
pub const INPUT_MOUSE_MOVE: u16 = 4;
pub const INPUT_MOUSE_DOWN: u16 = 5;
pub const INPUT_MOUSE_UP: u16 = 6;
pub const INPUT_MOUSE_WHEEL: u16 = 7;
pub const INPUT_FLAG_REPEAT: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub target_id: u64,
    pub kind: u16,
    pub code: u16,
    pub flags: u32,
    pub value: i32,
    pub value2: i32,
}
impl InputEvent {
    pub const LEN: usize = 24;
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut b = [0; Self::LEN];
        b[..8].copy_from_slice(&self.target_id.to_le_bytes());
        b[8..10].copy_from_slice(&self.kind.to_le_bytes());
        b[10..12].copy_from_slice(&self.code.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..20].copy_from_slice(&self.value.to_le_bytes());
        b[20..].copy_from_slice(&self.value2.to_le_bytes());
        b
    }
    pub fn decode(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() != Self::LEN {
            return Err(ProtocolError::InvalidPayload);
        }
        let event = Self {
            target_id: u64::from_le_bytes(p[..8].try_into().unwrap()),
            kind: u16::from_le_bytes(p[8..10].try_into().unwrap()),
            code: u16::from_le_bytes(p[10..12].try_into().unwrap()),
            flags: u32::from_le_bytes(p[12..16].try_into().unwrap()),
            value: i32::from_le_bytes(p[16..20].try_into().unwrap()),
            value2: i32::from_le_bytes(p[20..].try_into().unwrap()),
        };
        if !(INPUT_KEY_DOWN..=INPUT_MOUSE_WHEEL).contains(&event.kind) {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(event)
    }
}

/// A Linux abstract namespace socket name: the first byte is the abstract marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub name: Vec<u8>,
}
impl Endpoint {
    pub fn from_name(name: &[u8]) -> Result<Self, ProtocolError> {
        if name.is_empty() || name.len() + 1 > 108 {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self {
            name: name.to_vec(),
        })
    }
    pub fn name(&self) -> &[u8] {
        &self.name
    }
    pub fn random() -> Result<(Self, [u8; COOKIE_LEN]), ProtocolError> {
        let mut cookie = [0u8; COOKIE_LEN];
        getrandom::fill(&mut cookie).map_err(io::Error::other)?;
        let mut suffix = [0u8; SOCKET_SUFFIX_LEN];
        suffix.copy_from_slice(&cookie[..SOCKET_SUFFIX_LEN]);
        let mut name = Vec::with_capacity(1 + SOCKET_SUFFIX_LEN + 8);
        name.extend_from_slice(b"vosu-overlay-");
        name.extend_from_slice(&suffix);
        Ok((Self { name }, cookie))
    }
    fn addr(&self) -> Result<(libc::sockaddr_un, libc::socklen_t), ProtocolError> {
        if self.name.len() + 1 > 108 {
            return Err(ProtocolError::InvalidPayload);
        }
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        addr.sun_path[0] = 0;
        for (i, &v) in self.name.iter().enumerate() {
            addr.sun_path[i + 1] = v as libc::c_char;
        }
        Ok((
            addr,
            (mem::size_of::<libc::sa_family_t>() + 1 + self.name.len()) as libc::socklen_t,
        ))
    }
    pub fn bind_listener(&self) -> Result<SeqpacketListener, ProtocolError> {
        let fd = socket()?;
        let (addr, len) = self.addr()?;
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let r = unsafe { libc::listen(fd.as_raw_fd(), 8) };
        if r < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(SeqpacketListener { fd })
    }
    pub fn connect(&self) -> Result<Seqpacket, ProtocolError> {
        let fd = socket()?;
        let (addr, len) = self.addr()?;
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Seqpacket { fd })
    }
}
fn socket() -> Result<OwnedFd, ProtocolError> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub struct SeqpacketListener {
    fd: OwnedFd,
}
impl SeqpacketListener {
    pub fn set_nonblocking(&self) -> Result<(), ProtocolError> {
        let flags = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn accept(&self) -> Result<Seqpacket, ProtocolError> {
        let fd = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Seqpacket {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
pub struct Seqpacket {
    fd: OwnedFd,
}
impl Seqpacket {
    pub fn pair() -> Result<(Self, Self), ProtocolError> {
        let mut fds = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok((
            Self {
                fd: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            },
            Self {
                fd: unsafe { OwnedFd::from_raw_fd(fds[1]) },
            },
        ))
    }
    pub fn set_read_timeout(&self, timeout: Duration) -> Result<(), ProtocolError> {
        set_socket_timeout(self.as_raw_fd(), libc::SO_RCVTIMEO, timeout)
    }
    pub fn set_write_timeout(&self, timeout: Duration) -> Result<(), ProtocolError> {
        set_socket_timeout(self.as_raw_fd(), libc::SO_SNDTIMEO, timeout)
    }
    pub fn set_nonblocking(&self) -> Result<(), ProtocolError> {
        let flags = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if unsafe { libc::fcntl(self.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
    pub fn peer_credentials(&self) -> Result<(u32, u32), ProtocolError> {
        let mut c: libc::ucred = unsafe { mem::zeroed() };
        let mut n = mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut c as *mut _ as *mut _,
                &mut n,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        if n as usize != mem::size_of::<libc::ucred>() {
            return Err(ProtocolError::Credentials);
        }
        Ok((c.uid, c.pid as u32))
    }
    /// Send one packet with a finite deadline.  A peer that stops reading
    /// cannot pin a caller in `sendmsg`.
    pub fn send(&self, p: &Packet) -> Result<(), ProtocolError> {
        const SEND_DEADLINE: Duration = Duration::from_secs(2);
        self.send_with_deadline(p, SEND_DEADLINE)
    }
    pub fn send_with_deadline(&self, p: &Packet, timeout: Duration) -> Result<(), ProtocolError> {
        let data = p.encode()?;
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut _,
            iov_len: data.len(),
        };
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let mut cbuf = ControlBuffer::zeroed();
        let raw_fds: Vec<RawFd> = p.fds.iter().map(AsRawFd::as_raw_fd).collect();
        if !raw_fds.is_empty() {
            let needed =
                unsafe { libc::CMSG_SPACE((raw_fds.len() * mem::size_of::<RawFd>()) as u32) }
                    as usize;
            if needed > mem::size_of_val(&cbuf) {
                return Err(ProtocolError::Ancillary);
            }
            msg.msg_control = cbuf.as_mut_ptr();
            msg.msg_controllen = needed;
            let c = unsafe { libc::CMSG_FIRSTHDR(&msg) };
            if c.is_null() {
                return Err(ProtocolError::Ancillary);
            }
            unsafe {
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len =
                    libc::CMSG_LEN((raw_fds.len() * mem::size_of::<RawFd>()) as u32) as usize;
                ptr::copy_nonoverlapping(
                    raw_fds.as_ptr(),
                    libc::CMSG_DATA(c) as *mut RawFd,
                    raw_fds.len(),
                );
            }
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let n = unsafe {
                libc::sendmsg(
                    self.as_raw_fd(),
                    &msg,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            };
            if n >= 0 {
                if n as usize != data.len() {
                    return Err(ProtocolError::Truncated);
                }
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if e.kind() != io::ErrorKind::WouldBlock {
                return Err(e.into());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "send deadline").into());
            }
            let mut pfd = libc::pollfd {
                fd: self.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
            let r = unsafe { libc::poll(&mut pfd, 1, ms) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            if r == 0 {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "send deadline").into());
            }
        }
    }
    pub fn recv(&self) -> Result<Packet, ProtocolError> {
        let mut data = vec![0u8; MAX_PACKET];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr() as *mut _,
            iov_len: data.len(),
        };
        let mut cbuf = ControlBuffer::zeroed();
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr();
        msg.msg_controllen = mem::size_of_val(&cbuf);
        let n = loop {
            let n = unsafe { libc::recvmsg(self.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break n;
        };
        if n == 0 {
            return Err(ProtocolError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed",
            )));
        }
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // Always take ownership before checking truncation/count.  Every
        // descriptor visible in ancillary storage is then closed by `Packet`
        // on every rejection path, including malformed cmsgs.
        let mut fds = Vec::new();
        let mut ancillary_error = None;
        let mut c = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !c.is_null() {
            let h = unsafe { &*c };
            if h.cmsg_level != libc::SOL_SOCKET || h.cmsg_type != libc::SCM_RIGHTS {
                ancillary_error = Some(ProtocolError::Ancillary);
                break;
            }
            let base = unsafe { libc::CMSG_LEN(0) } as usize;
            let len = h.cmsg_len as usize;
            if len < base || !(len - base).is_multiple_of(mem::size_of::<RawFd>()) {
                ancillary_error = Some(ProtocolError::Ancillary);
                break;
            }
            let count = (len - base) / mem::size_of::<RawFd>();
            let raw = unsafe { libc::CMSG_DATA(c) as *const RawFd };
            for i in 0..count {
                let fd = unsafe { *raw.add(i) };
                if fd < 0 {
                    ancillary_error = Some(ProtocolError::Ancillary);
                    break;
                }
                fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
            if ancillary_error.is_some() {
                break;
            }
            c = unsafe { libc::CMSG_NXTHDR(&msg, c) };
        }
        if let Some(e) = ancillary_error {
            return Err(e);
        }
        if msg.msg_flags & libc::MSG_TRUNC != 0 {
            return Err(ProtocolError::Truncated);
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(ProtocolError::Truncated);
        }
        if fds.len() > MAX_FDS {
            return Err(ProtocolError::TooManyFds);
        }
        data.truncate(n as usize);
        Packet::decode(&data, fds)
    }
}

#[repr(C, align(16))]
struct ControlBuffer([usize; 32]);
impl ControlBuffer {
    fn zeroed() -> Self {
        Self([0; 32])
    }
    fn as_mut_ptr(&mut self) -> *mut libc::c_void {
        self.0.as_mut_ptr() as *mut libc::c_void
    }
}
fn set_socket_timeout(fd: RawFd, opt: libc::c_int, timeout: Duration) -> Result<(), ProtocolError> {
    let tv = libc::timeval {
        tv_sec: timeout.as_secs().min(i64::MAX as u64) as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            opt,
            &tv as *const _ as *const _,
            mem::size_of_val(&tv) as libc::socklen_t,
        )
    } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::IntoRawFd;

    #[test]
    fn header_is_explicit_and_roundtrips() {
        let h = MessageHeader::new(MessageKind::Ping, 4, 9);
        let b = h.encode().unwrap();
        assert_eq!(b.len(), HEADER_LEN);
        assert_eq!(&b[..8], &MAGIC);
        assert_eq!(MessageHeader::decode(&b).unwrap(), h);
    }
    #[test]
    fn rejects_truncated_major_and_packet_payload() {
        assert_eq!(
            MessageHeader::decode(&[0; 3]),
            Err(ProtocolError::Truncated)
        );
        let mut b = MessageHeader::new(MessageKind::Ping, 0, 0)
            .encode()
            .unwrap();
        b[8] = 2;
        assert!(matches!(
            MessageHeader::decode(&b),
            Err(ProtocolError::MajorVersion(2))
        ));
        assert!(matches!(
            Packet::decode(
                &MessageHeader::new(MessageKind::Ping, 4, 0)
                    .encode()
                    .unwrap(),
                Vec::new()
            ),
            Err(ProtocolError::Truncated)
        ));
    }
    #[test]
    fn fd_count_is_strict_and_seqpacket_transfers_rights() {
        let f = File::open("/dev/null").unwrap();
        let fd = unsafe { OwnedFd::from_raw_fd(f.into_raw_fd()) };
        assert!(matches!(
            Packet::new(MessageKind::Ping, 0, Vec::new(), vec![fd]),
            Err(ProtocolError::FdCount { .. })
        ));
        let (a, b) = Seqpacket::pair().unwrap();
        let f = File::open("/dev/null").unwrap();
        let fd = unsafe { OwnedFd::from_raw_fd(f.into_raw_fd()) };
        let p = Packet::new(
            MessageKind::Dmabuf,
            0,
            TextureDescriptor {
                target_id: 1,
                width: 1,
                height: 1,
                stride: 4,
                offset: 0,
                size: 4,
                format: 0,
            }
            .encode()
            .unwrap()
            .to_vec(),
            vec![fd],
        )
        .unwrap();
        a.send(&p).unwrap();
        let got = b.recv().unwrap();
        assert_eq!(got.fds.len(), 1);
    }
    #[test]
    fn missing_and_unexpected_fds_are_rejected() {
        let header = MessageHeader::new(MessageKind::Dmabuf, 0, 0)
            .encode()
            .unwrap();
        assert!(matches!(
            Packet::decode(&header, Vec::new()),
            Err(ProtocolError::FdCount { .. })
        ));
        let p = MessageHeader::new(MessageKind::Ping, 0, 0)
            .encode()
            .unwrap();
        let fd = unsafe { OwnedFd::from_raw_fd(File::open("/dev/null").unwrap().into_raw_fd()) };
        assert!(matches!(
            Packet::decode(&p, vec![fd]),
            Err(ProtocolError::FdCount { .. })
        ));
    }
    #[test]
    fn texture_bounds_are_checked() {
        let d = TextureDescriptor {
            target_id: 1,
            width: u32::MAX,
            height: 1,
            stride: u32::MAX,
            offset: 0,
            size: u64::MAX,
            format: 0,
        };
        assert!(d.validate().is_err());
        let d = TextureDescriptor {
            target_id: 1,
            width: 1,
            height: u32::MAX,
            stride: u32::MAX,
            offset: u64::MAX,
            size: u64::MAX,
            format: 0,
        };
        assert!(d.validate().is_err());
    }
    #[test]
    fn input_event_roundtrips_and_rejects_unknown_kinds() {
        let event = InputEvent {
            target_id: 7,
            kind: INPUT_MOUSE_MOVE,
            code: 0,
            flags: 3,
            value: 640,
            value2: 360,
        };
        assert_eq!(InputEvent::decode(&event.encode()).unwrap(), event);
        let mut invalid = event.encode();
        invalid[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            InputEvent::decode(&invalid),
            Err(ProtocolError::InvalidPayload)
        );
    }
    #[test]
    fn dmabuf_descriptor_roundtrips_and_rejects_bad_plane_bounds() {
        let descriptor = DmabufDescriptor {
            target_id: 4,
            sequence: 9,
            width: 64,
            height: 32,
            fourcc: DRM_FORMAT_ARGB8888,
            modifier: 0x1020_3040_5060_7080,
            planes: vec![DmabufPlane {
                stride: 256,
                offset: 17,
                size: 17 + 256 * 32,
            }],
        };
        assert_eq!(
            DmabufDescriptor::decode(&descriptor.encode().unwrap()).unwrap(),
            descriptor
        );

        let mut invalid = descriptor;
        invalid.planes[0].size = 32;
        assert!(invalid.validate().is_err());
        invalid.planes[0].size = 17 + 256 * 32;
        invalid.planes.push(invalid.planes[0]);
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn selected_target_roundtrips_and_is_fd_free() {
        let selection = SelectedTarget {
            target_id: 0x1122_3344_5566_7788,
            generation: 7,
        };
        let encoded = selection.encode();
        assert_eq!(encoded.len(), SelectedTarget::LEN);
        assert_eq!(SelectedTarget::decode(&encoded).unwrap(), selection);
        assert!(SelectedTarget::decode(&encoded[..15]).is_err());

        assert_eq!(
            MessageKind::try_from(17).unwrap(),
            MessageKind::SelectTarget
        );
        assert_eq!(expected_fd_count(MessageKind::SelectTarget), 0);
        let packet = Packet::new(
            MessageKind::SelectTarget,
            1,
            encoded.to_vec(),
            vec![File::open("/dev/null").unwrap().into()],
        );
        assert!(matches!(packet, Err(ProtocolError::FdCount { .. })));
    }

    #[test]
    fn frame_request_roundtrips_and_is_fd_free() {
        let request = FrameRequest {
            target_id: 0x1122_3344_5566_7788,
            sequence: 9,
        };
        let encoded = request.encode().unwrap();
        assert_eq!(FrameRequest::decode(&encoded).unwrap(), request);
        assert!(FrameRequest::decode(&encoded[..15]).is_err());
        assert!(FrameRequest {
            target_id: 0,
            sequence: 1,
        }
        .encode()
        .is_err());
        assert!(FrameRequest {
            target_id: 1,
            sequence: 0,
        }
        .encode()
        .is_err());

        assert_eq!(
            MessageKind::try_from(18).unwrap(),
            MessageKind::FrameRequest
        );
        assert_eq!(expected_fd_count(MessageKind::FrameRequest), 0);
        let packet = Packet::new(
            MessageKind::FrameRequest,
            request.sequence,
            encoded.to_vec(),
            vec![File::open("/dev/null").unwrap().into()],
        );
        assert!(matches!(packet, Err(ProtocolError::FdCount { .. })));
    }

    #[test]
    fn transport_mask_covers_exactly_the_frame_carrying_capabilities() {
        assert_eq!(CAP_TRANSPORT_MASK, CAP_SHM | CAP_DMABUF);
        assert_eq!(CAP_TRANSPORT_MASK & (CAP_SHM_BGRA | CAP_FRAME_REQUEST), 0);
    }
}