//! Explicit live attachment. This crate never attaches automatically.
//! Linux support is deliberately narrow: same-UID native x86_64 processes.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::ffi::c_void;
use std::ffi::{CStr, OsStr};
use std::fmt;
use std::io;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::path::{Path, PathBuf};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::time::{Duration, Instant};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CALL_TIMEOUT: Duration = Duration::from_millis(800);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MAX_LIBRARY: usize = 4096;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MAX_ENDPOINT: usize = 4096;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MAX_SYMBOL: usize = 128;

#[derive(Debug)]
pub enum AttachError {
    Io(io::Error),
    Unsupported(&'static str),
    NotAuthorized,
    IdentityChanged,
    Exited,
    Timeout,
    InvalidLibrary,
    InvalidEndpoint,
    InvalidStack,
    RemoteLoader,
    RemoteInit,
    SymbolIdentity,
    UnexpectedStop(i32),
    Ptrace(&'static str, io::Error),
}

impl From<io::Error> for AttachError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for AttachError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(output, "attachment I/O failed: {error}"),
            Self::Unsupported(reason) => output.write_str(reason),
            Self::NotAuthorized => output.write_str("target is owned by another user"),
            Self::IdentityChanged => output.write_str("target process identity changed"),
            Self::Exited => output.write_str("target exited during attachment"),
            Self::Timeout => output.write_str("target did not stop within the attachment deadline"),
            Self::InvalidLibrary => {
                output.write_str("hook library path is invalid in the target namespace")
            }
            Self::InvalidEndpoint => output.write_str("overlay endpoint is invalid"),
            Self::InvalidStack => {
                output.write_str("target stack cannot safely hold the remote call")
            }
            Self::RemoteLoader => output.write_str("target dlopen failed"),
            Self::RemoteInit => output.write_str("hook initialization failed in the target"),
            Self::SymbolIdentity => {
                output.write_str("target loader symbols do not match this process")
            }
            Self::UnexpectedStop(signal) => {
                write!(output, "target stopped with unexpected signal {signal}")
            }
            Self::Ptrace(operation, error) => write!(output, "ptrace {operation} failed: {error}"),
        }
    }
}

impl std::error::Error for AttachError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Ptrace(_, error) => Some(error),
            _ => None,
        }
    }
}

pub const ATTACH_EXPORT: &CStr = c"vosulay_attach_v1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
    pub uid: u32,
}

