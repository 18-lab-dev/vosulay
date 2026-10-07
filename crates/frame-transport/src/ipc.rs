//! Nonblocking SOCK_SEQPACKET control plane with same-UID peer authentication.
use crate::protocol::{Kind, PACKET_SIZE, Packet, invalid};
use std::{
    ffi::CString,
    io,
    mem::{ManuallyDrop, size_of, zeroed},
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    ptr::null_mut,
};

const CONTROL_BYTES: usize = 64;
#[repr(C)]
union Control {
    alignment: ManuallyDrop<libc::cmsghdr>,
    bytes: [u8; CONTROL_BYTES],
}
impl Control {
    fn new() -> Self {
        Self {
            bytes: [0; CONTROL_BYTES],
        }
    }
    fn pointer(&mut self) -> *mut u8 {
        (self as *mut Self).cast()
    }
}

pub struct Received {
    pub packet: Packet,
    handles: [Option<OwnedFd>; 1],
}
impl Received {
    pub fn take_handle(&mut self, index: usize) -> Option<OwnedFd> {
        self.handles.get_mut(index).and_then(Option::take)
    }
}

pub struct Connection {
    fd: OwnedFd,
}
impl AsRawFd for Connection {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
impl Connection {
    pub fn connect(endpoint: &str) -> io::Result<Self> {
        let fd = socket()?;
        let address = Address::new(endpoint)?;
        // SAFETY: address is initialized through length, fd is a live socket.
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address.addr as *const libc::sockaddr_un).cast(),
                address.length,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        authenticate(fd.as_raw_fd())?;
        Ok(Self { fd })
    }
    pub fn pair() -> io::Result<(Self, Self)> {
        let mut descriptors = [-1; 2];
        // SAFETY: storage has room for both newly created descriptors.
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful socketpair created two distinct owned descriptors.
        let first = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let second = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        authenticate(first.as_raw_fd())?;
        authenticate(second.as_raw_fd())?;
        Ok((Self { fd: first }, Self { fd: second }))
    }
    pub fn peer_pid(&self) -> io::Result<u32> {
        Ok(credentials(self.as_raw_fd())?.pid as u32)
    }
    pub fn send(&self, packet: &Packet, descriptors: &[BorrowedFd<'_>]) -> io::Result<()> {
        let required = if packet.kind == Kind::Ring { 1 } else { 0 };
        if descriptors.len() != required {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid control descriptor count",
            ));
        }
        let bytes = packet.encode();
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr() as *mut _,
            iov_len: bytes.len(),
        };
        let mut control = Control::new();
        let control_length = if descriptors.is_empty() {
            0
        } else {
            let payload = (descriptors.len() * size_of::<RawFd>()) as u32;
            // SAFETY: constant bounded payload and aligned ancillary storage.
            unsafe {
                let header = control.pointer().cast::<libc::cmsghdr>();
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(payload) as usize;
                let data = libc::CMSG_DATA(header).cast::<RawFd>();
                for (index, descriptor) in descriptors.iter().enumerate() {
                    data.add(index).write(descriptor.as_raw_fd());
                }
                libc::CMSG_SPACE(payload) as usize
            }
        };
        let message = libc::msghdr {
            msg_name: null_mut(),
            msg_namelen: 0,
            msg_iov: &mut iov,
            msg_iovlen: 1,
            msg_control: if control_length == 0 {
                null_mut()
            } else {
                control.pointer().cast()
            },
            msg_controllen: control_length,
            msg_flags: 0,
        };
        // SAFETY: both buffers remain live; sendmsg does not retain pointers.
        let sent = unsafe {
            libc::sendmsg(
                self.as_raw_fd(),
                &message,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        if sent as usize != PACKET_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "partial control record",
            ));
        }
        Ok(())
    }
    pub fn recv(&self) -> io::Result<Option<Received>> {
        let mut bytes = [0; PACKET_SIZE];
        let mut control = Control::new();
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        // SAFETY: zero-initialized msghdr has its used pointers assigned below.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.pointer().cast();
        message.msg_controllen = CONTROL_BYTES;
        // SAFETY: output buffers are writable for the declared bounds.
        let count = unsafe {
            libc::recvmsg(
                self.as_raw_fd(),
                &mut message,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "overlay peer closed",
            ));
        }
        let mut handles: [Option<OwnedFd>; 1] = [None];
        let mut handle_count = 0;
        let mut malformed = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || message.msg_controllen > CONTROL_BYTES;
        let length = message.msg_controllen.min(CONTROL_BYTES);
        // SAFETY: zero payload length is valid for the platform CMSG API.
        let header_bytes = unsafe { libc::CMSG_LEN(0) } as usize;
        let mut offset = 0;
        while offset < length {
            if length - offset < header_bytes {
                malformed = true;
                break;
            }
            // SAFETY: offset lies within the kernel-reported bounded buffer.
            let pointer = unsafe { control.pointer().add(offset).cast::<libc::cmsghdr>() };
            let header = unsafe { pointer.read_unaligned() };
            let message_bytes = header.cmsg_len;
            if message_bytes < header_bytes || message_bytes > length - offset {
                malformed = true;
                break;
            }
            if header.cmsg_level != libc::SOL_SOCKET || header.cmsg_type != libc::SCM_RIGHTS {
                malformed = true;
            } else {
                let payload_bytes = message_bytes - header_bytes;
                if !payload_bytes.is_multiple_of(size_of::<RawFd>()) {
                    malformed = true;
                }
                // SAFETY: header extent was validated; payload contains complete fd words.
                let data = unsafe { libc::CMSG_DATA(pointer).cast::<RawFd>() };
                for index in 0..payload_bytes / size_of::<RawFd>() {
                    let raw = unsafe { data.add(index).read_unaligned() };
                    // SAFETY: each SCM_RIGHTS word is a new kernel-installed fd.
                    let handle = unsafe { OwnedFd::from_raw_fd(raw) };
                    if handle_count < handles.len() {
                        handles[handle_count] = Some(handle);
                        handle_count += 1;
                    } else {
                        drop(handle);
                        malformed = true;
                    }
                }
            }
            let alignment = size_of::<usize>();
            let aligned = (message_bytes + alignment - 1) & !(alignment - 1);
            if aligned > length - offset {
                if message_bytes != length - offset {
                    malformed = true;
                }
                break;
            }
            offset += aligned;
        }
        if malformed || count as usize != PACKET_SIZE {
            return Err(invalid("truncated overlay control record"));
        }
        let packet = Packet::decode(&bytes)?;
        if handle_count != if packet.kind == Kind::Ring { 1 } else { 0 } {
            return Err(invalid("invalid received descriptor count"));
        }
        Ok(Some(Received { packet, handles }))
    }
}

