use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LINE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    ToggleEdit,
    Quit,
    YieldToVosu,
}
impl Command {
    fn line(self) -> &'static str {
        match self {
            Self::ToggleEdit => "toggle-edit\n",
            Self::Quit => "quit\n",
            Self::YieldToVosu => "yield-to-vosu\n",
        }
    }
    fn parse(line: &str) -> Result<Self> {
        match line {
            "toggle-edit" => Ok(Self::ToggleEdit),
            "quit" => Ok(Self::Quit),
            "yield-to-vosu" => Ok(Self::YieldToVosu),
            _ => bail!("unknown control command"),
        }
    }
}

pub struct Request {
    pub command: Command,
    reply: mpsc::SyncSender<Result<(), String>>,
}
impl Request {
    pub fn answer(self, result: Result<(), String>) {
        let _ = self.reply.send(result);
    }
}

pub struct Server {
    requests: mpsc::Receiver<Request>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    #[cfg(target_os = "linux")]
    path: std::path::PathBuf,
}
impl Server {
    pub fn start() -> Result<Self> {
        Self::start_at(default_endpoint()?)
    }
    fn start_at(endpoint: Endpoint) -> Result<Self> {
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let host = thread::current();
        #[cfg(target_os = "linux")]
        let (listener, path) = (linux_listener(&endpoint)?, endpoint);
        #[cfg(windows)]
        let mut pipe = Pipe(windows_pipe(&endpoint)?);
        let thread = thread::Builder::new()
            .name("vosulay-control".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    #[cfg(target_os = "linux")]
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if same_user(&stream).unwrap_or(false) {
                                let _ = stream.set_read_timeout(Some(TIMEOUT));
                                let _ = stream.set_write_timeout(Some(TIMEOUT));
                                serve(&mut stream, &sender, &host);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => {
                            eprintln!("control listener: {error}");
                            break;
                        }
                    }
                    #[cfg(windows)]
                    {
                        use windows::Win32::{
                            Foundation::{
                                ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
                            },
                            System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe},
                        };
                        let handle = pipe.handle();
                        match unsafe { ConnectNamedPipe(handle, None) } {
                            Ok(()) => serve(&mut pipe, &sender, &host),
                            Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => {
                                serve(&mut pipe, &sender, &host)
                            }
                            Err(error) if error.code() == ERROR_PIPE_LISTENING.to_hresult() => {
                                thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            // A client connected and closed before this poll saw it; free the
                            // instance for the next one.
                            Err(error) if error.code() == ERROR_NO_DATA.to_hresult() => {}
                            Err(error) => {
                                eprintln!("control pipe: {error}");
                                break;
                            }
                        }
                        let _ = unsafe { DisconnectNamedPipe(handle) };
                    }
                }
            })?;
        Ok(Self {
            requests,
            stop,
            thread: Some(thread),
            #[cfg(target_os = "linux")]
            path,
        })
    }
    pub fn next_request(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(target_os = "linux")]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where the running instance listens: a socket path on Linux, a pipe name on Windows.
#[cfg(target_os = "linux")]
type Endpoint = std::path::PathBuf;
#[cfg(windows)]
type Endpoint = String;

fn default_endpoint() -> Result<Endpoint> {
    #[cfg(target_os = "linux")]
    {
        Ok(super::instance::InstanceGuard::runtime_directory()?.join("control.sock"))
    }
    #[cfg(windows)]
    {
        pipe_name()
    }
}