pub fn process_identity(pid: u32) -> Result<ProcessIdentity, AttachError> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .ok_or(AttachError::Exited)?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let command_end = stat.rfind(')').ok_or(AttachError::Exited)?;
    // starttime is field 22; fields after the command begin with field 3.
    let start_time = stat[command_end + 2..]
        .split_whitespace()
        .nth(19)
        .ok_or(AttachError::Exited)?
        .parse()
        .map_err(|_| AttachError::Exited)?;
    Ok(ProcessIdentity {
        pid,
        start_time,
        uid,
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn attach(
    pid: u32,
    library: impl AsRef<OsStr>,
    endpoint: &str,
) -> Result<ProcessIdentity, AttachError> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let path = Path::new(&library);
    let path_text = path.to_str().ok_or(AttachError::InvalidLibrary)?;
    if !path.is_absolute()
        || !path.is_file()
        || path_text.len() > MAX_LIBRARY
        || path_text.as_bytes().contains(&0)
    {
        return Err(AttachError::InvalidLibrary);
    }
    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT || endpoint.as_bytes().contains(&0) {
        return Err(AttachError::InvalidEndpoint);
    }

    let identity = process_identity(pid)?;
    let canonical = std::fs::canonicalize(path).map_err(|_| AttachError::InvalidLibrary)?;
    let relative = canonical
        .strip_prefix("/")
        .map_err(|_| AttachError::InvalidLibrary)?;
    let target_path = Path::new(&format!("/proc/{pid}/root")).join(relative);
    let local_metadata = std::fs::metadata(&canonical).map_err(|_| AttachError::InvalidLibrary)?;
    let target_metadata =
        std::fs::metadata(target_path).map_err(|_| AttachError::InvalidLibrary)?;
    if local_metadata.dev() != target_metadata.dev()
        || local_metadata.ino() != target_metadata.ino()
    {
        return Err(AttachError::InvalidLibrary);
    }
    let mut header = [0u8; 20];
    std::fs::File::open(format!("/proc/{pid}/exe"))?.read_exact(&mut header)?;
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || u16::from_le_bytes([header[18], header[19]]) != 62
    {
        return Err(AttachError::Unsupported(
            "live attachment requires a native x86_64 ELF process",
        ));
    }
    // SAFETY: geteuid has no preconditions and does not return borrowed state.
    let effective_uid = unsafe { libc::geteuid() as u32 };
    if identity.uid != effective_uid {
        return Err(AttachError::NotAuthorized);
    }

    let pidfd = PidFd::open(pid)?;
    pidfd.alive()?;
    let mut remote = Remote::seize(pid, identity, pidfd)?;
    let result = remote.run(
        canonical.to_str().ok_or(AttachError::InvalidLibrary)?,
        endpoint,
    );
    let cleanup = remote.finish();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(identity),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub fn attach(
    _pid: u32,
    _library: impl AsRef<OsStr>,
    _endpoint: &str,
) -> Result<ProcessIdentity, AttachError> {
    Err(AttachError::Unsupported(
        "vosulay attach requires Linux x86_64 SysV ABI; this target is unsupported",
    ))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct PidFd(libc::c_int);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl PidFd {
    fn open(pid: u32) -> Result<Self, AttachError> {
        // SAFETY: pidfd_open takes scalar arguments; a successful descriptor is owned here.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
        if descriptor < 0 {
            Err(io::Error::last_os_error().into())
        } else {
            Ok(Self(descriptor))
        }
    }

    fn alive(&self) -> Result<(), AttachError> {
        let mut descriptor = libc::pollfd {
            fd: self.0,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if result < 0 {
            Err(io::Error::last_os_error().into())
        } else if descriptor.revents != 0 {
            Err(AttachError::Exited)
        } else {
            Ok(())
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for PidFd {
    fn drop(&mut self) {
        // SAFETY: PidFd exclusively owns this valid descriptor.
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct Remote {
    pid: libc::pid_t,
    identity: ProcessIdentity,
    _pidfd: PidFd,
    original_registers: libc::user_regs_struct,
    extended_registers: Vec<u8>,
    registers_valid: bool,
    stopped: bool,
    seized: bool,
    detach_signal: i32,
    saved_words: Vec<(u64, u64)>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Remote {
    fn seize(pid: u32, identity: ProcessIdentity, pidfd: PidFd) -> Result<Self, AttachError> {
        let pid = pid as libc::pid_t;
        let mut remote = Self {
            pid,
            identity,
            _pidfd: pidfd,
            // SAFETY: Linux user_regs_struct is a plain integer register record.
            original_registers: unsafe { std::mem::zeroed() },
            extended_registers: Vec::new(),
            registers_valid: false,
            stopped: false,
            seized: false,
            detach_signal: 0,
            saved_words: Vec::new(),
        };
        // PTRACE_SEIZE is non-stopping. EXITKILL is intentionally absent: an
        // injector crash must not kill the explicitly selected target.
        ptrace(libc::PTRACE_SEIZE, pid, 0, 0)?;
        remote.seized = true;
        ptrace(libc::PTRACE_INTERRUPT, pid, 0, 0)?;
        let signal = wait_stopped(pid, CALL_TIMEOUT)?;
        remote.stopped = true;
        if signal != libc::SIGTRAP {
            remote.detach_signal = signal;
            return Err(AttachError::UnexpectedStop(signal));
        }
        ptrace_getregs(pid, &mut remote.original_registers)?;
        remote.registers_valid = true;
        let mut extended = vec![0; 64 * 1024];
        let mut state = libc::iovec {
            iov_base: extended.as_mut_ptr().cast(),
            iov_len: extended.len(),
        };
        // Preserve x87/SSE/AVX and enabled extended state before calling into
        // the loader. An unsupported regset fails before modifying the target.
        ptrace(
            libc::PTRACE_GETREGSET,
            pid,
            0x202,
            std::ptr::from_mut(&mut state) as usize,
        )?;
        extended.truncate(state.iov_len);
        remote.extended_registers = extended;
        Ok(remote)
    }

    fn run(&mut self, library: &str, endpoint: &str) -> Result<(), AttachError> {
        self.check_identity()?;
        let stack = stack_mapping(self.pid as u32, self.original_registers.rsp)?;
        // The remote function returns to an unmapped sentinel address. Ptrace
        // consumes that exact SIGSEGV before delivery to the application; no
        // shared executable bytes are patched, even temporarily.
        let call_rsp = (self.original_registers.rsp.saturating_sub(0x1000) & !15) + 8;
        let payload = self.original_registers.rsp.saturating_sub(0xa000) & !7;
        let library_at = payload + 0x100;
        let endpoint_at = payload + 0x2200;
        let symbol_at = payload + 0x4400;
        let payload_end = symbol_at + MAX_SYMBOL as u64 + 8;
        if payload < stack.start
            || payload_end > call_rsp.saturating_sub(0x1000)
            || call_rsp < stack.start
            || call_rsp + 8 > stack.end
            || !stack.writable
        {
            return Err(AttachError::InvalidStack);
        }

        let trap = 0;
        if maps(self.pid as u32)
            .iter()
            .any(|mapping| mapping.start == trap)
        {
            return Err(AttachError::Unsupported(
                "live attachment requires an unmapped null page",
            ));
        }

        self.write_cstr(library_at, library, MAX_LIBRARY)?;
        self.write_cstr(endpoint_at, endpoint, MAX_ENDPOINT)?;
        self.write_cstr(
            symbol_at,
            ATTACH_EXPORT
                .to_str()
                .map_err(|_| AttachError::RemoteInit)?,
            MAX_SYMBOL,
        )?;

        let dlopen = remote_symbol(self.pid, libc::dlopen as *const c_void)?;
        let handle = self.call(
            dlopen as u64,
            [
                library_at,
                (libc::RTLD_NOW | libc::RTLD_GLOBAL) as u64,
                0,
                0,
                0,
                0,
            ],
            trap,
            call_rsp,
        )?;
        if handle == 0 {
            return Err(AttachError::RemoteLoader);
        }

        let dlsym = remote_symbol(self.pid, libc::dlsym as *const c_void)?;
        let initializer = self.call(
            dlsym as u64,
            [handle, symbol_at, 0, 0, 0, 0],
            trap,
            call_rsp,
        )?;
        if initializer == 0 {
            return Err(AttachError::RemoteInit);
        }
        let status = self.call(initializer, [endpoint_at, 0, 0, 0, 0, 0], trap, call_rsp)?;
        if status == 0 {
            return Err(AttachError::RemoteInit);
        }
        Ok(())
    }

    fn call(
        &mut self,
        function: u64,
        arguments: [u64; 6],
        trap: u64,
        call_rsp: u64,
    ) -> Result<u64, AttachError> {
        self.write_word(call_rsp, trap)?;
        let mut registers = self.original_registers;
        // SysV function entry requires RSP % 16 == 8 and a readable return
        // address at [RSP]. The return sentinel is deliberately unmapped.
        registers.rsp = call_rsp;
        registers.rip = function;
        // An interrupted blocking syscall carries a restart code in RAX.
        // Disable kernel restart while executing our function, or resume may
        // rewind its entry RIP by two bytes. Original registers are retained
        // unchanged and restored at detach, including the interrupted syscall.
        registers.orig_rax = u64::MAX;
        registers.rax = 0;
        registers.rdi = arguments[0];
        registers.rsi = arguments[1];
        registers.rdx = arguments[2];
        registers.rcx = arguments[3];
        registers.r8 = arguments[4];
        registers.r9 = arguments[5];
        ptrace_setregs(self.pid, &registers)?;
        self.stopped = false;
        if let Err(error) = ptrace(libc::PTRACE_CONT, self.pid, 0, 0) {
            self.stopped = true;
            return Err(error);
        }

        let signal = match wait_stopped(self.pid, CALL_TIMEOUT) {
            Ok(signal) => {
                self.stopped = true;
                signal
            }
            Err(error) => {
                self.interrupt_for_cleanup()?;
                return Err(error);
            }
        };
        // SAFETY: the tracee is stopped and the kernel writes the complete register record.
        let mut output: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        ptrace_getregs(self.pid, &mut output)?;
        if signal != libc::SIGSEGV || output.rip != trap || output.rsp != call_rsp + 8 {
            self.detach_signal = signal;
            return Err(AttachError::UnexpectedStop(signal));
        }
        // Suppress only our exact return fault. Nested calls or genuine faults
        // cannot satisfy both the sentinel RIP and restored entry-stack shape.
        output.rip = self.original_registers.rip;
        ptrace_setregs(self.pid, &output)?;
        Ok(output.rax)
    }

    fn write_cstr(&mut self, address: u64, text: &str, maximum: usize) -> Result<(), AttachError> {
        if text.len() + 1 > maximum {
            return Err(AttachError::InvalidLibrary);
        }
        let bytes = text.as_bytes();
        let words = (bytes.len() + 1).div_ceil(8);
        for index in 0..words {
            let mut value = 0_u64;
            for offset in 0..8 {
                let position = index * 8 + offset;
                let byte = if position < bytes.len() {
                    bytes[position]
                } else {
                    0
                };
                value |= u64::from(byte) << (offset * 8);
            }
            self.write_word(address + (index * 8) as u64, value)?;
        }
        Ok(())
    }

    fn save_word(&mut self, address: u64) -> Result<u64, AttachError> {
        if let Some((_, value)) = self.saved_words.iter().find(|(saved, _)| *saved == address) {
            return Ok(*value);
        }
        let value = peek(self.pid, address)?;
        self.saved_words.push((address, value));
        Ok(value)
    }

    fn write_word(&mut self, address: u64, value: u64) -> Result<(), AttachError> {
        self.save_word(address)?;
        poke(self.pid, address, value)
    }

    fn check_identity(&self) -> Result<(), AttachError> {
        if process_identity(self.identity.pid)? != self.identity {
            Err(AttachError::IdentityChanged)
        } else {
            self._pidfd.alive()
        }
    }

    fn interrupt_for_cleanup(&mut self) -> Result<(), AttachError> {
        if self.stopped {
            return Ok(());
        }
        ptrace(libc::PTRACE_INTERRUPT, self.pid, 0, 0)?;
        let signal = wait_stopped(self.pid, CLEANUP_TIMEOUT)?;
        self.stopped = true;
        if signal != libc::SIGTRAP {
            self.detach_signal = signal;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AttachError> {
        if !self.seized {
            return Ok(());
        }
        self.interrupt_for_cleanup()?;
        for &(address, value) in self.saved_words.iter().rev() {
            poke(self.pid, address, value)?;
        }
        if !self.extended_registers.is_empty() {
            let mut state = libc::iovec {
                iov_base: self.extended_registers.as_mut_ptr().cast(),
                iov_len: self.extended_registers.len(),
            };
            ptrace(
                libc::PTRACE_SETREGSET,
                self.pid,
                0x202,
                std::ptr::from_mut(&mut state) as usize,
            )?;
        }
        if self.registers_valid {
            ptrace_setregs(self.pid, &self.original_registers)?;
        }
        let signal = if self.detach_signal == libc::SIGTRAP {
            0
        } else {
            self.detach_signal
        };
        ptrace(libc::PTRACE_DETACH, self.pid, 0, signal as usize)?;
        self.seized = false;
        self.stopped = false;
        self.saved_words.clear();
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for Remote {
    fn drop(&mut self) {
        if self.seized {
            // Best-effort is unavoidable in Drop, but finish itself is bounded
            // and never restores memory while the tracee may still be running.
            let _ = self.finish();
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ptrace(
    request: libc::c_uint,
    pid: libc::pid_t,
    address: u64,
    data: usize,
) -> Result<(), AttachError> {
    // SAFETY: ptrace interprets address/data according to request; callers use
    // scalar values or stopped-tracee addresses as required by that request.
    let result = unsafe { libc::ptrace(request, pid, address as *mut c_void, data as *mut c_void) };
    if result == -1 {
        Err(AttachError::Ptrace("request", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ptrace_getregs(
    pid: libc::pid_t,
    registers: &mut libc::user_regs_struct,
) -> Result<(), AttachError> {
    // SAFETY: tracee is stopped and registers points to writable process memory.
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_GETREGS,
            pid,
            std::ptr::null_mut::<c_void>(),
            std::ptr::from_mut(registers).cast::<c_void>(),
        )
    };
    if result == -1 {
        Err(AttachError::Ptrace("getregs", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ptrace_setregs(pid: libc::pid_t, registers: &libc::user_regs_struct) -> Result<(), AttachError> {
    // SAFETY: tracee is stopped and registers points to a complete readable record.
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_SETREGS,
            pid,
            std::ptr::null_mut::<c_void>(),
            (registers as *const libc::user_regs_struct)
                .cast_mut()
                .cast::<c_void>(),
        )
    };
    if result == -1 {
        Err(AttachError::Ptrace("setregs", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn peek(pid: libc::pid_t, address: u64) -> Result<u64, AttachError> {
    // SAFETY: errno is thread-local and the tracee is stopped at a validated mapping.
    unsafe {
        *libc::__errno_location() = 0;
    }
    // SAFETY: PTRACE_PEEKDATA reads one machine word from the stopped tracee.
    let value = unsafe {
        libc::ptrace(
            libc::PTRACE_PEEKDATA,
            pid,
            address as *mut c_void,
            std::ptr::null_mut::<c_void>(),
        )
    };
    // SAFETY: errno is read only in this thread immediately after ptrace.
    if value == -1 && unsafe { *libc::__errno_location() } != 0 {
        Err(AttachError::Ptrace("peek", io::Error::last_os_error()))
    } else {
        Ok(value as u64)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn poke(pid: libc::pid_t, address: u64, value: u64) -> Result<(), AttachError> {
    ptrace(libc::PTRACE_POKEDATA, pid, address, value as usize)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn wait_stopped(pid: libc::pid_t, timeout: Duration) -> Result<i32, AttachError> {
    let start = Instant::now();
    loop {
        let mut status = 0;
        // SAFETY: status is writable and __WALL observes a ptrace stop of this task.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::__WALL) };
        if result == pid {
            if libc::WIFSTOPPED(status) {
                return Ok(libc::WSTOPSIG(status));
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                return Err(AttachError::Exited);
            }
            return Err(AttachError::UnexpectedStop(status));
        }
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if start.elapsed() >= timeout {
            return Err(AttachError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone)]
struct Map {
    start: u64,
    end: u64,
    offset: u64,
    device: String,
    inode: u64,
    executable: bool,
    writable: bool,
    path: Option<PathBuf>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn maps(pid: u32) -> Vec<Map> {
    std::fs::read_to_string(format!("/proc/{pid}/maps"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let mut range = fields.next()?.split('-');
            let start = u64::from_str_radix(range.next()?, 16).ok()?;
            let end = u64::from_str_radix(range.next()?, 16).ok()?;
            let permissions = fields.next()?;
            let offset = u64::from_str_radix(fields.next()?, 16).ok()?;
            let device = fields.next()?.to_owned();
            let inode = fields.next()?.parse().ok()?;
            let path = fields.next().map(PathBuf::from);
            Some(Map {
                start,
                end,
                offset,
                device,
                inode,
                executable: permissions.as_bytes().get(2) == Some(&b'x'),
                writable: permissions.as_bytes().get(1) == Some(&b'w'),
                path,
            })
        })
        .collect()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn stack_mapping(pid: u32, stack_pointer: u64) -> Result<Map, AttachError> {
    maps(pid)
        .into_iter()
        .find(|mapping| {
            mapping.start <= stack_pointer
                && stack_pointer < mapping.end
                && mapping.writable
                && mapping
                    .path
                    .as_deref()
                    .is_some_and(|path| path.to_string_lossy().starts_with("[stack"))
        })
        .ok_or(AttachError::InvalidStack)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn remote_symbol(pid: libc::pid_t, local: *const c_void) -> Result<*const c_void, AttachError> {
    let local_mapping = maps(std::process::id())
        .into_iter()
        .find(|mapping| mapping.start <= local as u64 && (local as u64) < mapping.end)
        .ok_or(AttachError::SymbolIdentity)?;
    let target_mapping = maps(pid as u32)
        .into_iter()
        .find(|mapping| {
            mapping.device == local_mapping.device
                && mapping.inode == local_mapping.inode
                && mapping.offset == local_mapping.offset
                && mapping.executable
        })
        .ok_or(AttachError::SymbolIdentity)?;
    Ok((target_mapping.start + (local as u64 - local_mapping.start)) as *const c_void)
}
