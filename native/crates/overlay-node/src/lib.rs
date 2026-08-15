use napi::bindgen_prelude::{AsyncTask, Buffer, Task, Unknown};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Result, Status};
use napi_derive::napi;
use overlay_host::{Host, HostEvent, HostSession};
use overlay_protocol::{
    CAP_DMABUF, CAP_FRAME_REQUEST, CAP_SHM_BGRA, DRM_FORMAT_ABGR8888,
    DRM_FORMAT_ARGB8888, DmabufDescriptor, DmabufPlane,
    TEXTURE_FORMAT_PREMULTIPLIED_BGRA8, TEXTURE_FORMAT_PREMULTIPLIED_RGBA8, Target,
    TextureDescriptor,
};
use std::{
    collections::{HashMap, VecDeque},
    ffi::{OsStr, OsString},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn fail<E: std::fmt::Display>(error: E) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const MAX_PENDING_EVENTS: usize = 64;

#[napi(object)]
pub struct AttachOptions {
    pub vosu_url: Option<String>,
    pub attach_tool_path: Option<String>,
    pub profiler_library_path: Option<String>,
    pub agent_library_path: Option<String>,
    pub timeout: Option<u32>,
}

#[napi(object)]
pub struct VulkanLaunchOptions {
    pub executable: String,
    pub args: Option<Vec<String>>,
    pub layer_directory: String,
    pub agent_library_path: Option<String>,
    pub working_directory: Option<String>,
    pub timeout: Option<u32>,
}

#[napi(object)]
pub struct SurfaceDescriptor {
    pub target_id: i64,
    pub width: i64,
    pub height: i64,
    pub stride: i64,
    pub offset: i64,
    pub size: i64,
    pub fd: Option<i64>,
    pub format: Option<i64>,
}

#[napi(object)]
pub struct BitmapDescriptor {
    pub target_id: i64,
    pub width: i64,
    pub height: i64,
    pub stride: i64,
    pub format: Option<String>,
}

#[napi(object)]
pub struct SharedTexturePlane {
    pub fd: i64,
    pub stride: i64,
    pub offset: i64,
    pub size: i64,
}

#[napi(object)]
pub struct SharedTextureDescriptor {
    pub target_id: i64,
    pub sequence: i64,
    pub width: i64,
    pub height: i64,
    pub fourcc: String,
    pub modifier: String,
    pub planes: Vec<SharedTexturePlane>,
}

type EventCallback = ThreadsafeFunction<String, Unknown<'static>, String, Status, false>;

struct EventState {
    callbacks: HashMap<String, EventCallback>,
    pending: VecDeque<(String, String)>,
}

struct Shared {
    commands: Mutex<CommandQueue>,
    signal: Condvar,
    cancel: Arc<AtomicBool>,
    events: Mutex<EventState>,
}

struct CommandQueue {
    pending: VecDeque<Envelope>,
    closed: bool,
}

struct Envelope {
    command: SessionCommand,
    completion: Arc<Completion>,
}

struct Completion {
    outcome: Mutex<Option<std::result::Result<(), String>>>,
    signal: Condvar,
}

impl Completion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(None),
            signal: Condvar::new(),
        })
    }

    fn settle(&self, outcome: std::result::Result<(), String>) {
        if let Ok(mut slot) = self.outcome.lock() {
            if slot.is_none() {
                *slot = Some(outcome);
            }
            self.signal.notify_all();
        }
    }

    fn wait(&self) -> Result<()> {
        let mut slot = self.outcome.lock().map_err(fail)?;
        while slot.is_none() {
            slot = self.signal.wait(slot).map_err(fail)?;
        }
        match slot.take().expect("completion was settled") {
            Ok(()) => Ok(()),
            Err(message) => Err(fail(message)),
        }
    }
}

enum SessionCommand {
    Visible(bool),
    Capture(bool),
    SurfaceFd {
        descriptor: TextureDescriptor,
        fd: OwnedFd,
    },
    SurfaceBytes {
        descriptor: TextureDescriptor,
        bytes: Buffer,
    },
    Dmabuf {
        descriptor: DmabufDescriptor,
        fds: Vec<OwnedFd>,
    },
    Clear(u64),
    Destroy,
}

impl SessionCommand {
    fn frame_target(&self) -> Option<u64> {
        match self {
            Self::SurfaceFd { descriptor, .. } | Self::SurfaceBytes { descriptor, .. } => {
                Some(descriptor.target_id)
            }
            Self::Dmabuf { descriptor, .. } => Some(descriptor.target_id),
            _ => None,
        }
    }

    fn locally_consumed(&self) -> Option<(u64, u64)> {
        match self {
            Self::Dmabuf { descriptor, .. } => Some((descriptor.target_id, descriptor.sequence)),
            _ => None,
        }
    }
}

pub struct SessionTask {
    completion: Option<Arc<Completion>>,
}

