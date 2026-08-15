//! Small, fail-open agent-side protocol state machine. It owns no graphics
//! resources: descriptors and descriptors' FDs are passed through unchanged.
pub mod completion;
pub use completion::CompletionQueue;

use overlay_protocol::{
    BoolValue, CAP_DMABUF, CAP_FRAME_REQUEST, CAP_SHM, COOKIE_LEN, DmabufDescriptor, Endpoint,
    FrameConsumed, FrameRequest, Hello, InputEvent, MessageKind, Packet, ProtocolError,
    SelectedTarget, Seqpacket, Target, TextureDescriptor,
};
use std::{
    collections::VecDeque,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const MAX_INPUT_QUEUE: usize = 256;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("agent is disconnected")]
    Disconnected,
    #[error("invalid peer welcome")]
    InvalidWelcome,
}

pub struct AgentConfig {
    pub pid: u32,
    pub uid: u32,
    pub expected_host_pid: u32,
    pub expected_host_uid: u32,
    /// Capabilities the session cannot run without; the host must grant all of
    /// them or the agent refuses the session.
    pub capabilities: u64,
    /// Frame transports this backend can actually consume. The host must grant
    /// at least one of them, but never all: the GL backend can sample either an
    /// imported DMA-BUF or an uploaded SHM buffer, while the Vulkan layer only
    /// has a DMA-BUF import path. Requiring the union here is what previously
    /// forced every backend to claim SHM.
    pub required_transports: u64,
    /// Everything this target can actually use. The host picks its frame
    /// transport from this set, so it must reflect the live graphics context.
    pub advertised_capabilities: u64,
    pub watchdog: Duration,
}
impl AgentConfig {
    pub fn current() -> Self {
        Self {
            pid: unsafe { libc::getpid() } as u32,
            uid: unsafe { libc::geteuid() },
            expected_host_pid: unsafe { libc::getppid() } as u32,
            expected_host_uid: unsafe { libc::geteuid() },
            capabilities: 0,
            required_transports: CAP_SHM,
            advertised_capabilities: CAP_SHM | CAP_FRAME_REQUEST,
            watchdog: Duration::from_secs(2),
        }
    }

    /// Configuration for a backend that can only consume zero-copy buffers.
    /// It advertises and requires DMA-BUF alone, so a host that cannot provide
    /// it fails the handshake instead of silently negotiating an SHM session
    /// whose frames the backend would reject on every present.
    pub fn dmabuf_only() -> Self {
        Self {
            capabilities: 0,
            required_transports: CAP_DMABUF,
            advertised_capabilities: CAP_DMABUF | CAP_FRAME_REQUEST,
            ..Self::current()
        }
    }

    /// The transport bits the handshake must yield for this configuration.
    fn accepts_granted(&self, granted: u64) -> bool {
        granted & self.capabilities == self.capabilities
            && (self.required_transports == 0 || granted & self.required_transports != 0)
    }
}

pub struct FrameMailbox {
    latest: Option<Frame>,
}
impl Default for FrameMailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameMailbox {
    pub fn new() -> Self {
        Self { latest: None }
    }
    pub fn replace(&mut self, frame: Frame) -> Option<Frame> {
        self.latest.replace(frame)
    }
    pub fn take(&mut self) -> Option<Frame> {
        self.latest.take()
    }
}

