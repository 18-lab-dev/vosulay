//! Host-side authenticated overlay session.
use overlay_protocol::{
    BoolValue, CAP_DMABUF, CAP_FRAME_REQUEST, CAP_SHM, CAP_SHM_BGRA, CAP_TRANSPORT_MASK,
    COOKIE_LEN, DmabufDescriptor, Endpoint, FrameConsumed, FrameRequest, Hello, InputEvent,
    MAJOR_VERSION, MessageKind, Packet, ProtocolError, SelectedTarget, Seqpacket, Target,
    TextureDescriptor,
};
use std::{
    collections::HashSet,
    os::fd::OwnedFd,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

pub use overlay_protocol::{MessageHeader, MessageKind as Kind};

#[derive(Debug, Error)]
pub enum HostError {
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("peer is not authenticated")]
    NotAuthenticated,
    #[error("target already exists")]
    TargetExists,
    #[error("target does not exist")]
    UnknownTarget,
    #[error("session closed")]
    Closed,
}

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub expected_pid: u32,
    pub expected_uid: u32,
    /// Capabilities the host requires from every agent, all of which must be
    /// granted.
    pub capabilities: u64,
    /// Frame transports the host will accept a session on. The agent must
    /// support at least one of them; which one is the backend's choice, so a
    /// DMA-BUF-only backend is a valid peer.
    pub required_transports: u64,
    /// Capabilities the host is able to use; the granted set is the
    /// intersection with what the agent advertises.
    pub supported_capabilities: u64,
    pub expected_process_group: Option<u32>,
}
impl HostConfig {
    pub fn for_pid(expected_pid: u32) -> Self {
        Self {
            expected_pid,
            expected_process_group: None,
            expected_uid: unsafe { libc::geteuid() },
            capabilities: 0,
            required_transports: CAP_TRANSPORT_MASK,
            supported_capabilities: CAP_SHM | CAP_DMABUF | CAP_SHM_BGRA | CAP_FRAME_REQUEST,
        }
    }

    pub fn for_process_group(expected_process_group: u32) -> Self {
        Self {
            expected_pid: 0,
            expected_process_group: Some(expected_process_group),
            expected_uid: unsafe { libc::geteuid() },
            capabilities: 0,
            required_transports: CAP_TRANSPORT_MASK,
            supported_capabilities: CAP_SHM | CAP_DMABUF | CAP_SHM_BGRA | CAP_FRAME_REQUEST,
        }
    }
}