impl Task for SessionTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.completion
            .take()
            .ok_or_else(|| fail("task already run"))?
            .wait()
    }

    fn resolve(&mut self, _env: Env, _: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

fn enqueue(shared: &Arc<Shared>, command: SessionCommand) -> Arc<Completion> {
    let completion = Completion::new();
    let Ok(mut queue) = shared.commands.lock() else {
        completion.settle(Err("overlay session state is unavailable".to_owned()));
        return completion;
    };
    if queue.closed {
        completion.settle(match command {
            SessionCommand::Destroy => Ok(()),
            _ => Err("overlay handle is destroyed".to_owned()),
        });
        return completion;
    }

    let mut locally_consumed = Vec::new();
    if let Some(target_id) = command.frame_target() {
        queue.pending.retain(|envelope| {
            if envelope.command.frame_target() == Some(target_id) {
                if let Some(consumed) = envelope.command.locally_consumed() {
                    locally_consumed.push(consumed);
                }
                envelope.completion.settle(Ok(()));
                false
            } else {
                true
            }
        });
    }
    queue.pending.push_back(Envelope {
        command,
        completion: Arc::clone(&completion),
    });
    drop(queue);

    for (target_id, sequence) in locally_consumed {
        emit_event(shared, "consumed", format!("{target_id}:{sequence}"));
    }
    shared.signal.notify_one();
    completion
}

fn queue(shared: &Arc<Shared>, command: SessionCommand) -> AsyncTask<SessionTask> {
    AsyncTask::new(SessionTask {
        completion: Some(enqueue(shared, command)),
    })
}

#[napi]
pub struct OverlaySurface {
    shared: Arc<Shared>,
    capabilities: u64,
}

#[napi]
pub struct OverlayHandle {
    pid: u32,
    capabilities: u64,
    socket_name: String,
    cookie: String,
    shared: Arc<Shared>,
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Release);
        if let Ok(mut events) = self.shared.events.lock() {
            events.callbacks.clear();
            events.pending.clear();
        }
        if let Ok(mut queue) = self.shared.commands.lock() {
            queue.closed = true;
            for envelope in queue.pending.drain(..) {
                envelope
                    .completion
                    .settle(Err("overlay handle is destroyed".to_owned()));
            }
        }
        self.shared.signal.notify_all();
    }
}

#[napi]
impl OverlayHandle {
    #[napi]
    pub fn socket_name(&self) -> String {
        self.socket_name.clone()
    }

    #[napi]
    pub fn cookie(&self) -> String {
        self.cookie.clone()
    }

    #[napi(getter)]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    #[napi]
    pub fn get_pid(&self) -> u32 {
        self.pid
    }

    #[napi]
    pub fn supports_dmabuf(&self) -> bool {
        self.capabilities & CAP_DMABUF != 0
    }

    #[napi]
    pub fn supports_bgra_shm(&self) -> bool {
        self.capabilities & CAP_SHM_BGRA != 0
    }

    #[napi]
    pub fn supports_frame_requests(&self) -> bool {
        self.capabilities & CAP_FRAME_REQUEST != 0
    }

    #[napi]
    pub fn destroy(&self) -> AsyncTask<SessionTask> {
        if let Ok(mut events) = self.shared.events.lock() {
            events.callbacks.clear();
            events.pending.clear();
        }
        queue(&self.shared, SessionCommand::Destroy)
    }

    #[napi]
    pub fn on(
        &self,
        event: String,
        callback: ThreadsafeFunction<String, Unknown<'static>, String, Status, false>,
    ) -> Result<()> {
        if !matches!(
            event.as_str(),
            "disconnect"
                | "input"
                | "end"
                | "consumed"
                | "frame"
                | "frame-request"
                | "target"
        ) {
            return Err(fail("unsupported overlay event"));
        }
        let mut state = self.shared.events.lock().map_err(fail)?;
        state.callbacks.insert(event.clone(), callback);
        let mut deliveries = Vec::new();
        let mut retained = VecDeque::with_capacity(state.pending.len());
        while let Some((name, payload)) = state.pending.pop_front() {
            if name == event {
                deliveries.push(payload);
            } else {
                retained.push_back((name, payload));
            }
        }
        state.pending = retained;
        if let Some(callback) = state.callbacks.get(&event) {
            for payload in deliveries {
                let _ = callback.call(payload, ThreadsafeFunctionCallMode::NonBlocking);
            }
        }
        Ok(())
    }

    #[napi]
    pub fn set_editing(&self, editing: bool) -> AsyncTask<SessionTask> {
        self.capture_input(editing)
    }

    #[napi]
    pub fn set_visible(&self, visible: bool) -> AsyncTask<SessionTask> {
        queue(&self.shared, SessionCommand::Visible(visible))
    }

    #[napi]
    pub fn capture_input(&self, capture: bool) -> AsyncTask<SessionTask> {
        queue(&self.shared, SessionCommand::Capture(capture))
    }

    #[napi]
    pub fn create_surface(&self) -> Result<OverlaySurface> {
        Ok(OverlaySurface {
            shared: Arc::clone(&self.shared),
            capabilities: self.capabilities,
        })
    }

    #[napi]
    pub fn poll_event(&self) -> Result<String> {
        let mut events = self.shared.events.lock().map_err(fail)?;
        match events.pending.pop_front() {
            Some((name, payload)) => Ok(format!("{name}:{payload}")),
            None => Ok(String::new()),
        }
    }
}