fn read_line(reader: &mut impl Read) -> Result<String> {
    let until = Instant::now() + TIMEOUT;
    let mut line = Vec::new();
    loop {
        ensure!(Instant::now() < until, "control read timed out");
        let mut byte = [0];
        match reader.read(&mut byte) {
            Ok(0) => bail!("incomplete control line"),
            Ok(_) if byte[0] == b'\n' => {
                return String::from_utf8(line).context("invalid control text");
            }
            Ok(_) => {
                ensure!(line.len() < MAX_LINE, "control line is too long");
                line.push(byte[0]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn serve(stream: &mut (impl Read + Write), sender: &mpsc::Sender<Request>, host: &thread::Thread) {
    let result = (|| -> Result<()> {
        let command = Command::parse(&read_line(stream)?)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .send(Request { command, reply })
            .context("host is shutting down")?;
        host.unpark();
        response
            .recv_timeout(TIMEOUT)
            .context("host did not answer control command")?
            .map_err(anyhow::Error::msg)
    })();
    let answer = match result {
        Ok(()) => "ok\n".to_owned(),
        Err(error) => format!("error: {}\n", error.to_string().replace(['\r', '\n'], " ")),
    };
    let _ = stream.write_all(answer.as_bytes());
    // DisconnectNamedPipe discards unread output. Wait boundedly for the
    // client to consume its response and close, without a blocking flush.
    #[cfg(windows)]
    let _ = read_line(stream);
}
pub fn send(command: Command) -> Result<()> {
    send_to(&default_endpoint()?, command)
}
fn send_to(endpoint: &Endpoint, command: Command) -> Result<()> {
    #[cfg(target_os = "linux")]
    let mut stream = {
        let stream = std::os::unix::net::UnixStream::connect(endpoint)
            .context("could not connect to running vosulay")?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream
    };
    #[cfg(windows)]
    let mut stream = {
        use windows::Win32::System::Pipes::{PIPE_NOWAIT, SetNamedPipeHandleState};
        // The single pipe instance stays busy until the server has disconnected the previous
        // client, so a quick second command may briefly find it busy.
        const ERROR_PIPE_BUSY: i32 = 231;
        let until = Instant::now() + TIMEOUT;
        let file = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(endpoint)
            {
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < until =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                result => break result.context("could not connect to running vosulay")?,
            }
        };
        let pipe = Pipe(file);
        unsafe { SetNamedPipeHandleState(pipe.handle(), Some(&PIPE_NOWAIT), None, None)? };
        pipe
    };
    stream.write_all(command.line().as_bytes())?;
    let response = read_line(&mut stream)?;
    ensure!(response == "ok", "{response}");
    Ok(())
}

/// A non-blocking (`PIPE_NOWAIT`) pipe handle. std's `File::read` reports `ERROR_NO_DATA`, which
/// on such a handle means "nothing to read yet", as end of file, so neither side could ever wait
/// for the other's line; this reads with `ReadFile` and reports it as `WouldBlock` instead.
#[cfg(windows)]
struct Pipe(std::fs::File);
#[cfg(windows)]
impl Pipe {
    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        windows::Win32::Foundation::HANDLE(self.0.as_raw_handle())
    }
}
#[cfg(windows)]
impl Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use windows::Win32::{
            Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA},
            Storage::FileSystem::ReadFile,
        };
        let mut read = 0;
        match unsafe { ReadFile(self.handle(), Some(buffer), Some(&mut read), None) } {
            Ok(()) => Ok(read as usize),
            Err(error) if error.code() == ERROR_NO_DATA.to_hresult() => {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            Err(error) if error.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
            Err(error) => Err(error.into()),
        }
    }
}
#[cfg(windows)]
impl Write for Pipe {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write(buffer)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(target_os = "linux")]
fn linux_listener(path: &std::path::Path) -> Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}
#[cfg(target_os = "linux")]
fn same_user(stream: &std::os::unix::net::UnixStream) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        } == 0,
        "could not authenticate control peer"
    );
    Ok(credentials.uid == unsafe { libc::getuid() })
}

#[cfg(windows)]
fn pipe_name() -> Result<String> {
    Ok(format!(
        "\\\\.\\pipe\\vosulay-control-{:016x}",
        super::instance::user_suffix()?
    ))
}
#[cfg(windows)]
fn windows_pipe(name: &str) -> Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows::{
        Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
            System::Pipes::{
                CreateNamedPipeW, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE,
            },
        },
        core::PCWSTR,
    };
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1024,
            1024,
            0,
            None,
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "could not create control pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { std::fs::File::from_raw_handle(handle.0) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_rejects_unknown_incomplete_and_oversize_lines() {
        assert_eq!(
            Command::parse(&read_line(&mut &b"toggle-edit\n"[..]).unwrap()).unwrap(),
            Command::ToggleEdit
        );
        assert_eq!(Command::parse("quit").unwrap(), Command::Quit);
        assert_eq!(
            Command::parse("yield-to-vosu").unwrap(),
            Command::YieldToVosu
        );
        assert_eq!(Command::YieldToVosu.line(), "yield-to-vosu\n");
        assert!(Command::parse("exit").is_err());
        assert!(read_line(&mut &b"quit"[..]).is_err());
        assert!(read_line(&mut vec![b'x'; MAX_LINE + 1].as_slice()).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn peer_credentials_match_current_user() {
        let (stream, _) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(same_user(&stream).unwrap());
    }
    /// A private endpoint, so the test never meets a running vosulay.
    fn test_endpoint(directory: &std::path::Path) -> Endpoint {
        #[cfg(target_os = "linux")]
        {
            directory.join("control.sock")
        }
        #[cfg(windows)]
        {
            let _ = directory;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            format!(
                r"\\.\pipe\vosulay-control-test-{}-{nanos}",
                std::process::id()
            )
        }
    }
    #[test]
    fn server_answers_consecutive_commands_after_a_client_that_left_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = test_endpoint(directory.path());
        let server = Server::start_at(endpoint.clone()).unwrap();
        let client = thread::spawn(move || {
            // On Windows this made ConnectNamedPipe report ERROR_NO_DATA, which stopped the
            // control thread for good.
            #[cfg(target_os = "linux")]
            drop(std::os::unix::net::UnixStream::connect(&endpoint).unwrap());
            #[cfg(windows)]
            drop(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&endpoint)
                    .unwrap(),
            );
            thread::sleep(Duration::from_millis(50));
            let first = send_to(&endpoint, Command::Quit);
            let second = send_to(&endpoint, Command::ToggleEdit);
            (first, second)
        });
        let mut seen = Vec::new();
        while !client.is_finished() {
            match server.next_request() {
                Some(request) => {
                    seen.push(request.command);
                    let answer = match request.command {
                        Command::Quit | Command::YieldToVosu => Ok(()),
                        Command::ToggleEdit => Err("not now".to_owned()),
                    };
                    request.answer(answer);
                }
                None => thread::park_timeout(Duration::from_millis(10)),
            }
        }
        let (first, second) = client.join().unwrap();
        first.unwrap();
        assert!(
            second
                .as_ref()
                .is_err_and(|error| error.to_string() == "error: not now"),
            "{second:?}"
        );
        assert_eq!(seen, [Command::Quit, Command::ToggleEdit]);
    }
}