pub struct Agent {
    conn: Seqpacket,
    granted_capabilities: u64,
    capture: bool,
    capture_since: Option<Instant>,
    watchdog: Duration,
    latest: FrameMailbox,
    input: VecDeque<InputEvent>,
    targets: Vec<u64>,
    disconnected: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameDescriptor {
    Dmabuf(DmabufDescriptor),
    Shm(TextureDescriptor),
}
impl FrameDescriptor {
    pub fn target_id(&self) -> u64 {
        match self {
            Self::Dmabuf(descriptor) => descriptor.target_id,
            Self::Shm(descriptor) => descriptor.target_id,
        }
    }
    pub fn consumed(&self) -> Option<FrameConsumed> {
        match self {
            Self::Dmabuf(descriptor) => Some(FrameConsumed {
                target_id: descriptor.target_id,
                sequence: descriptor.sequence,
            }),
            Self::Shm(_) => None,
        }
    }
}
pub struct Frame {
    pub descriptor: FrameDescriptor,
    pub fds: Vec<OwnedFd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    TargetCreated(Target),
    TargetDestroyed(u64),
    Visible(bool),
    Capture(bool),
    Clear(u64),
    Frame(FrameDescriptor),
    Input(InputEvent),
    Shutdown,
    Ping,
    Error,
}

impl Agent {
    pub fn from_connected(conn: Seqpacket, watchdog: Duration) -> Self {
        Self {
            conn,
            granted_capabilities: 0,
            capture: false,
            capture_since: None,
            watchdog,
            latest: FrameMailbox::new(),
            input: VecDeque::new(),
            targets: Vec::new(),
            disconnected: false,
        }
    }
    pub fn connect(
        endpoint: &Endpoint,
        cookie: [u8; COOKIE_LEN],
        config: AgentConfig,
    ) -> Result<Self, AgentError> {
        let conn = endpoint.connect()?;
        let (uid, pid) = conn.peer_credentials()?;
        if pid != config.expected_host_pid {
            return Err(ProtocolError::TargetPid.into());
        }
        if uid != config.expected_host_uid {
            return Err(ProtocolError::Credentials.into());
        }
        let hello = Hello {
            cookie,
            pid: config.pid,
            uid: config.uid,
            capabilities: config.advertised_capabilities
                | config.capabilities
                | config.required_transports,
        };
        conn.send(&Packet::new(
            MessageKind::Hello,
            1,
            hello.encode().to_vec(),
            Vec::new(),
        )?)?;
        conn.set_read_timeout(Duration::from_secs(2))?;
        let welcome = conn.recv()?;
        conn.set_read_timeout(Duration::ZERO)?;
        if welcome.header.kind != MessageKind::Welcome || welcome.payload.len() != 8 {
            return Err(AgentError::InvalidWelcome);
        }
        let granted = u64::from_le_bytes(
            welcome
                .payload
                .as_slice()
                .try_into()
                .map_err(|_| AgentError::InvalidWelcome)?,
        );
        if !config.accepts_granted(granted) {
            return Err(AgentError::InvalidWelcome);
        }
        Ok(Self {
            conn,
            granted_capabilities: granted,
            capture: false,
            capture_since: None,
            watchdog: config.watchdog,
            latest: FrameMailbox::new(),
            input: VecDeque::with_capacity(MAX_INPUT_QUEUE),
            targets: Vec::new(),
            disconnected: false,
        })
    }
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }
    pub fn supports_frame_requests(&self) -> bool {
        self.granted_capabilities & CAP_FRAME_REQUEST != 0
    }
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.conn.as_raw_fd()
    }
    pub fn poll(&mut self) -> Result<AgentEvent, AgentError> {
        if self.disconnected {
            return Err(AgentError::Disconnected);
        }
        match self.conn.recv() {
            Ok(p) => self.decode(p),
            Err(error)
                if matches!(
                    &error,
                    ProtocolError::Io(io_error)
                        if matches!(
                            io_error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        )
                ) =>
            {
                // A nonblocking socket can be temporarily idle. Keep the
                // authenticated connection alive so the watchdog can run.
                Err(error.into())
            }
            Err(error) => {
                self.release_capture();
                self.disconnected = true;
                Err(error.into())
            }
        }
    }
    pub fn send_dmabuf(
        &mut self,
        descriptor: DmabufDescriptor,
        fds: Vec<OwnedFd>,
    ) -> Result<(), AgentError> {
        if descriptor.planes.len() != fds.len() {
            return Err(ProtocolError::FdCount {
                expected: descriptor.planes.len(),
                actual: fds.len(),
            }
            .into());
        }
        let sequence = descriptor.sequence;
        let p = Packet::new(MessageKind::Dmabuf, sequence, descriptor.encode()?, fds)?;
        self.send_packet(&p)
    }
    pub fn send_shm(
        &mut self,
        descriptor: TextureDescriptor,
        fd: OwnedFd,
    ) -> Result<(), AgentError> {
        let p = Packet::new(MessageKind::Shm, 0, descriptor.encode()?.to_vec(), vec![fd])?;
        self.send_packet(&p)
    }
    pub fn send_input(&mut self, event: InputEvent) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::Input,
            0,
            event.encode().to_vec(),
            Vec::new(),
        )?)
    }
    pub fn send_consumed(&mut self, consumed: FrameConsumed) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::Consumed,
            consumed.sequence,
            consumed.encode().to_vec(),
            Vec::new(),
        )?)
    }
    /// Ask the host for one current offscreen frame. Presents may be coalesced,
    /// so the monotonically advancing sequence is observability, not a promise
    /// that the producer will render every intermediate value.
    pub fn send_frame_request(&mut self, request: FrameRequest) -> Result<(), AgentError> {
        if !self.supports_frame_requests() {
            return Ok(());
        }
        self.send_packet(&Packet::new(
            MessageKind::FrameRequest,
            request.sequence,
            request.encode()?.to_vec(),
            Vec::new(),
        )?)
    }
    /// Announce the target this agent will composite into. The host follows
    /// this instead of guessing from target-creation order, so the two sides
    /// cannot disagree about where frames should go.
    pub fn send_selected_target(&mut self, selection: SelectedTarget) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::SelectTarget,
            selection.generation,
            selection.encode().to_vec(),
            Vec::new(),
        )?)
    }
    pub fn send_end(&mut self, target_id: u64) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::End,
            0,
            target_id.to_le_bytes().to_vec(),
            Vec::new(),
        )?)
    }
    /// Advertise a drawable target to the authenticated host.
    pub fn send_target_create(&mut self, target: Target) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::TargetCreate,
            0,
            target.encode().to_vec(),
            Vec::new(),
        )?)
    }

    /// Remove a drawable target from the authenticated host.
    pub fn send_target_destroy(&mut self, target_id: u64) -> Result<(), AgentError> {
        self.send_packet(&Packet::new(
            MessageKind::TargetDestroy,
            0,
            target_id.to_le_bytes().to_vec(),
            Vec::new(),
        )?)
    }
    fn send_packet(&mut self, p: &Packet) -> Result<(), AgentError> {
        if self.disconnected {
            return Err(AgentError::Disconnected);
        }
        match self.conn.send(p) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.release_capture();
                self.disconnected = true;
                Err(e.into())
            }
        }
    }
    pub fn set_nonblocking(&self) -> Result<(), AgentError> {
        self.conn.set_nonblocking()?;
        Ok(())
    }
    /// Keep only the newest frame and close the superseded descriptor FDs.
    pub fn replace_latest(&mut self, frame: Frame) {
        let _ = self.latest.replace(frame);
    }
    pub fn take_latest(&mut self) -> Option<Frame> {
        self.latest.take()
    }
    pub fn pop_input(&mut self) -> Option<InputEvent> {
        self.input.pop_front()
    }
    pub fn capture_active(&self) -> bool {
        self.capture
    }
    pub fn watchdog_tick(&mut self) -> bool {
        if self.capture
            && self
                .capture_since
                .map(|at| at.elapsed() >= self.watchdog)
                .unwrap_or(false)
        {
            self.release_capture();
            true
        } else {
            false
        }
    }
    pub fn release_capture(&mut self) {
        self.capture = false;
        self.capture_since = None
    }
    fn decode(&mut self, p: Packet) -> Result<AgentEvent, AgentError> {
        let result = (|| match p.header.kind {
            MessageKind::TargetCreate => {
                let t = Target::decode(&p.payload)?;
                if !self.targets.contains(&t.target_id) {
                    self.targets.push(t.target_id);
                }
                Ok(AgentEvent::TargetCreated(t))
            }
            MessageKind::TargetDestroy => {
                if p.payload.len() != 8 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                let id = u64::from_le_bytes(p.payload.try_into().unwrap());
                self.targets.retain(|x| *x != id);
                Ok(AgentEvent::TargetDestroyed(id))
            }
            MessageKind::Visibility => Ok(AgentEvent::Visible(BoolValue::decode(&p.payload)?.0)),
            MessageKind::CaptureInput => {
                let yes = BoolValue::decode(&p.payload)?.0;
                self.capture = yes;
                self.capture_since = yes.then(Instant::now);
                Ok(AgentEvent::Capture(yes))
            }
            MessageKind::Dmabuf => {
                let descriptor = DmabufDescriptor::decode(&p.payload)?;
                if p.header.request_id != descriptor.sequence
                    || p.fds.len() != descriptor.planes.len()
                {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                let event = FrameDescriptor::Dmabuf(descriptor);
                if let Some(consumed) = self
                    .latest
                    .replace(Frame {
                        descriptor: event.clone(),
                        fds: p.fds,
                    })
                    .and_then(|frame| frame.descriptor.consumed())
                {
                    self.send_consumed(consumed)?;
                }
                Ok(AgentEvent::Frame(event))
            }
            MessageKind::Shm => {
                let descriptor = TextureDescriptor::decode(&p.payload)?;
                let event = FrameDescriptor::Shm(descriptor);
                if let Some(consumed) = self
                    .latest
                    .replace(Frame {
                        descriptor: event.clone(),
                        fds: p.fds,
                    })
                    .and_then(|frame| frame.descriptor.consumed())
                {
                    self.send_consumed(consumed)?;
                }
                Ok(AgentEvent::Frame(event))
            }
            MessageKind::Clear => {
                if p.payload.len() != 8 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                Ok(AgentEvent::Clear(u64::from_le_bytes(
                    p.payload.try_into().unwrap(),
                )))
            }
            MessageKind::Shutdown => {
                self.release_capture();
                self.disconnected = true;
                Ok(AgentEvent::Shutdown)
            }
            MessageKind::Ping => {
                self.send_packet(&Packet::new(
                    MessageKind::Pong,
                    p.header.request_id,
                    Vec::new(),
                    Vec::new(),
                )?)?;
                if self.capture {
                    self.capture_since = Some(Instant::now());
                }
                Ok(AgentEvent::Ping)
            }
            MessageKind::Input | MessageKind::FrameRequest => {
                Err(ProtocolError::InvalidPayload.into())
            }
            _ => Ok(AgentEvent::Error),
        })();
        if result.is_err() {
            self.release_capture();
            self.disconnected = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn latest_frame_replaces_old() {
        let mut mailbox = FrameMailbox::new();
        let d = TextureDescriptor {
            target_id: 1,
            width: 1,
            height: 1,
            stride: 4,
            offset: 0,
            size: 4,
            format: 0,
        };
        let fd1 = std::fs::File::open("/dev/null").unwrap().into();
        let fd2 = std::fs::File::open("/dev/null").unwrap().into();
        mailbox.replace(Frame {
            descriptor: FrameDescriptor::Shm(d),
            fds: vec![fd1],
        });
        mailbox.replace(Frame {
            descriptor: FrameDescriptor::Shm(d),
            fds: vec![fd2],
        });
        assert_eq!(mailbox.take().unwrap().descriptor.target_id(), 1);
    }
    #[test]
    fn capture_releases_on_watchdog_and_eof() {
        let (a, b) = Seqpacket::pair().unwrap();
        let mut agent = Agent::from_connected(a, Duration::ZERO);
        b.send(
            &Packet::new(
                MessageKind::CaptureInput,
                0,
                BoolValue(true).encode().to_vec(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(agent.poll().unwrap(), AgentEvent::Capture(true));
        assert!(agent.watchdog_tick());
        assert!(!agent.capture_active());
        drop(b);
        assert!(agent.poll().is_err());
        assert!(agent.is_disconnected());
        assert!(!agent.capture_active());
    }

    #[test]
    fn nonblocking_idle_poll_keeps_connection_alive() {
        let (a, _b) = Seqpacket::pair().unwrap();
        let mut agent = Agent::from_connected(a, Duration::from_secs(2));
        agent.set_nonblocking().unwrap();
        assert!(matches!(
            agent.poll(),
            Err(AgentError::Protocol(ProtocolError::Io(error)))
                if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(!agent.is_disconnected());
    }

    #[test]
    fn frame_request_is_negotiated_and_sent_without_fds() {
        let current = AgentConfig::current();
        let dmabuf = AgentConfig::dmabuf_only();
        assert_ne!(current.advertised_capabilities & CAP_FRAME_REQUEST, 0);
        assert_ne!(dmabuf.advertised_capabilities & CAP_FRAME_REQUEST, 0);

        let (a, b) = Seqpacket::pair().unwrap();
        let mut agent = Agent::from_connected(a, Duration::from_secs(2));
        agent.granted_capabilities = CAP_FRAME_REQUEST;
        let request = FrameRequest {
            target_id: 7,
            sequence: 11,
        };
        agent.send_frame_request(request).unwrap();
        let packet = b.recv().unwrap();
        assert_eq!(packet.header.kind, MessageKind::FrameRequest);
        assert_eq!(packet.header.request_id, request.sequence);
        assert_eq!(FrameRequest::decode(&packet.payload).unwrap(), request);
        assert!(packet.fds.is_empty());
    }

    #[test]
    fn malformed_packet_releases_capture_and_disconnects() {
        let (a, b) = Seqpacket::pair().unwrap();
        let mut agent = Agent::from_connected(a, Duration::from_secs(2));
        b.send(
            &Packet::new(
                MessageKind::CaptureInput,
                0,
                BoolValue(true).encode().to_vec(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(agent.poll().unwrap(), AgentEvent::Capture(true));
        b.send(&Packet::new(MessageKind::Visibility, 0, vec![2], Vec::new()).unwrap())
            .unwrap();

        assert!(matches!(
            agent.poll(),
            Err(AgentError::Protocol(ProtocolError::InvalidPayload))
        ));
        assert!(!agent.capture_active());
        assert!(agent.is_disconnected());
    }
}