#[napi]
impl OverlaySurface {
    #[napi]
    pub fn update_shared_texture(
        &self,
        descriptor: SurfaceDescriptor,
    ) -> Result<AsyncTask<SessionTask>> {
        self.update_descriptor(descriptor)
    }

    #[napi]
    pub fn update_descriptor(
        &self,
        descriptor: SurfaceDescriptor,
    ) -> Result<AsyncTask<SessionTask>> {
        let descriptor = TextureDescriptor {
            target_id: u64::try_from(descriptor.target_id).map_err(fail)?,
            width: u32::try_from(descriptor.width).map_err(fail)?,
            height: u32::try_from(descriptor.height).map_err(fail)?,
            stride: u32::try_from(descriptor.stride).map_err(fail)?,
            offset: u64::try_from(descriptor.offset).map_err(fail)?,
            size: u64::try_from(descriptor.size).map_err(fail)?,
            format: u32::try_from(descriptor.format.unwrap_or(0)).map_err(fail)?,
        };
        descriptor.validate().map_err(fail)?;
        let raw = RawFd::try_from(
            descriptor_fd(&descriptor, None)
                .ok_or_else(|| fail("descriptor requires fd"))?,
        )
        .map_err(fail)?;
        if raw < 0 {
            return Err(fail("descriptor fd is negative"));
        }
        let duplicate = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(fail(std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(duplicate) };
        Ok(queue(
            &self.shared,
            SessionCommand::SurfaceFd { descriptor, fd },
        ))
    }

    #[napi]
    pub fn update_bitmap(
        &self,
        descriptor: BitmapDescriptor,
        mut bitmap: Buffer,
    ) -> Result<AsyncTask<SessionTask>> {
        let width = u32::try_from(descriptor.width).map_err(fail)?;
        let height = u32::try_from(descriptor.height).map_err(fail)?;
        let stride = u32::try_from(descriptor.stride).map_err(fail)?;
        let format = match descriptor.format.as_deref().unwrap_or("BGRA") {
            "RGBA" => TEXTURE_FORMAT_PREMULTIPLIED_RGBA8,
            "BGRA" if self.capabilities & CAP_SHM_BGRA != 0 => {
                TEXTURE_FORMAT_PREMULTIPLIED_BGRA8
            }
            "BGRA" => {
                swizzle_bgra_to_rgba(&mut bitmap, width, height, stride)?;
                TEXTURE_FORMAT_PREMULTIPLIED_RGBA8
            }
            _ => return Err(fail("bitmap format must be BGRA or RGBA")),
        };
        let descriptor = TextureDescriptor {
            target_id: u64::try_from(descriptor.target_id).map_err(fail)?,
            width,
            height,
            stride,
            offset: 0,
            size: u64::try_from(bitmap.len()).map_err(fail)?,
            format,
        };
        descriptor.validate().map_err(fail)?;
        Ok(queue(
            &self.shared,
            SessionCommand::SurfaceBytes {
                descriptor,
                bytes: bitmap,
            },
        ))
    }

    #[napi]
    pub fn update_dmabuf(
        &self,
        descriptor: SharedTextureDescriptor,
    ) -> Result<AsyncTask<SessionTask>> {
        let fourcc = match descriptor.fourcc.as_str() {
            "AR24" => DRM_FORMAT_ARGB8888,
            "AB24" => DRM_FORMAT_ABGR8888,
            _ => return Err(fail("unsupported DRM fourcc")),
        };
        let modifier = parse_u64(&descriptor.modifier, "modifier")?;
        let mut fds = Vec::with_capacity(descriptor.planes.len());
        let mut planes = Vec::with_capacity(descriptor.planes.len());
        for plane in descriptor.planes {
            let raw = RawFd::try_from(plane.fd).map_err(fail)?;
            if raw < 0 {
                return Err(fail("descriptor fd is negative"));
            }
            let duplicate = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                return Err(fail(std::io::Error::last_os_error()));
            }
            fds.push(unsafe { OwnedFd::from_raw_fd(duplicate) });
            planes.push(DmabufPlane {
                stride: u32::try_from(plane.stride).map_err(fail)?,
                offset: u64::try_from(plane.offset).map_err(fail)?,
                size: u64::try_from(plane.size).map_err(fail)?,
            });
        }
        let descriptor = DmabufDescriptor {
            target_id: u64::try_from(descriptor.target_id).map_err(fail)?,
            sequence: u64::try_from(descriptor.sequence).map_err(fail)?,
            width: u32::try_from(descriptor.width).map_err(fail)?,
            height: u32::try_from(descriptor.height).map_err(fail)?,
            fourcc,
            modifier,
            planes,
        };
        descriptor.validate().map_err(fail)?;
        Ok(queue(
            &self.shared,
            SessionCommand::Dmabuf { descriptor, fds },
        ))
    }

    #[napi]
    pub fn clear(&self, target_id: i64) -> Result<AsyncTask<SessionTask>> {
        Ok(queue(
            &self.shared,
            SessionCommand::Clear(u64::try_from(target_id).map_err(fail)?),
        ))
    }
}

// Kept separate so descriptor conversion above cannot accidentally consume the
// JS object's fd before all numeric fields have been validated.
fn descriptor_fd(_descriptor: &TextureDescriptor, fd: Option<i64>) -> Option<i64> {
    fd
}