pub struct Host {
    endpoint: Endpoint,
    cookie: [u8; COOKIE_LEN],
    listener: overlay_protocol::SeqpacketListener,
    config: HostConfig,
}
impl Host {
    pub fn new(expected_pid: u32) -> Result<Self, HostError> {
        Self::with_config(HostConfig::for_pid(expected_pid))
    }
    pub fn with_config(config: HostConfig) -> Result<Self, HostError> {
        let (endpoint, cookie) = Endpoint::random()?;
        let listener = endpoint.bind_listener()?;
        Ok(Self {
            endpoint,
            cookie,
            listener,
            config,
        })
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn cookie(&self) -> [u8; COOKIE_LEN] {
        self.cookie
    }
    pub fn set_expected_pid(&mut self, expected_pid: u32) {
        self.config.expected_pid = expected_pid;
        self.config.expected_process_group = None;
    }
    pub fn set_expected_process_group(&mut self, expected_process_group: u32) {
        self.config.expected_pid = 0;
        self.config.expected_process_group = Some(expected_process_group);
    }
    pub fn config(&self) -> &HostConfig {
        &self.config
    }
    /// Accept one connection and authenticate it. Invalid peers are rejected and
    /// never become an observable session.
    pub fn accept(&self) -> Result<HostSession, HostError> {
        self.authenticate(self.listener.accept()?)
    }
    /// Cancellable bounded accept used by the Node binding. It never leaves a
    /// blocking accept behind when the JS handle is destroyed.
    pub fn accept_cancellable(&self, cancel: &AtomicBool) -> Result<HostSession, HostError> {
        self.accept_cancellable_until(cancel, None)
    }
    /// Cancellable bounded accept used by async host bindings. A timeout rejects
    /// attachment instead of leaving a libuv worker blocked indefinitely.
    pub fn accept_cancellable_timeout(
        &self,
        cancel: &AtomicBool,
        timeout: Duration,
    ) -> Result<HostSession, HostError> {
        self.accept_cancellable_until(cancel, Some(Instant::now() + timeout))
    }
    fn accept_cancellable_until(
        &self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<HostSession, HostError> {
        self.listener.set_nonblocking()?;
        while !cancel.load(Ordering::Acquire) {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(ProtocolError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "overlay agent did not connect before attach deadline",
                ))
                .into());
            }
            match self.listener.accept() {
                Ok(conn) => match self.authenticate(conn) {
                    Ok(session) => return Ok(session),
                    Err(_) => continue,
                },
                Err(ProtocolError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(HostError::Closed)
    }
    fn authenticate(&self, conn: Seqpacket) -> Result<HostSession, HostError> {
        conn.set_read_timeout(Duration::from_secs(2))?;
        let (uid, pid) = conn.peer_credentials()?;
        if !self.config.accepts_pid(pid) {
            return Err(ProtocolError::TargetPid.into());
        }
        if uid != self.config.expected_uid {
            return Err(ProtocolError::Credentials.into());
        }
        let hello = conn.recv()?;
        if hello.header.kind != MessageKind::Hello {
            return Err(ProtocolError::InvalidPayload.into());
        }
        if hello.header.major != MAJOR_VERSION {
            return Err(ProtocolError::MajorVersion(hello.header.major).into());
        }
        let h = Hello::decode(&hello.payload)?;
        if h.cookie != self.cookie {
            return Err(ProtocolError::Cookie.into());
        }
        if h.pid != pid {
            return Err(ProtocolError::TargetPid.into());
        }
        if h.uid != uid {
            return Err(ProtocolError::Credentials.into());
        }
        if h.capabilities & self.config.capabilities != self.config.capabilities {
            return Err(ProtocolError::Credentials.into());
        }
        let granted = h.capabilities & self.config.supported_capabilities;
        // A session with no usable frame transport can never present anything,
        // so reject it here instead of accepting a peer whose every frame the
        // backend would discard.
        if self.config.required_transports != 0 && granted & self.config.required_transports == 0 {
            return Err(ProtocolError::Credentials.into());
        }
        let welcome = Packet::new(
            MessageKind::Welcome,
            hello.header.request_id,
            granted.to_le_bytes().to_vec(),
            Vec::new(),
        )?;
        conn.send(&welcome)?;
        // Handshake deadline must not leak into the quiet session.
        conn.set_read_timeout(Duration::ZERO)?;
        Ok(HostSession {
            peer_pid: pid,
            capabilities: granted,
            conn,
            targets: HashSet::new(),
            closed: false,
            capture: false,
        })
    }
}

impl HostConfig {
    fn accepts_pid(&self, pid: u32) -> bool {
        if self.expected_pid != 0 {
            return pid == self.expected_pid;
        }
        self.expected_process_group.is_some_and(|expected| {
            let process_group = unsafe { libc::getpgid(pid.cast_signed()) };
            process_group >= 0 && process_group.cast_unsigned() == expected
        })
    }
}

#[derive(Debug)]
pub enum HostFrameDescriptor {
    Dmabuf(DmabufDescriptor),
    Shm(TextureDescriptor),
}
#[derive(Debug)]
pub struct HostFrame {
    pub descriptor: HostFrameDescriptor,
    pub fds: Vec<OwnedFd>,
}
#[derive(Debug)]
pub enum HostEvent {
    TargetCreated(Target),
    TargetDestroyed(u64),
    /// The agent chose which target it composites into. This is the only
    /// authority for target selection; the host must not infer one from
    /// creation order.
    TargetSelected(SelectedTarget),
    /// The target presented and is ready for one newer offscreen frame. This is
    /// demand only, not a release acknowledgement, and may be coalesced.
    FrameRequested(FrameRequest),
    Frame(HostFrame),
    Consumed(FrameConsumed),
    Cleared(u64),
    Input(InputEvent),
    End(u64),
    Pong(u64),
    Disconnected,
}

pub struct HostSession {
    peer_pid: u32,
    capabilities: u64,
    conn: Seqpacket,
    targets: HashSet<u64>,
    closed: bool,
    capture: bool,
}
impl HostSession {
    pub fn set_nonblocking(&self) -> Result<(), HostError> {
        self.conn.set_nonblocking().map_err(HostError::from)
    }
}
impl HostSession {
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.conn.as_raw_fd()
    }
    pub fn peer_pid(&self) -> u32 {
        self.peer_pid
    }
    /// Frame transports and optional demand-pacing capabilities both sides
    /// agreed on for this session.
    pub fn capabilities(&self) -> u64 {
        self.capabilities
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn ensure_target(&mut self, target: Target) -> Result<(), HostError> {
        if self.targets.contains(&target.target_id) {
            return Ok(());
        }
        self.create_target(target)
    }
    pub fn create_target(&mut self, target: Target) -> Result<(), HostError> {
        if self.closed {
            return Err(HostError::Closed);
        }
        if self.targets.contains(&target.target_id) {
            return Err(HostError::TargetExists);
        }
        let p = Packet::new(
            MessageKind::TargetCreate,
            0,
            target.encode().to_vec(),
            Vec::new(),
        )?;
        self.send_packet(&p)?;
        self.targets.insert(target.target_id);
        Ok(())
    }
    pub fn destroy_target(&mut self, id: u64) -> Result<(), HostError> {
        if self.closed {
            return Err(HostError::Closed);
        }
        if !self.targets.contains(&id) {
            return Err(HostError::UnknownTarget);
        }
        let p = Packet::new(
            MessageKind::TargetDestroy,
            0,
            id.to_le_bytes().to_vec(),
            Vec::new(),
        )?;
        self.send_packet(&p)?;
        self.targets.remove(&id);
        Ok(())
    }
    pub fn set_visible(&mut self, visible: bool) -> Result<(), HostError> {
        self.send_bool(MessageKind::Visibility, visible)
    }
    pub fn set_input_capture(&mut self, capture: bool) -> Result<(), HostError> {
        self.send_bool(MessageKind::CaptureInput, capture)?;
        self.capture = capture;
        Ok(())
    }
    pub fn ping(&mut self, request_id: u64) -> Result<(), HostError> {
        let packet = Packet::new(MessageKind::Ping, request_id, Vec::new(), Vec::new())?;
        self.send_packet(&packet)
    }
    pub fn send_dmabuf(
        &mut self,
        target_id: u64,
        mut descriptor: DmabufDescriptor,
        fds: Vec<OwnedFd>,
    ) -> Result<(), HostError> {
        if !self.targets.contains(&target_id) {
            return Err(HostError::UnknownTarget);
        }
        descriptor.target_id = target_id;
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
    /// Send one validated RGBA SHM descriptor. The FD is consumed by the packet.
    pub fn send_shm(
        &mut self,
        target_id: u64,
        descriptor: TextureDescriptor,
        fd: OwnedFd,
    ) -> Result<(), HostError> {
        if !self.targets.contains(&target_id) {
            return Err(HostError::UnknownTarget);
        }
        let d = TextureDescriptor {
            target_id,
            ..descriptor
        };
        let p = Packet::new(MessageKind::Shm, 0, d.encode()?.to_vec(), vec![fd])?;
        self.send_packet(&p)
    }
    pub fn clear(&mut self, target_id: u64) -> Result<(), HostError> {
        let p = Packet::new(
            MessageKind::Clear,
            0,
            target_id.to_le_bytes().to_vec(),
            Vec::new(),
        )?;
        self.send_packet(&p)
    }
    pub fn recv(&mut self) -> Result<HostEvent, HostError> {
        if self.closed {
            return Err(HostError::Closed);
        }
        match self.conn.recv() {
            Ok(packet) => self.decode_event(packet),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                Err(ProtocolError::Io(error).into())
            }
            Err(error) => {
                self.closed = true;
                self.capture = false;
                Err(error.into())
            }
        }
    }
    pub fn shutdown(&mut self) -> Result<(), HostError> {
        if self.closed {
            return Ok(());
        }
        let p = Packet::new(MessageKind::Shutdown, 0, Vec::new(), Vec::new())?;
        let result = self.send_packet(&p);
        self.closed = true;
        self.capture = false;
        result
    }
    pub fn capture_active(&self) -> bool {
        self.capture
    }
    fn send_bool(&mut self, kind: MessageKind, value: bool) -> Result<(), HostError> {
        let p = Packet::new(kind, 0, BoolValue(value).encode().to_vec(), Vec::new())?;
        self.send_packet(&p)
    }
    fn send_packet(&mut self, packet: &Packet) -> Result<(), HostError> {
        if self.closed {
            return Err(HostError::Closed);
        }
        match self.conn.send(packet) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A failed send means the peer is no longer a usable session.
                // Close and clear state before exposing the terminal error.
                self.closed = true;
                self.capture = false;
                Err(error.into())
            }
        }
    }
    fn decode_event(&mut self, p: Packet) -> Result<HostEvent, HostError> {
        match p.header.kind {
            MessageKind::TargetCreate => {
                let t = Target::decode(&p.payload)?;
                self.targets.insert(t.target_id);
                Ok(HostEvent::TargetCreated(t))
            }
            MessageKind::TargetDestroy => {
                if p.payload.len() != 8 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                let id = u64::from_le_bytes(p.payload.try_into().unwrap());
                self.targets.remove(&id);
                Ok(HostEvent::TargetDestroyed(id))
            }
            MessageKind::SelectTarget => {
                let selection = SelectedTarget::decode(&p.payload)?;
                // Zero clears the selection; anything else must name a target
                // the agent has already registered.
                if selection.target_id != 0 && !self.targets.contains(&selection.target_id) {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                Ok(HostEvent::TargetSelected(selection))
            }
            MessageKind::FrameRequest => {
                if self.capabilities & CAP_FRAME_REQUEST == 0 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                let request = FrameRequest::decode(&p.payload)?;
                if p.header.request_id != request.sequence
                    || !self.targets.contains(&request.target_id)
                {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                Ok(HostEvent::FrameRequested(request))
            }
            MessageKind::Dmabuf => {
                let descriptor = DmabufDescriptor::decode(&p.payload)?;
                if p.fds.len() != descriptor.planes.len() {
                    return Err(ProtocolError::FdCount {
                        expected: descriptor.planes.len(),
                        actual: p.fds.len(),
                    }
                    .into());
                }
                Ok(HostEvent::Frame(HostFrame {
                    descriptor: HostFrameDescriptor::Dmabuf(descriptor),
                    fds: p.fds,
                }))
            }
            MessageKind::Shm => {
                let descriptor = TextureDescriptor::decode(&p.payload)?;
                Ok(HostEvent::Frame(HostFrame {
                    descriptor: HostFrameDescriptor::Shm(descriptor),
                    fds: p.fds,
                }))
            }
            MessageKind::Consumed => Ok(HostEvent::Consumed(FrameConsumed::decode(&p.payload)?)),
            MessageKind::Clear => {
                if p.payload.len() != 8 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                Ok(HostEvent::Cleared(u64::from_le_bytes(
                    p.payload.try_into().unwrap(),
                )))
            }
            MessageKind::Input => Ok(HostEvent::Input(InputEvent::decode(&p.payload)?)),
            MessageKind::End => {
                if p.payload.len() != 8 {
                    return Err(ProtocolError::InvalidPayload.into());
                }
                Ok(HostEvent::End(u64::from_le_bytes(
                    p.payload.try_into().unwrap(),
                )))
            }
            MessageKind::Pong if p.payload.is_empty() => Ok(HostEvent::Pong(p.header.request_id)),
            _ => Err(ProtocolError::InvalidPayload.into()),
        }
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        self.closed = true;
        self.capture = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn successful_handshake_and_target_lifecycle() {
        let pid = unsafe { libc::getpid() } as u32;
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let t = thread::spawn(move || {
            let c = ep.connect().unwrap();
            let h = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                capabilities: u64::MAX,
            };
            c.send(&Packet::new(MessageKind::Hello, 1, h.encode().to_vec(), Vec::new()).unwrap())
                .unwrap();
            c.recv().unwrap();
            c.recv().unwrap();
            c.recv().unwrap();
        });
        let mut s = host.accept().unwrap();
        s.create_target(Target {
            target_id: 7,
            width: 1,
            height: 1,
        })
        .unwrap();
        s.send_shm(
            7,
            TextureDescriptor {
                target_id: 7,
                width: 1,
                height: 1,
                stride: 4,
                offset: 0,
                size: 4,
                format: 0,
            },
            std::fs::File::open("/dev/null").unwrap().into(),
        )
        .unwrap();
        assert!(!s.is_closed());
        t.join().unwrap();
    }

    #[test]
    fn process_group_handshake_accepts_the_authenticated_member() {
        let pid = unsafe { libc::getpid() }.cast_unsigned();
        let process_group = unsafe { libc::getpgid(0) }.cast_unsigned();
        let host = Host::with_config(HostConfig::for_process_group(process_group)).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let client = thread::spawn(move || {
            let connection = ep.connect().unwrap();
            let hello = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                capabilities: u64::MAX,
            };
            connection
                .send(
                    &Packet::new(MessageKind::Hello, 1, hello.encode().to_vec(), Vec::new())
                        .unwrap(),
                )
                .unwrap();
            connection.recv().unwrap();
        });
        let session = host.accept().unwrap();
        assert_eq!(session.peer_pid(), pid);
        client.join().unwrap();
    }

    #[test]
    fn granted_capabilities_are_the_intersection_with_the_target() {
        let pid = unsafe { libc::getpid() }.cast_unsigned();
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let client = thread::spawn(move || {
            let connection = ep.connect().unwrap();
            let hello = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                // A GLX target: shared memory only, plus an unknown future bit.
                capabilities: CAP_SHM | CAP_SHM_BGRA | 1 << 40,
            };
            connection
                .send(
                    &Packet::new(MessageKind::Hello, 1, hello.encode().to_vec(), Vec::new())
                        .unwrap(),
                )
                .unwrap();
            let welcome = connection.recv().unwrap();
            u64::from_le_bytes(welcome.payload.as_slice().try_into().unwrap())
        });
        let session = host.accept().unwrap();
        assert_eq!(session.capabilities(), CAP_SHM | CAP_SHM_BGRA);
        assert_eq!(session.capabilities() & CAP_DMABUF, 0);
        assert_eq!(client.join().unwrap(), CAP_SHM | CAP_SHM_BGRA);
    }