fn socket() -> io::Result<OwnedFd> {
    // SAFETY: socket has no pointer arguments and returns a new descriptor.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful socket returned one owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn credentials(fd: RawFd) -> io::Result<libc::ucred> {
    // SAFETY: writable output and exact requested SO_PEERCRED size.
    let mut credentials: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<libc::ucred>() || credentials.pid <= 0 {
        return Err(invalid("invalid overlay peer credentials"));
    }
    Ok(credentials)
}
fn authenticate(fd: RawFd) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if credentials(fd)?.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "overlay peer UID mismatch",
        ));
    }
    Ok(())
}
struct Address {
    addr: libc::sockaddr_un,
    length: libc::socklen_t,
    path: Option<CString>,
}
impl Address {
    fn new(endpoint: &str) -> io::Result<Self> {
        // SAFETY: zero initialization gives a correctly terminated UNIX address.
        let mut addr: libc::sockaddr_un = unsafe { zeroed() };
        addr.sun_family = libc::AF_UNIX as _;
        let (name, abstract_name) = if let Some(name) = endpoint.strip_prefix('@') {
            (name, true)
        } else {
            (endpoint, false)
        };
        if name.is_empty()
            || name.as_bytes().contains(&0)
            || name.len() + 1 > addr.sun_path.len()
            || (!abstract_name && !name.starts_with('/'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid overlay socket name",
            ));
        }
        let start = usize::from(abstract_name);
        for (index, byte) in name.as_bytes().iter().enumerate() {
            addr.sun_path[start + index] = *byte as _;
        }
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1) as libc::socklen_t;
        let path = if abstract_name {
            None
        } else {
            Some(CString::new(name).expect("checked NUL"))
        };
        Ok(Self { addr, length, path })
    }
}
struct BoundPath(Option<CString>);
impl Drop for BoundPath {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            // SAFETY: this is the path successfully bound by this listener only.
            unsafe {
                libc::unlink(path.as_ptr());
            }
        }
    }
}
pub struct Listener {
    fd: OwnedFd,
    _path: BoundPath,
}
impl AsRawFd for Listener {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
impl Listener {
    pub fn bind(endpoint: &str) -> io::Result<Self> {
        let address = Address::new(endpoint)?;
        let fd = socket()?;
        // SAFETY: bounded initialized sockaddr and a live socket.
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&address.addr as *const libc::sockaddr_un).cast(),
                address.length,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let path = BoundPath(address.path);
        if let Some(path) = &path.0 {
            // SAFETY: only adjust the newly bound socket, never an existing path.
            if unsafe { libc::chmod(path.as_ptr(), 0o600) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: fd is bound; a small backlog bounds pending connections.
        if unsafe { libc::listen(fd.as_raw_fd(), 16) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, _path: path })
    }
    pub fn accept(&self) -> io::Result<Option<Connection>> {
        // SAFETY: no address output requested; atomically NONBLOCK and CLOEXEC.
        let raw = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                null_mut(),
                null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        if raw < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: successful accept4 returned a new descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        authenticate(fd.as_raw_fd())?;
        Ok(Some(Connection { fd }))
    }
}

pub fn default_endpoint() -> String {
    // SAFETY: geteuid has no preconditions. Abstract sockets vanish on exit.
    format!("@vosulay-{}", unsafe { libc::geteuid() })
}