fn swizzle_bgra_to_rgba(
    bytes: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
) -> Result<()> {
    let row_pixels = usize::try_from(width)
        .map_err(fail)?
        .checked_mul(4)
        .ok_or_else(|| fail("bitmap row is too large"))?;
    let stride = usize::try_from(stride).map_err(fail)?;
    if stride < row_pixels {
        return Err(fail("bitmap stride is shorter than one row"));
    }
    for row in 0..usize::try_from(height).map_err(fail)? {
        let start = row
            .checked_mul(stride)
            .ok_or_else(|| fail("bitmap offset overflow"))?;
        let end = start
            .checked_add(row_pixels)
            .ok_or_else(|| fail("bitmap row overflow"))?;
        let pixels = bytes
            .get_mut(start..end)
            .ok_or_else(|| fail("bitmap is shorter than its descriptor"))?;
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    Ok(())
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).map_err(|_| fail(format!("invalid {field}")));
    }
    value
        .parse::<u64>()
        .or_else(|_| u64::from_str_radix(value, 16))
        .map_err(|_| fail(format!("invalid {field}")))
}

fn emit_event(shared: &Shared, name: &str, payload: String) {
    let Ok(mut state) = shared.events.lock() else {
        return;
    };
    if let Some(callback) = state.callbacks.get(name) {
        let _ = callback.call(payload, ThreadsafeFunctionCallMode::NonBlocking);
        return;
    }
    if state.pending.len() == MAX_PENDING_EVENTS {
        state.pending.pop_front();
    }
    state.pending.push_back((name.to_owned(), payload));
}

fn apply_command(
    session: &mut HostSession,
    command: SessionCommand,
) -> std::result::Result<(), String> {
    let describe = |error: overlay_host::HostError| error.to_string();
    match command {
        SessionCommand::Visible(value) => session.set_visible(value).map_err(describe),
        SessionCommand::Capture(value) => session.set_input_capture(value).map_err(describe),
        SessionCommand::SurfaceFd { descriptor, fd } => {
            session
                .ensure_target(Target {
                    target_id: descriptor.target_id,
                    width: descriptor.width,
                    height: descriptor.height,
                })
                .map_err(describe)?;
            session
                .send_shm(descriptor.target_id, descriptor, fd)
                .map_err(describe)
        }
        SessionCommand::SurfaceBytes { descriptor, bytes } => {
            let fd = sealed_memfd(&bytes).map_err(|error| error.to_string())?;
            session
                .ensure_target(Target {
                    target_id: descriptor.target_id,
                    width: descriptor.width,
                    height: descriptor.height,
                })
                .map_err(describe)?;
            session
                .send_shm(descriptor.target_id, descriptor, fd)
                .map_err(describe)
        }
        SessionCommand::Dmabuf { descriptor, fds } => {
            session
                .ensure_target(Target {
                    target_id: descriptor.target_id,
                    width: descriptor.width,
                    height: descriptor.height,
                })
                .map_err(describe)?;
            session
                .send_dmabuf(descriptor.target_id, descriptor, fds)
                .map_err(describe)
        }
        SessionCommand::Clear(target_id) => session.clear(target_id).map_err(describe),
        SessionCommand::Destroy => {
            let _ = session.shutdown();
            Ok(())
        }
    }
}

fn start_session_actor(shared: Arc<Shared>, mut session: HostSession) {
    thread::spawn(move || {
        let mut ping_sequence = 1u64;
        let mut last_ping = Instant::now();
        loop {
            if shared.cancel.load(Ordering::Acquire) {
                break;
            }
            let mut destroyed = false;
            loop {
                let envelope = match shared.commands.lock() {
                    Ok(mut queue) => queue.pending.pop_front(),
                    Err(_) => None,
                };
                let Some(envelope) = envelope else {
                    break;
                };
                let stop = matches!(envelope.command, SessionCommand::Destroy);
                let outcome = apply_command(&mut session, envelope.command);
                envelope.completion.settle(outcome);
                if stop {
                    destroyed = true;
                    break;
                }
            }
            if destroyed {
                shared.cancel.store(true, Ordering::Release);
                break;
            }

            if last_ping.elapsed() >= Duration::from_millis(500) {
                if session.ping(ping_sequence).is_err() {
                    emit_event(&shared, "disconnect", "disconnect".to_owned());
                    break;
                }
                ping_sequence = ping_sequence.wrapping_add(1);
                last_ping = Instant::now();
            }

            let event = match session.recv() {
                Ok(event) => Some(event),
                Err(overlay_host::HostError::Protocol(overlay_protocol::ProtocolError::Io(
                    error,
                ))) if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => None,
                Err(_) => {
                    emit_event(&shared, "disconnect", "disconnect".to_owned());
                    break;
                }
            };
            let Some(event) = event else {
                if let Ok(queue) = shared.commands.lock()
                    && queue.pending.is_empty()
                {
                    let _ = shared.signal.wait_timeout(queue, Duration::from_millis(10));
                }
                continue;
            };
            let (name, payload) = match event {
                HostEvent::TargetCreated(target) => (
                    "target",
                    format!(
                        "created:{}:{}:{}",
                        target.target_id, target.width, target.height
                    ),
                ),
                HostEvent::TargetDestroyed(target_id) => {
                    ("target", format!("destroyed:{target_id}"))
                }
                HostEvent::TargetSelected(selection) => (
                    "target",
                    format!("selected:{}:{}", selection.target_id, selection.generation),
                ),
                HostEvent::FrameRequested(request) => (
                    "frame-request",
                    format!("{}:{}", request.target_id, request.sequence),
                ),
                HostEvent::Input(event) => (
                    "input",
                    format!(
                        "{}:{}:{}:{}:{}:{}",
                        event.target_id,
                        event.kind,
                        event.code,
                        event.flags,
                        event.value,
                        event.value2
                    ),
                ),
                HostEvent::End(target_id) => ("end", target_id.to_string()),
                HostEvent::Consumed(consumed) => (
                    "consumed",
                    format!("{}:{}", consumed.target_id, consumed.sequence),
                ),
                HostEvent::Frame(frame) => ("frame", format!("{frame:?}")),
                _ => continue,
            };
            emit_event(&shared, name, payload);
        }
        if let Ok(mut queue) = shared.commands.lock() {
            queue.closed = true;
            for envelope in queue.pending.drain(..) {
                envelope
                    .completion
                    .settle(Err("overlay session ended".to_owned()));
            }
        }
    });
}

