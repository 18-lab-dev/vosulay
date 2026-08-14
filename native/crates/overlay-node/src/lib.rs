use napi::bindgen_prelude::{AsyncTask, Buffer, Task, Unknown};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Result, Status};
use napi_derive::napi;
use overlay_host::{Host, HostEvent, HostSession};
use overlay_protocol::{
    CAP_DMABUF, CAP_SHM_BGRA, DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, DmabufDescriptor,
    DmabufPlane, TEXTURE_FORMAT_PREMULTIPLIED_BGRA8, Target, TextureDescriptor,
};
use std::{
    collections::{HashMap, VecDeque},
    ffi::{OsStr, OsString},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
fn fail<E: std::fmt::Display>(e: E) -> Error {
    Error::new(Status::GenericFailure, e.to_string())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

/// Geometry for a CPU frame; the buffer length defines its size, so no offset
/// or descriptor fd is accepted from the renderer.
#[napi(object)]
pub struct BitmapDescriptor {
    pub target_id: i64,
    pub width: i64,
    pub height: i64,
    pub stride: i64,
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
    state: Mutex<Lifecycle>,
    cancel: Arc<AtomicBool>,
    events: Mutex<EventState>,
}
enum Lifecycle {
    Connected(Arc<Mutex<HostSession>>),
    Destroyed,
}
impl Shared {
    fn with_session<T>(&self, f: impl FnOnce(&mut HostSession) -> Result<T>) -> Result<T> {
        let state = self.state.lock().map_err(fail)?;
        match &*state {
            Lifecycle::Connected(session) => {
                let mut session = session.lock().map_err(fail)?;
                f(&mut session)
            }
            Lifecycle::Destroyed => Err(fail("overlay handle is destroyed")),
        }
    }
}
enum SessionCommand {
    Visible(bool),
    Capture(bool),
    /// Already-sealed SHM fd (test/update_descriptor path).
    SurfaceFd {
        descriptor: TextureDescriptor,
        fd: OwnedFd,
    },
    /// Premultiplied BGRA bytes. Sealed memfd is created on the worker thread
    /// so a multi-megabyte write does not block Chromium's paint callback.
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

pub struct SessionTask {
    shared: Arc<Shared>,
    command: Option<SessionCommand>,
}

impl Task for SessionTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        let command = self
            .command
            .take()
            .ok_or_else(|| fail("task already run"))?;
        match command {
            SessionCommand::Visible(value) => self
                .shared
                .with_session(|session| session.set_visible(value).map_err(fail)),
            SessionCommand::Capture(value) => self
                .shared
                .with_session(|session| session.set_input_capture(value).map_err(fail)),
            SessionCommand::SurfaceFd { descriptor, fd } => self.shared.with_session(|session| {
                session
                    .ensure_target(Target {
                        target_id: descriptor.target_id,
                        width: descriptor.width,
                        height: descriptor.height,
                    })
                    .map_err(fail)?;
                session
                    .send_shm(descriptor.target_id, descriptor, fd)
                    .map_err(fail)
            }),
            SessionCommand::SurfaceBytes { descriptor, bytes } => {
                let fd = sealed_memfd(&bytes)?;
                self.shared.with_session(|session| {
                    session
                        .ensure_target(Target {
                            target_id: descriptor.target_id,
                            width: descriptor.width,
                            height: descriptor.height,
                        })
                        .map_err(fail)?;
                    session
                        .send_shm(descriptor.target_id, descriptor, fd)
                        .map_err(fail)
                })
            }
            SessionCommand::Dmabuf { descriptor, fds } => self.shared.with_session(|session| {
                session
                    .ensure_target(Target {
                        target_id: descriptor.target_id,
                        width: descriptor.width,
                        height: descriptor.height,
                    })
                    .map_err(fail)?;
                session
                    .send_dmabuf(descriptor.target_id, descriptor, fds)
                    .map_err(fail)
            }),
            SessionCommand::Clear(target_id) => self
                .shared
                .with_session(|session| session.clear(target_id).map_err(fail)),
            SessionCommand::Destroy => {
                let mut state = self.shared.state.lock().map_err(fail)?;
                if let Lifecycle::Connected(session) =
                    std::mem::replace(&mut *state, Lifecycle::Destroyed)
                    && let Ok(mut session) = session.lock()
                {
                    let _ = session.shutdown();
                }
                self.shared.cancel.store(true, Ordering::Release);
                Ok(())
            }
        }
    }

    fn resolve(&mut self, _env: napi::Env, _: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

fn queue(shared: &Arc<Shared>, command: SessionCommand) -> AsyncTask<SessionTask> {
    AsyncTask::new(SessionTask {
        shared: shared.clone(),
        command: Some(command),
    })
}
#[napi]
pub struct OverlaySurface {
    shared: Arc<Shared>,
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
        if let Ok(mut state) = self.shared.state.lock()
            && !matches!(*state, Lifecycle::Destroyed)
        {
            *state = Lifecycle::Destroyed;
        }
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
    /// Frame transports the attached target can actually use. The Electron host
    /// picks shared textures or CPU bitmaps from this.
    #[napi]
    pub fn supports_dmabuf(&self) -> bool {
        self.capabilities & CAP_DMABUF != 0
    }
    #[napi]
    pub fn supports_bgra_shm(&self) -> bool {
        self.capabilities & CAP_SHM_BGRA != 0
    }
    #[napi]
    pub fn destroy(&self) -> AsyncTask<SessionTask> {
        self.shared.cancel.store(true, Ordering::Release);
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
            "disconnect" | "input" | "end" | "consumed" | "frame" | "target"
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
            shared: self.shared.clone(),
        })
    }
    #[napi]
    pub fn poll_event(&self) -> Result<String> {
        self.shared.with_session(|session| {
            let event = session.recv().map_err(fail)?;
            Ok(format!("{event:?}"))
        })
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
        let d = TextureDescriptor {
            target_id: u64::try_from(descriptor.target_id).map_err(fail)?,
            width: u32::try_from(descriptor.width).map_err(fail)?,
            height: u32::try_from(descriptor.height).map_err(fail)?,
            stride: u32::try_from(descriptor.stride).map_err(fail)?,
            offset: u64::try_from(descriptor.offset).map_err(fail)?,
            size: u64::try_from(descriptor.size).map_err(fail)?,
            format: u32::try_from(descriptor.format.unwrap_or(0)).map_err(fail)?,
        };
        d.validate().map_err(fail)?;
        let raw = RawFd::try_from(
            descriptor
                .fd
                .ok_or_else(|| fail("descriptor requires fd"))?,
        )
        .map_err(fail)?;
        if raw < 0 {
            return Err(fail("descriptor fd is negative"));
        }
        let dup = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            return Err(fail(std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(dup) };
        Ok(queue(
            &self.shared,
            SessionCommand::SurfaceFd { descriptor: d, fd },
        ))
    }

    /// Shared-memory path for targets that cannot sample an imported DMA-BUF.
    /// The bitmap is Chromium's premultiplied BGRA, copied into a sealed memfd
    /// so the agent maps a buffer the host can no longer resize.
    #[napi]
    pub fn update_bitmap(
        &self,
        descriptor: BitmapDescriptor,
        bitmap: Buffer,
    ) -> Result<AsyncTask<SessionTask>> {
        let d = TextureDescriptor {
            target_id: u64::try_from(descriptor.target_id).map_err(fail)?,
            width: u32::try_from(descriptor.width).map_err(fail)?,
            height: u32::try_from(descriptor.height).map_err(fail)?,
            stride: u32::try_from(descriptor.stride).map_err(fail)?,
            offset: 0,
            size: u64::try_from(bitmap.len()).map_err(fail)?,
            format: TEXTURE_FORMAT_PREMULTIPLIED_BGRA8,
        };
        d.validate().map_err(fail)?;
        // Move the N-API buffer onto the worker; sealing writes it once there.
        Ok(queue(
            &self.shared,
            SessionCommand::SurfaceBytes {
                descriptor: d,
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
            let dup = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                return Err(fail(std::io::Error::last_os_error()));
            }
            fds.push(unsafe { OwnedFd::from_raw_fd(dup) });
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
        let id = u64::try_from(target_id).map_err(fail)?;
        Ok(queue(&self.shared, SessionCommand::Clear(id)))
    }
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(value, 16).map_err(|_| fail(format!("invalid {field}")))
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

/// Receive authenticated agent events off the Node thread.
fn start_event_pump(shared: Arc<Shared>, session: Arc<Mutex<HostSession>>) {
    thread::spawn(move || {
        let mut ping_sequence = 1u64;
        let mut last_ping = Instant::now();
        loop {
            if shared.cancel.load(Ordering::Acquire) {
                break;
            }
            let event = {
                let mut guard = match session.lock() {
                    Ok(guard) => guard,
                    Err(_) => break,
                };
                if last_ping.elapsed() >= Duration::from_millis(500) {
                    if guard.ping(ping_sequence).is_err() {
                        emit_event(&shared, "disconnect", "disconnect".to_owned());
                        break;
                    }
                    ping_sequence = ping_sequence.wrapping_add(1);
                    last_ping = Instant::now();
                }
                match guard.recv() {
                    Ok(event) => Some(event),
                    Err(overlay_host::HostError::Protocol(
                        overlay_protocol::ProtocolError::Io(error),
                    )) if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                    {
                        None
                    }
                    Err(_) => {
                        emit_event(&shared, "disconnect", "disconnect".to_owned());
                        break;
                    }
                }
            };
            if let Some(event) = event {
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
            } else {
                thread::sleep(Duration::from_millis(10));
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

    fn run(&self, pid: u32, socket_name: &str, cookie: &str) -> Result<()> {
        let mut command = if self.tool.ends_with(".dll") {
            let mut command = Command::new("dotnet");
            command.arg(&self.tool);
            command
        } else {
            Command::new(&self.tool)
        };
        command
            .envs(std::env::vars())
            .env("VOSULAY_ATTACH_TARGET_PID", pid.to_string());
        let output = command
            .arg(pid.to_string())
            .arg(&self.profiler)
            .arg(&self.agent)
            .arg(socket_name)
            .arg(cookie)
            .arg(std::process::id().to_string())
            .output()
            .map_err(fail)?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        Err(fail(format!(
            "profiler attach exited {}: {}",
            output.status,
            detail.trim()
        )))
    }
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
        let host = Host::new(self.pid).map_err(fail)?;
        let socket_name = hex(host.endpoint().name());
        let cookie = hex(&host.cookie());
        if let Some(attach) = &self.attach {
            attach.run(self.pid, &socket_name, &cookie)?;
        }
        connect_handle(host, socket_name, cookie, self.timeout)
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
        let child = command.spawn().map_err(fail)?;
        let pid = child.id();
        host.set_expected_process_group(pid);
        connect_handle(
            host,
            socket_name,
            cookie,
            Duration::from_secs(u64::from(options.timeout.unwrap_or(30).clamp(1, 120))),
        )
    }

    fn resolve(&mut self, _env: Env, handle: Self::Output) -> Result<Self::JsValue> {
        Ok(handle)
    }
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

/// The dynamic loader splits `LD_PRELOAD` on colons and whitespace, so a path
/// containing either separator cannot be expressed and must be rejected.
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

/// Frames are copied into a memfd sealed against resizing so a compromised or
/// buggy target cannot make the agent's mapping outlive the buffer's length.
fn sealed_memfd(bytes: &[u8]) -> Result<OwnedFd> {
    // SAFETY: the name is a static NUL-terminated string.
    let raw = unsafe {
        libc::memfd_create(
            c"vosulay-overlay-frame".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(fail(std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a fresh owned descriptor from memfd_create.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: the slice range is in bounds for the remaining bytes.
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
    // SAFETY: sealing a memfd this process owns exclusively.
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
) -> Result<OverlayHandle> {
    let cancel = Arc::new(AtomicBool::new(false));
    let session = host
        .accept_cancellable_timeout(&cancel, timeout)
        .map_err(fail)?;
    let pid = session.peer_pid();
    let capabilities = session.capabilities();
    session.set_nonblocking().map_err(fail)?;
    let session = Arc::new(Mutex::new(session));
    let shared = Arc::new(Shared {
        state: Mutex::new(Lifecycle::Connected(session.clone())),
        cancel,
        events: Mutex::new(EventState {
            callbacks: HashMap::new(),
            pending: VecDeque::with_capacity(MAX_PENDING_EVENTS),
        }),
    });
    start_event_pump(shared.clone(), session);
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
    use super::{prepend_layer_name, prepend_preload, prepend_search_path};
    use std::{ffi::OsStr, path::Path};

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
}