    #[test]
    fn frame_request_is_negotiated_and_requires_a_registered_target() {
        let pid = unsafe { libc::getpid() }.cast_unsigned();
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let client = thread::spawn(move || {
            let connection = ep.connect().unwrap();
            let hello = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                capabilities: CAP_SHM | CAP_FRAME_REQUEST,
            };
            connection
                .send(
                    &Packet::new(MessageKind::Hello, 1, hello.encode().to_vec(), Vec::new())
                        .unwrap(),
                )
                .unwrap();
            let welcome = connection.recv().unwrap();
            assert_ne!(
                u64::from_le_bytes(welcome.payload.as_slice().try_into().unwrap())
                    & CAP_FRAME_REQUEST,
                0,
            );
            connection
                .send(
                    &Packet::new(
                        MessageKind::TargetCreate,
                        0,
                        Target {
                            target_id: 7,
                            width: 640,
                            height: 480,
                        }
                        .encode()
                        .to_vec(),
                        Vec::new(),
                    )
                    .unwrap(),
                )
                .unwrap();
            let request = FrameRequest {
                target_id: 7,
                sequence: 3,
            };
            connection
                .send(
                    &Packet::new(
                        MessageKind::FrameRequest,
                        request.sequence,
                        request.encode().unwrap().to_vec(),
                        Vec::new(),
                    )
                    .unwrap(),
                )
                .unwrap();
        });
        let mut session = host.accept().unwrap();
        assert!(matches!(
            session.recv().unwrap(),
            HostEvent::TargetCreated(Target { target_id: 7, .. })
        ));
        assert!(matches!(
            session.recv().unwrap(),
            HostEvent::FrameRequested(FrameRequest {
                target_id: 7,
                sequence: 3
            })
        ));
        client.join().unwrap();
    }

    #[test]
    fn a_dmabuf_only_target_negotiates_a_zero_copy_session() {
        let pid = unsafe { libc::getpid() }.cast_unsigned();
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let client = thread::spawn(move || {
            let connection = ep.connect().unwrap();
            let hello = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                capabilities: CAP_DMABUF,
            };
            connection
                .send(
                    &Packet::new(MessageKind::Hello, 1, hello.encode().to_vec(), Vec::new())
                        .unwrap(),
                )
                .unwrap();
            let welcome = connection.recv().unwrap();
            u64::from_le_bytes(welcome.payload.as_slice().try_into().unwrap())
        });
        let session = host.accept().unwrap();
        assert_eq!(session.capabilities(), CAP_DMABUF);
        assert_eq!(session.capabilities() & CAP_SHM, 0);
        assert_eq!(client.join().unwrap(), CAP_DMABUF);
    }

    #[test]
    fn a_target_with_no_usable_frame_transport_is_rejected() {
        let pid = unsafe { libc::getpid() }.cast_unsigned();
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let client = thread::spawn(move || {
            let connection = ep.connect().unwrap();
            let hello = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                // Colour ordering and demand pacing carry no pixels.
                capabilities: CAP_SHM_BGRA | CAP_FRAME_REQUEST,
            };
            connection
                .send(
                    &Packet::new(MessageKind::Hello, 1, hello.encode().to_vec(), Vec::new())
                        .unwrap(),
                )
                .unwrap();
            let _ = connection.recv();
        });
        assert!(host.accept().is_err());
        client.join().unwrap();
    }

    #[test]
    fn bad_cookie_is_rejected() {
        let pid = unsafe { libc::getpid() } as u32;
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let t = thread::spawn(move || {
            let c = ep.connect().unwrap();
            let p = Packet::new(
                MessageKind::Hello,
                0,
                Hello {
                    cookie: [1; 32],
                    pid,
                    uid: unsafe { libc::geteuid() },
                    capabilities: u64::MAX,
                }
                .encode()
                .to_vec(),
                Vec::new(),
            )
            .unwrap();
            let _ = c.send(&p);
        });
        assert!(matches!(
            host.accept(),
            Err(HostError::Protocol(ProtocolError::Cookie))
        ));
        t.join().unwrap();
    }

    #[test]
    fn wrong_pid_is_rejected() {
        let pid = unsafe { libc::getpid() } as u32;
        let mut cfg = HostConfig::for_pid(pid);
        cfg.expected_pid = pid.wrapping_add(1);
        let host = Host::with_config(cfg).unwrap();
        let ep = host.endpoint().clone();
        let t = thread::spawn(move || {
            let _ = ep.connect();
        });
        assert!(matches!(
            host.accept(),
            Err(HostError::Protocol(ProtocolError::TargetPid))
        ));
        t.join().unwrap();
    }

    #[test]
    fn failed_target_send_closes_session_without_publishing_target() {
        let pid = unsafe { libc::getpid() } as u32;
        let host = Host::new(pid).unwrap();
        let ep = host.endpoint().clone();
        let cookie = host.cookie();
        let t = thread::spawn(move || {
            let c = ep.connect().unwrap();
            let h = Hello {
                cookie,
                pid,
                uid: unsafe { libc::geteuid() },
                capabilities: u64::MAX,
            };
            c.send(&Packet::new(MessageKind::Hello, 1, h.encode().to_vec(), Vec::new()).unwrap())
                .unwrap();
            c.recv().unwrap();
            drop(c);
        });
        let mut session = host.accept().unwrap();
        t.join().unwrap();
        let target = Target {
            target_id: 99,
            width: 1,
            height: 1,
        };
        assert!(session.create_target(target).is_err());
        assert!(session.is_closed());
        assert!(!session.targets.contains(&target.target_id));
    }
}