struct ProfilerAttach {
    tool: String,
    profiler: String,
    agent: String,
}

impl ProfilerAttach {
    fn from_options(options: Option<AttachOptions>) -> Result<Option<Self>> {
        let options = options.unwrap_or(AttachOptions {
            vosu_url: None,
            attach_tool_path: None,
            profiler_library_path: None,
            agent_library_path: None,
            timeout: None,
        });
        let tool = options
            .attach_tool_path
            .or_else(|| std::env::var("VOSULAY_DOTNET_ATTACH_TOOL").ok());
        let profiler = options
            .profiler_library_path
            .or_else(|| std::env::var("VOSULAY_DOTNET_PROFILER").ok());
        let agent = options
            .agent_library_path
            .or_else(|| std::env::var("VOSULAY_GL_AGENT").ok());
        match (tool, profiler, agent) {
            (None, None, None) => Ok(None),
            (Some(tool), Some(profiler), Some(agent)) => Ok(Some(Self {
                tool,
                profiler,
                agent,
            })),
            _ => Err(fail(
                "attachToolPath, profilerLibraryPath, and agentLibraryPath must be provided together",
            )),
        }
    }

    fn run(&self, pid: u32, socket_name: &str, cookie: &str, deadline: Instant) -> Result<()> {
        let mut command = if self.tool.ends_with(".dll") {
            let mut command = Command::new("dotnet");
            command.arg(&self.tool);
            command
        } else {
            Command::new(&self.tool)
        };
        command
            .envs(std::env::vars())
            .env("VOSULAY_ATTACH_TARGET_PID", pid.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command
            .arg(pid.to_string())
            .arg(&self.profiler)
            .arg(&self.agent)
            .arg(socket_name)
            .arg(cookie)
            .arg(std::process::id().to_string())
            .spawn()
            .map_err(fail)?;
        loop {
            match child.try_wait().map_err(fail)? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    let detail = child
                        .wait_with_output()
                        .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_owned())
                        .unwrap_or_default();
                    return Err(fail(format!("profiler attach exited {status}: {detail}")));
                }
                None => {}
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(
                    "profiler attach did not finish before the attach deadline",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration> {
    let now = Instant::now();
    if now >= deadline {
        return Err(fail("attach deadline elapsed before the agent connected"));
    }
    Ok(deadline - now)
}

pub struct AttachTask {
    pid: u32,
    attach: Option<ProfilerAttach>,
    timeout: Duration,
}

impl Task for AttachTask {
    type Output = OverlayHandle;
    type JsValue = OverlayHandle;

    fn compute(&mut self) -> Result<Self::Output> {
        let deadline = Instant::now() + self.timeout;
        let host = Host::new(self.pid).map_err(fail)?;
        let socket_name = hex(host.endpoint().name());
        let cookie = hex(&host.cookie());
        if let Some(attach) = &self.attach {
            attach.run(self.pid, &socket_name, &cookie, deadline)?;
        }
        connect_handle(
            host,
            socket_name,
            cookie,
            remaining(deadline)?,
            std::env::var_os("VOSULAY_GL_SHM_ONLY").is_none(),
        )
    }

    fn resolve(&mut self, _env: Env, handle: Self::Output) -> Result<Self::JsValue> {
        Ok(handle)
    }
}

#[napi(ts_return_type = "Promise<OverlayHandle>")]
pub fn attach(pid: u32, options: Option<AttachOptions>) -> Result<AsyncTask<AttachTask>> {
    let timeout = Duration::from_secs(u64::from(
        options
            .as_ref()
            .and_then(|options| options.timeout)
            .unwrap_or(30)
            .clamp(1, 120),
    ));
    Ok(AsyncTask::new(AttachTask {
        pid,
        attach: ProfilerAttach::from_options(options)?,
        timeout,
    }))
}

pub struct VulkanLaunchTask {
    options: Option<VulkanLaunchOptions>,
}

impl Task for VulkanLaunchTask {
    type Output = OverlayHandle;
    type JsValue = OverlayHandle;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self
            .options
            .take()
            .ok_or_else(|| fail("task already run"))?;
        let layer_directory = PathBuf::from(&options.layer_directory);
        validate_vulkan_layer_directory(&layer_directory)?;

        let mut host = Host::new(0).map_err(fail)?;
        let socket_name = hex(host.endpoint().name());
        let cookie = hex(&host.cookie());
        let mut command = Command::new(&options.executable);
        command
            .args(options.args.unwrap_or_default())
            .envs(std::env::vars());
        if let Some(directory) = options.working_directory {
            command.current_dir(directory);
        }
        configure_vulkan_environment(&mut command, &layer_directory, &socket_name, &cookie)?;
        if let Some(agent) = options.agent_library_path.as_deref() {
            configure_agent_preload(&mut command, Path::new(agent))?;
        }
        command.process_group(0);
        let mut child = command.spawn().map_err(fail)?;
        let pid = child.id();
        host.set_expected_process_group(pid);
        let timeout = Duration::from_secs(u64::from(
            options.timeout.unwrap_or(30).clamp(1, 120),
        ));
        match connect_handle(host, socket_name, cookie, timeout, true) {
            Ok(handle) => {
                reap_child(child);
                Ok(handle)
            }
            Err(error) => {
                terminate_process_group(&mut child, pid);
                Err(error)
            }
        }
    }

    fn resolve(&mut self, _env: Env, handle: Self::Output) -> Result<Self::JsValue> {
        Ok(handle)
    }
}

fn process_group_signal_target(process_group: u32) -> Option<i32> {
    let process_group = i32::try_from(process_group).ok()?;
    (process_group > 0).then_some(-process_group)
}

fn terminate_process_group(child: &mut Child, process_group: u32) {
    let Some(target) = process_group_signal_target(process_group) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    unsafe {
        libc::kill(target, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(target, libc::SIGKILL);
    }
    let _ = child.wait();
}

fn reap_child(mut child: Child) {
    let _ = thread::Builder::new()
        .name("vosulay-vulkan-child-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn validate_vulkan_layer_directory(directory: &Path) -> Result<()> {
    for name in ["VkLayer_VOSULAY_overlay.json", "liboverlay_vulkan_layer.so"] {
        let path = directory.join(name);
        if !path.is_file() {
            return Err(fail(format!(
                "Vulkan layer artifact is missing: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn configure_vulkan_environment(
    command: &mut Command,
    layer_directory: &Path,
    socket_name: &str,
    cookie: &str,
) -> Result<()> {
    let layer_path = prepend_search_path(
        layer_directory,
        std::env::var_os("VK_LAYER_PATH").as_deref(),
    )?;
    let instance_layers = prepend_layer_name(
        "VK_LAYER_VOSULAY_overlay",
        std::env::var("VK_INSTANCE_LAYERS").ok().as_deref(),
    );
    command
        .env("VK_LAYER_PATH", layer_path)
        .env("VK_INSTANCE_LAYERS", instance_layers)
        .env("VOSULAY_VULKAN_LAYER", "1")
        .env_remove("DISABLE_VOSULAY_VULKAN_LAYER")
        .env("VOSULAY_AGENT_SOCKET", socket_name)
        .env("VOSULAY_AGENT_COOKIE", cookie)
        .env("VOSULAY_AGENT_HOST_PID", std::process::id().to_string());
    Ok(())
}

fn configure_agent_preload(command: &mut Command, agent_library: &Path) -> Result<()> {
    if !agent_library.is_file() {
        return Err(fail(format!(
            "overlay agent library is missing: {}",
            agent_library.display()
        )));
    }
    command.env(
        "LD_PRELOAD",
        prepend_preload(agent_library, std::env::var_os("LD_PRELOAD").as_deref())?,
    );
    Ok(())
}

fn prepend_preload(library: &Path, existing: Option<&OsStr>) -> Result<OsString> {
    let library = library
        .to_str()
        .ok_or_else(|| fail("overlay agent library path is not valid UTF-8"))?;
    if library.contains(':') || library.contains(char::is_whitespace) {
        return Err(fail(format!(
            "overlay agent library path cannot be preloaded: {library}"
        )));
    }
    let mut value = OsString::from(library);
    if let Some(existing) = existing.filter(|existing| !existing.is_empty()) {
        value.push(":");
        value.push(existing);
    }
    Ok(value)
}

fn sealed_memfd(bytes: &[u8]) -> Result<OwnedFd> {
    let raw = unsafe {
        libc::memfd_create(
            c"vosulay-overlay-frame".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(fail(std::io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut written = 0;
    while written < bytes.len() {
        let count = unsafe {
            libc::write(
                fd.as_raw_fd(),
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if count <= 0 {
            return Err(fail(std::io::Error::last_os_error()));
        }
        written += count.cast_unsigned();
    }
    if unsafe {
        libc::fcntl(
            fd.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_SHRINK | libc::F_SEAL_GROW,
        )
    } < 0
    {
        return Err(fail(std::io::Error::last_os_error()));
    }
    Ok(fd)
}

fn prepend_search_path(directory: &Path, existing: Option<&OsStr>) -> Result<OsString> {
    let mut paths = vec![directory.to_owned()];
    if let Some(existing) = existing {
        paths.extend(std::env::split_paths(existing));
    }
    std::env::join_paths(paths).map_err(fail)
}

fn prepend_layer_name(layer: &str, existing: Option<&str>) -> String {
    existing
        .filter(|value| !value.is_empty())
        .map_or_else(|| layer.to_owned(), |value| format!("{layer}:{value}"))
}

fn connect_handle(
    host: Host,
    socket_name: String,
    cookie: String,
    timeout: Duration,
    expose_dmabuf: bool,
) -> Result<OverlayHandle> {
    let cancel = Arc::new(AtomicBool::new(false));
    let session = host
        .accept_cancellable_timeout(&cancel, timeout)
        .map_err(fail)?;
    let pid = session.peer_pid();
    let capabilities = if expose_dmabuf {
        session.capabilities()
    } else {
        session.capabilities() & !CAP_DMABUF
    };
    session.set_nonblocking().map_err(fail)?;
    let shared = Arc::new(Shared {
        commands: Mutex::new(CommandQueue {
            pending: VecDeque::new(),
            closed: false,
        }),
        signal: Condvar::new(),
        cancel,
        events: Mutex::new(EventState {
            callbacks: HashMap::new(),
            pending: VecDeque::with_capacity(MAX_PENDING_EVENTS),
        }),
    });
    start_session_actor(Arc::clone(&shared), session);
    Ok(OverlayHandle {
        pid,
        capabilities,
        socket_name,
        cookie,
        shared,
    })
}

#[napi(ts_return_type = "Promise<OverlayHandle>")]
pub fn launch_vulkan(options: VulkanLaunchOptions) -> AsyncTask<VulkanLaunchTask> {
    AsyncTask::new(VulkanLaunchTask {
        options: Some(options),
    })
}

#[cfg(test)]
mod launch_tests {
    use super::{
        CommandQueue, Completion, DRM_FORMAT_ARGB8888, DmabufDescriptor, EventState,
        SessionCommand, Shared, enqueue, parse_u64, prepend_layer_name, prepend_preload,
        prepend_search_path, process_group_signal_target, remaining, swizzle_bgra_to_rgba,
    };
    use std::{
        collections::{HashMap, VecDeque},
        ffi::OsStr,
        path::Path,
        sync::{Arc, Condvar, Mutex, atomic::AtomicBool, atomic::Ordering},
        time::{Duration, Instant},
    };

    #[test]
    fn packaged_layer_precedes_existing_vulkan_configuration() {
        assert_eq!(
            prepend_layer_name("VK_LAYER_VOSULAY_overlay", Some("VK_LAYER_MESA_overlay")),
            "VK_LAYER_VOSULAY_overlay:VK_LAYER_MESA_overlay",
        );
        assert_eq!(
            prepend_search_path(Path::new("/app/native"), Some(OsStr::new("/user/layers")))
                .unwrap(),
            OsStr::new("/app/native:/user/layers"),
        );
    }

    #[test]
    fn agent_preload_precedes_existing_preloads() {
        assert_eq!(
            prepend_preload(
                Path::new("/app/native/liboverlay_agent_gl.so"),
                Some(OsStr::new("/usr/lib/libmangohud.so")),
            )
            .unwrap(),
            OsStr::new("/app/native/liboverlay_agent_gl.so:/usr/lib/libmangohud.so"),
        );
        assert_eq!(
            prepend_preload(Path::new("/app/native/liboverlay_agent_gl.so"), None).unwrap(),
            OsStr::new("/app/native/liboverlay_agent_gl.so"),
        );
        assert_eq!(
            prepend_preload(
                Path::new("/app/native/liboverlay_agent_gl.so"),
                Some(OsStr::new("")),
            )
            .unwrap(),
            OsStr::new("/app/native/liboverlay_agent_gl.so"),
        );
    }

    #[test]
    fn unrepresentable_preload_paths_are_rejected() {
        assert!(prepend_preload(Path::new("/app dir/agent.so"), None).is_err());
        assert!(prepend_preload(Path::new("/app:dir/agent.so"), None).is_err());
    }

    #[test]
    fn electron_modifiers_parse_as_decimal_with_explicit_hex_support() {
        assert_eq!(
            parse_u64("72057594037927935", "modifier").unwrap(),
            0x00ff_ffff_ffff_ffff,
        );
        assert_eq!(
            parse_u64("0x100000000000002", "modifier").unwrap(),
            0x100000000000002
        );
        assert_eq!(parse_u64("deadbeef", "modifier").unwrap(), 0xdead_beef);
    }

    #[test]
    fn bgra_rows_are_swizzled_without_touching_padding() {
        let mut bytes = vec![1, 2, 3, 4, 9, 9, 9, 9, 5, 6, 7, 8, 8, 8, 8, 8];
        swizzle_bgra_to_rgba(&mut bytes, 1, 2, 8).unwrap();
        assert_eq!(
            bytes,
            vec![3, 2, 1, 4, 9, 9, 9, 9, 7, 6, 5, 8, 8, 8, 8, 8]
        );
    }

    #[test]
    fn process_group_signals_never_target_the_host_group() {
        assert_eq!(process_group_signal_target(0), None);
        assert_eq!(process_group_signal_target(42), Some(-42));
        assert_eq!(process_group_signal_target(u32::MAX), None);
    }

    fn test_shared() -> Arc<Shared> {
        Arc::new(Shared {
            commands: Mutex::new(CommandQueue {
                pending: VecDeque::new(),
                closed: false,
            }),
            signal: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            events: Mutex::new(EventState {
                callbacks: HashMap::new(),
                pending: VecDeque::new(),
            }),
        })
    }

    fn frame_command(target_id: u64, width: u32) -> SessionCommand {
        SessionCommand::Dmabuf {
            descriptor: DmabufDescriptor {
                target_id,
                sequence: u64::from(width),
                width,
                height: 4,
                fourcc: DRM_FORMAT_ARGB8888,
                modifier: 0,
                planes: Vec::new(),
            },
            fds: Vec::new(),
        }
    }

    fn queued(shared: &Arc<Shared>) -> Vec<Option<u64>> {
        shared
            .commands
            .lock()
            .unwrap()
            .pending
            .iter()
            .map(|envelope| envelope.command.frame_target())
            .collect()
    }

    #[test]
    fn a_control_command_never_overtakes_an_earlier_frame() {
        let shared = test_shared();
        enqueue(&shared, frame_command(1, 64));
        enqueue(&shared, SessionCommand::Clear(1));
        assert_eq!(queued(&shared), vec![Some(1), None]);
    }

    #[test]
    fn a_newer_frame_supersedes_an_unsent_one_for_the_same_target() {
        let shared = test_shared();
        enqueue(&shared, frame_command(1, 64));
        enqueue(&shared, frame_command(1, 128));
        let pending = shared.commands.lock().unwrap();
        assert_eq!(pending.pending.len(), 1);
        let sequence = match &pending.pending[0].command {
            SessionCommand::Dmabuf { descriptor, .. } => descriptor.sequence,
            _ => panic!("expected the newer frame"),
        };
        assert_eq!(sequence, 128);
    }

    #[test]
    fn superseding_settles_and_releases_the_unsent_dma_buf() {
        let shared = test_shared();
        let stranded = enqueue(&shared, frame_command(1, 64));
        enqueue(&shared, frame_command(1, 128));
        assert!(stranded.wait().is_ok());
        let events = shared.events.lock().unwrap();
        assert!(
            events
                .pending
                .iter()
                .any(|(name, payload)| name == "consumed" && payload == "1:64")
        );
    }

    #[test]
    fn frames_for_different_targets_do_not_supersede_each_other() {
        let shared = test_shared();
        enqueue(&shared, frame_command(1, 64));
        enqueue(&shared, frame_command(2, 64));
        assert_eq!(queued(&shared), vec![Some(1), Some(2)]);
    }

    #[test]
    fn control_commands_are_never_superseded() {
        let shared = test_shared();
        enqueue(&shared, SessionCommand::Visible(true));
        enqueue(&shared, SessionCommand::Capture(true));
        enqueue(&shared, SessionCommand::Visible(false));
        assert_eq!(queued(&shared), vec![None, None, None]);
    }

    #[test]
    fn a_closed_queue_fails_commands_rather_than_hanging() {
        let shared = test_shared();
        shared.commands.lock().unwrap().closed = true;
        let completion = enqueue(&shared, SessionCommand::Visible(true));
        assert!(queued(&shared).is_empty());
        let error = completion.wait().unwrap_err();
        assert!(error.to_string().contains("destroyed"));
    }

    #[test]
    fn destroying_an_already_ended_session_succeeds() {
        let shared = test_shared();
        shared.commands.lock().unwrap().closed = true;
        assert!(enqueue(&shared, SessionCommand::Destroy).wait().is_ok());
    }

    #[test]
    fn destroy_is_applied_by_the_actor_rather_than_short_circuited() {
        let shared = test_shared();
        let completion = enqueue(&shared, SessionCommand::Destroy);
        assert_eq!(queued(&shared), vec![None]);
        assert!(!shared.cancel.load(Ordering::Acquire));
        drop(completion);
    }

    #[test]
    fn a_completion_reports_the_applied_outcome_once() {
        let completion = Completion::new();
        completion.settle(Ok(()));
        assert!(completion.wait().is_ok());

        let failed = Completion::new();
        failed.settle(Err("host rejected the frame".to_owned()));
        let error = failed.wait().unwrap_err();
        assert!(error.to_string().contains("host rejected the frame"));
    }

    #[test]
    fn a_settled_completion_keeps_its_first_outcome() {
        let completion = Completion::new();
        completion.settle(Ok(()));
        completion.settle(Err("late".to_owned()));
        assert!(completion.wait().is_ok());
    }

    #[test]
    fn remaining_shrinks_toward_the_deadline_and_then_fails() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let first = remaining(deadline).unwrap();
        let second = remaining(deadline).unwrap();
        assert!(second <= first);
        assert!(first <= Duration::from_secs(30));
        assert!(remaining(Instant::now() - Duration::from_millis(1)).is_err());
    }
}
