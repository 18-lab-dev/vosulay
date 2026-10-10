pub mod bridge;
#[cfg(target_os = "linux")]
mod glib;
pub mod scheme;
mod strings;

use anyhow::{Context, Result, bail, ensure};
use cef::{args::Args, *};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc as Shared,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};
use vosulay_frame::{
    BrowserFrame, BrowserFrameBackend, BrowserInput, DamageRect, InputModifiers, KeyKind,
    MouseButton,
};

fn configure_api() {
    // These generated wrappers use the experimental layout from this exact
    // pinned build; never substitute another CEF patch-level distribution.
    assert!(
        !api_hash(cef::sys::CEF_API_VERSION, 0).is_null(),
        "CEF rejected the pinned binding API"
    );
}

pub struct RuntimeOptions<'a> {
    pub runtime: &'a Path,
    /// Windows: sandbox context supplied by the pinned CEF bootstrap.
    /// Linux: null; Chromium uses the user-namespace sandbox.
    pub sandbox_info: *mut u8,
    pub cache: &'a Path,
    pub resources: Arc<dyn scheme::ResourceService>,
}

pub struct BrowserOptions<'a> {
    pub url: &'a str,
    pub backend_url: &'a str,
    pub broker_url: Option<&'a str>,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub fps_limit: u32,
    pub shared_texture: bool,
    pub shared_array_buffer: bool,
}

struct View {
    width: u32,
    height: u32,
    scale: f32,
    closed: bool,
    error: Option<String>,
    console: Vec<String>,
    backend: Option<Box<dyn BrowserFrameBackend>>,
}

type ViewState = Shared<RefCell<View>>;
const PUMP_INTERVAL: Duration = Duration::from_millis(1000 / 30);

// Only callbacks cross threads. Keep real CEF work separate from the view's
// reference-pump fallback so a retained runtime has no idle rendering clock.
struct PumpState {
    requested: Option<Instant>,
    fallback: Option<Instant>,
}

struct PumpSchedule {
    state: Mutex<PumpState>,
    host: Thread,
}

impl PumpSchedule {
    fn new() -> Self {
        Self {
            state: Mutex::new(PumpState {
                requested: Some(Instant::now()),
                fallback: None,
            }),
            host: thread::current(),
        }
    }

    fn set_active(&self, active: bool) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fallback = active.then(Instant::now);
    }

    fn schedule(&self, delay_ms: i64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let delay = Duration::from_millis(delay_ms.max(0) as u64);
        let delay = if state.fallback.is_some() {
            delay.min(PUMP_INTERVAL)
        } else {
            delay
        };
        state.requested = Some(Instant::now() + delay);
        self.host.unpark();
    }

    fn deadline(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.requested.into_iter().chain(state.fallback).min()
    }

    fn pump(&self) {
        let due = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let due = state.requested.is_some_and(|deadline| deadline <= now)
                || state.fallback.is_some_and(|deadline| deadline <= now);
            if due {
                state.requested = None;
                if state.fallback.is_some() {
                    state.fallback = Some(now + PUMP_INTERVAL);
                }
            }
            due
        };
        if due {
            // Never hold the scheduling lock: CEF may synchronously reschedule.
            do_message_loop_work();
        }
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;

    #[test]
    fn final_view_close_removes_only_the_fallback_clock() {
        let schedule = PumpSchedule::new();
        let requested = Instant::now() + Duration::from_secs(60);
        schedule
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .requested = Some(requested);
        schedule.set_active(true);
        assert!(schedule.deadline().unwrap() < requested);
        schedule.set_active(false);
        assert_eq!(schedule.deadline(), Some(requested));
        schedule
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .requested = None;
        assert_eq!(schedule.deadline(), None);
    }

    #[test]
    fn detached_callbacks_keep_their_real_maintenance_delay() {
        let schedule = PumpSchedule::new();
        let before = Instant::now();
        schedule.schedule(60_000);
        assert!(schedule.deadline().unwrap() >= before + Duration::from_secs(60));
        schedule.set_active(true);
        schedule.schedule(60_000);
        let after = Instant::now();
        assert!(
            schedule
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .requested
                .unwrap()
                <= after + PUMP_INTERVAL
        );
    }
}

wrap_browser_process_handler! {
    struct Scheduling { schedule: Arc<PumpSchedule>, shared_array_buffer: Arc<AtomicBool> }
    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.schedule.schedule(delay_ms);
        }
        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            let Some(command_line) = command_line else { return };
            let enabled = self.shared_array_buffer.load(Ordering::Relaxed);
            let feature = "SharedArrayBufferUnrestrictedAccessAllowed";
            for (switch, include) in [
                ("enable-blink-features", enabled),
                ("disable-blink-features", !enabled),
            ] {
                let current = command_line.switch_value(Some(&switch.into()));
                let current = CefString::from(&current).to_string();
                let mut value = String::with_capacity(current.len() + feature.len() + 1);
                for item in current.split(',').filter(|item| !item.is_empty() && *item != feature) {
                    if !value.is_empty() {
                        value.push(',');
                    }
                    value.push_str(item);
                }
                if include {
                    if !value.is_empty() {
                        value.push(',');
                    }
                    value.push_str(feature);
                }
                command_line.append_switch_with_value(Some(&switch.into()), Some(&value.as_str().into()));
            }
        }
    }
}

wrap_app! {
    struct Application { renderer: RenderProcessHandler, shared_array_buffer: Option<Arc<AtomicBool>>, schedule: Option<Arc<PumpSchedule>> }
    impl App {
        fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                if command_line.has_switch(Some(&"benchmark-uncapped".into())) != 0 {
                    command_line.append_switch(Some(&"disable-frame-rate-limit".into()));
                    command_line.append_switch(Some(&"disable-gpu-vsync".into()));
                }
                command_line.append_switch(Some(&"no-startup-window".into()));
                // Windowless Chromium never holds Wayland keyboard focus, so a
                // Wayland compositor never offers it the clipboard and rejects
                // its selections. X11 lets any client read and own selections,
                // and Xwayland bridges them to Wayland clients.
                #[cfg(target_os = "linux")]
                if std::env::var_os("DISPLAY").is_some_and(|display| !display.is_empty())
                    && command_line.has_switch(Some(&"ozone-platform".into())) == 0
                {
                    command_line.append_switch_with_value(Some(&"ozone-platform".into()), Some(&"x11".into()));
                }
                #[cfg(windows)]
                {
                    // Native attachment uses the host's selected privileges.
                    // Keep that token; CEF subprocess sandboxing is unchanged.
                    command_line.append_switch(Some(&"do-not-de-elevate".into()));
                }
                // Embedded application: no standalone Chrome first-run UI.
                // CEF's BSD terms are shipped with the runtime package.
                command_line.append_switch(Some(&"no-first-run".into()));
                // One composed application page: no speculative spare renderer
                // or Chrome's periodic stack sampler in the normal runtime.
                // Site isolation, GPU acceleration, and sandboxing stay enabled.
                command_line.append_switch(Some(&"disable-stack-profiler".into()));
                let disable_features: CefString = "disable-features".into();
                let current = command_line.switch_value(Some(&disable_features));
                let mut disabled = CefString::from(&current).to_string();
                for feature in ["SpareRendererForSitePerProcess"] {
                    if !disabled.split(',').any(|value| value == feature) {
                        if !disabled.is_empty() {
                            disabled.push(',');
                        }
                        disabled.push_str(feature);
                    }
                }
                command_line.append_switch_with_value(Some(&disable_features), Some(&disabled.as_str().into()));
                command_line.append_switch_with_value(Some(&"autoplay-policy".into()), Some(&"no-user-gesture-required".into()));
                if let Some(enabled) = self.shared_array_buffer.as_ref() {
                    let switch = if enabled.load(Ordering::Relaxed) { "enable-blink-features" } else { "disable-blink-features" };
                    command_line.append_switch_with_value(Some(&switch.into()), Some(&"SharedArrayBufferUnrestrictedAccessAllowed".into()));
                }
            }
        }
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if !scheme::register_scheme(registrar) { eprintln!("CEF rejected vosulay scheme registration"); }
        }
        fn render_process_handler(&self) -> Option<RenderProcessHandler> { Some(self.renderer.clone()) }
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            self.schedule.as_ref().zip(self.shared_array_buffer.as_ref())
                .map(|(schedule, enabled)| Scheduling::new(schedule.clone(), enabled.clone()))
        }
    }
}

fn paint_damage(rects: Option<&[Rect]>, width: u32, height: u32) -> Option<DamageRect> {
    let mut damage: Option<DamageRect> = None;
    for rect in rects? {
        let left = i64::from(rect.x).max(0);
        let top = i64::from(rect.y).max(0);
        let right = (i64::from(rect.x) + i64::from(rect.width)).min(i64::from(width));
        let bottom = (i64::from(rect.y) + i64::from(rect.height)).min(i64::from(height));
        // An empty or invalid damage report requires the complete snapshot.
        if right <= left || bottom <= top {
            return None;
        }
        let rect = DamageRect {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        };
        damage = Some(damage.map_or(rect, |previous| previous.union(rect)));
    }
    damage
}

wrap_render_handler! {
    struct Painting { state: ViewState }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let state = self.state.borrow();
                rect.width = state.width as i32;
                rect.height = state.height as i32;
            }
        }
        fn screen_info(&self, _browser: Option<&mut Browser>, screen_info: Option<&mut ScreenInfo>) -> i32 {
            let Some(info) = screen_info else { return 0 };
            let state = self.state.borrow();
            info.device_scale_factor = state.scale;
            info.depth = 32;
            info.depth_per_component = 8;
            info.rect = Rect { x: 0, y: 0, width: state.width as i32, height: state.height as i32 };
            info.available_rect = info.rect.clone();
            1
        }
        fn on_paint(&self, _browser: Option<&mut Browser>, type_: PaintElementType, dirty_rects: Option<&[Rect]>, buffer: *const u8, width: i32, height: i32) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 { return; }
            let Some(length) = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4)) else { return };
            // CEF provides a complete BGRA view valid only until this callback returns.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, length) };
            let mut state = self.state.borrow_mut();
            let Some(backend) = state.backend.as_mut() else { return };
            let damage = paint_damage(dirty_rects, width as u32, height as u32);
            if let Err(error) = backend.submit_frame(BrowserFrame::Bgra { pixels, width: width as u32, height: height as u32, damage }) {
                state.error = Some(error.to_string());
            }
        }
        #[cfg(windows)]
        fn on_accelerated_paint(&self, _browser: Option<&mut Browser>, type_: PaintElementType, _dirty_rects: Option<&[Rect]>, info: Option<&AcceleratedPaintInfo>) {
            if type_ != PaintElementType::VIEW { return; }
            let Some(info) = info else { return };
            let mut state = self.state.borrow_mut();
            let rect = &info.extra.visible_rect;
            if rect.x < 0 || rect.y < 0 || rect.width <= 0 || rect.height <= 0 {
                state.error = Some("CEF supplied invalid visible texture geometry".into());
                return;
            }
            let frame = BrowserFrame::D3d11 {
                handle: info.shared_texture_handle, width: rect.width as u32, height: rect.height as u32,
                source_x: rect.x as u32, source_y: rect.y as u32,
            };
            let Some(backend) = state.backend.as_mut() else { return };
            if let Err(error) = backend.submit_frame(frame) { state.error = Some(error.to_string()); }
        }
    }
}

wrap_life_span_handler! {
    struct Lifetime { state: ViewState }
    impl LifeSpanHandler {
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, _target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: i32, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 { 1 }
        fn on_before_close(&self, _browser: Option<&mut Browser>) { self.state.borrow_mut().closed = true; }
    }
}

wrap_display_handler! {
    struct Console { state: ViewState }
    impl DisplayHandler {
        fn on_console_message(&self, _browser: Option<&mut Browser>, _level: LogSeverity, message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            if let Some(message) = message {
                let message = message.to_string();
                eprintln!("CEF: {message}");
                let mut state = self.state.borrow_mut();
                // Diagnostics are bounded even if an untrusted counter floods console.
                if state.console.len() < 128 { state.console.push(message); }
            }
            1
        }
    }
}

wrap_client! {
    struct ClientHandlers { state: ViewState, inbox: bridge::BridgeInbox }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> { Some(Painting::new(self.state.clone())) }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(Lifetime::new(self.state.clone())) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(Console::new(self.state.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(FailureHandler::new(self.state.clone())) }
        fn permission_handler(&self) -> Option<PermissionHandler> { Some(NetworkAccess::new()) }
        fn on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            if self.state.borrow().closed { return 1; }
            self.inbox.dispatch(browser, frame, source_process, message)
        }
    }
}

wrap_permission_handler! {
    struct NetworkAccess;
    impl PermissionHandler {
        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, _prompt_id: u64, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            let (Some(origin), Some(callback)) = (requesting_origin, callback) else { return 0 };
            let Some(origin) = origin.as_slice() else { return 0 };
            if !manager_network_permission(origin, requested_permissions) { return 0; }
            // Chromium's local-network gate applies even to iframe navigation.
            // Only the embedded manager may load user-configured local counters;
            // arbitrary counter origins and unrelated permissions keep defaults.
            callback.cont(PermissionRequestResult::ACCEPT);
            1
        }
    }
}

// The pinned CEF bindings use signed permission flags on Windows, unsigned on Linux.
fn permission_bits(permission: PermissionRequestTypes) -> u32 {
    #[cfg(windows)]
    {
        permission.get_raw() as u32
    }
    #[cfg(not(windows))]
    {
        permission.get_raw()
    }
}

fn manager_network_permission(origin: &[u16], permissions: u32) -> bool {
    let network = permission_bits(PermissionRequestTypes::LOCAL_NETWORK)
        | permission_bits(PermissionRequestTypes::LOOPBACK_NETWORK);
    let origin = origin.strip_suffix(&[u16::from(b'/')]).unwrap_or(origin);
    permissions != 0
        && permissions & !network == 0
        && origin
            .iter()
            .copied()
            .eq("vosulay://manager".encode_utf16())
}

wrap_request_handler! {
    struct FailureHandler { state: ViewState }
    impl RequestHandler {
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, _status: TerminationStatus, _error_code: i32, _error_string: Option<&CefString>) {
            self.state.borrow_mut().error = Some("CEF renderer process terminated".to_owned());
        }
    }
}

/// CEF subprocess dispatch must precede product CLI parsing and initialization.
///
/// # Safety
/// On Windows, sandbox_info must be the live context supplied by CEF's bootstrap.
/// On Linux, pass null. Call on the process's main thread.
pub unsafe fn execute_subprocess(sandbox_info: *mut u8) -> Option<i32> {
    configure_api();
    let args = Args::new();
    let mut app = Application::new(bridge::renderer_handler(), None, None);
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), sandbox_info);
    (code >= 0).then_some(code)
}

struct RuntimeState {
    schedule: Arc<PumpSchedule>,
    #[cfg(target_os = "linux")]
    glib: RefCell<Option<glib::GlibPump>>,
    shared_array_buffer: Arc<AtomicBool>,
    active: Cell<bool>,
    poisoned: Cell<bool>,
    _app: App,
}

impl RuntimeState {
    fn pump(&self) {
        self.schedule.pump();
        #[cfg(target_os = "linux")]
        if let Ok(mut glib) = self.glib.try_borrow_mut()
            && let Some(glib) = glib.as_mut()
        {
            glib.pump();
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let deadline = self.schedule.deadline();
        #[cfg(target_os = "linux")]
        let deadline = match (
            deadline,
            self.glib
                .borrow()
                .as_ref()
                .and_then(glib::GlibPump::deadline),
        ) {
            (Some(cef), Some(glib)) => Some(cef.min(glib)),
            (cef, glib) => cef.or(glib),
        };
        deadline
    }
}

impl Drop for RuntimeState {
    fn drop(&mut self) {
        // A timed-out native close must never shut down CEF with live objects.
        if !self.poisoned.get() {
            shutdown();
        }
    }
}

pub struct CefRuntime {
    state: Shared<RuntimeState>,
}

impl CefRuntime {
    /// # Safety
    /// Initialize once on the main thread after subprocess dispatch. Windows
    /// requires the live matching bootstrap sandbox context.
    pub unsafe fn initialize(options: RuntimeOptions<'_>) -> Result<Self> {
        static INITIALIZED: AtomicBool = AtomicBool::new(false);
        ensure!(
            !INITIALIZED.swap(true, Ordering::Relaxed),
            "CEF cannot be initialized twice in one process"
        );
        configure_api();
        #[cfg(windows)]
        ensure!(
            !options.sandbox_info.is_null(),
            "Windows CEF requires the packaged sandbox bootstrap"
        );
        std::fs::create_dir_all(options.cache)?;
        let cache = options.cache.canonicalize()?;
        let runtime = options.runtime.canonicalize()?;
        let resources = if runtime.join("Resources").is_dir() {
            runtime.join("Resources")
        } else {
            runtime
        };
        let schedule = Arc::new(PumpSchedule::new());
        let shared_array_buffer = Arc::new(AtomicBool::new(false));
        let mut app = Application::new(
            bridge::renderer_handler(),
            Some(shared_array_buffer.clone()),
            Some(schedule.clone()),
        );
        let args = Args::new();
        let settings = Settings {
            windowless_rendering_enabled: 1,
            external_message_pump: 1,
            root_cache_path: cache.to_string_lossy().as_ref().into(),
            cache_path: cache.to_string_lossy().as_ref().into(),
            resources_dir_path: resources.to_string_lossy().as_ref().into(),
            locales_dir_path: resources.join("locales").to_string_lossy().as_ref().into(),
            log_file: cache.join("cef.log").to_string_lossy().as_ref().into(),
            background_color: 0,
            ..Default::default()
        };
        if initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            options.sandbox_info,
        ) != 1
        {
            bail!(
                "CEF initialization failed (code {}); see {}",
                get_exit_code(),
                cache.join("cef.log").display()
            );
        }
        if !scheme::install_scheme(options.resources) {
            shutdown();
            bail!("CEF rejected vosulay scheme handler");
        }
        Ok(Self {
            state: Shared::new(RuntimeState {
                #[cfg(target_os = "linux")]
                glib: RefCell::new(glib::GlibPump::new(schedule.host.clone())),
                schedule,
                shared_array_buffer,
                active: Cell::new(false),
                poisoned: Cell::new(false),
                _app: app,
            }),
        })
    }

    pub fn next_pump_deadline(&self) -> Option<Instant> {
        self.state.deadline()
    }

    pub fn pump(&self) {
        self.state.pump();
    }

    /// Open a fresh view on this runtime's main thread, without reinitializing CEF.
    pub fn open(
        &self,
        options: BrowserOptions<'_>,
        mut backend: Box<dyn BrowserFrameBackend>,
    ) -> Result<OsrBrowser> {
        ensure!(
            !self.state.poisoned.get(),
            "CEF runtime has a failed native close"
        );
        ensure!(
            !self.state.active.get(),
            "CEF runtime already has an OSR view"
        );
        ensure!(
            options.width > 0
                && options.height > 0
                && options.width <= 16384
                && options.height <= 16384,
            "invalid browser extent"
        );
        ensure!(
            options.scale.is_finite() && options.scale > 0.0,
            "invalid browser scale"
        );
        ensure!(
            (1..=i32::MAX as u32).contains(&options.fps_limit),
            "invalid browser FPS limit"
        );
        ensure!(
            !options.shared_texture || cfg!(windows),
            "shared-texture OSR is Windows-only"
        );
        backend.resize(options.width, options.height, options.scale)?;
        self.state
            .shared_array_buffer
            .store(options.shared_array_buffer, Ordering::Relaxed);
        let state = Shared::new(RefCell::new(View {
            width: options.width,
            height: options.height,
            scale: options.scale,
            closed: false,
            error: None,
            console: Vec::new(),
            backend: Some(backend),
        }));
        let inbox = bridge::BridgeInbox::new();
        let mut client = ClientHandlers::new(state.clone(), inbox.clone());
        let window = WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: i32::from(options.shared_texture),
            // Let CEF's damage-driven OSR clock apply the configured frame rate.
            ..Default::default()
        };
        let mut extra_info = bridge::renderer_extra_info(options.backend_url, options.broker_url)
            .context("invalid HTTP(S) backend or broker URL")?;
        let browser_settings = BrowserSettings {
            background_color: 0,
            windowless_frame_rate: options.fps_limit as i32,
            ..Default::default()
        };
        let browser = browser_host_create_browser_sync(
            Some(&window),
            Some(&mut client),
            Some(&options.url.into()),
            Some(&browser_settings),
            Some(&mut extra_info),
            None,
        );
        let Some(browser) = browser else {
            bail!("CEF rejected OSR browser creation");
        };
        self.state.active.set(true);
        self.state.schedule.set_active(true);
        Ok(OsrBrowser {
            browser: Some(browser),
            state,
            inbox,
            runtime: self.state.clone(),
            _client: client,
        })
    }
}

pub struct OsrBrowser {
    browser: Option<Browser>,
    state: ViewState,
    inbox: bridge::BridgeInbox,
    _client: Client,
    runtime: Shared<RuntimeState>,
}

impl OsrBrowser {
    pub fn take_console(&self) -> Vec<String> {
        std::mem::take(&mut self.state.borrow_mut().console)
    }

    pub fn next_message(&self) -> Option<String> {
        self.inbox.next_message()
    }

    pub fn send_input(&self, input: BrowserInput) {
        let Some(host) = self.browser.as_ref().and_then(ImplBrowser::host) else {
            return;
        };
        let flags = |m: InputModifiers| -> u32 {
            u32::from(m.caps_lock)
                | (u32::from(m.shift) << 1)
                | (u32::from(m.control) << 2)
                | (u32::from(m.alt) << 3)
                | (u32::from(m.left_button) << 4)
                | (u32::from(m.middle_button) << 5)
                | (u32::from(m.right_button) << 6)
                | (u32::from(m.meta) << 7)
                | (u32::from(m.num_lock) << 8)
                | (u32::from(m.keypad) << 9)
                | (u32::from(m.left_key) << 10)
                | (u32::from(m.right_key) << 11)
        };
        match input {
            BrowserInput::Move {
                x,
                y,
                modifiers,
                leave,
            } => host.send_mouse_move_event(
                Some(&MouseEvent {
                    x,
                    y,
                    modifiers: flags(modifiers),
                }),
                i32::from(leave),
            ),
            BrowserInput::Button {
                x,
                y,
                modifiers,
                button,
                up,
                click_count,
            } => host.send_mouse_click_event(
                Some(&MouseEvent {
                    x,
                    y,
                    modifiers: flags(modifiers),
                }),
                match button {
                    MouseButton::Left => MouseButtonType::LEFT,
                    MouseButton::Middle => MouseButtonType::MIDDLE,
                    MouseButton::Right => MouseButtonType::RIGHT,
                },
                i32::from(up),
                click_count,
            ),
            BrowserInput::Wheel {
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            } => host.send_mouse_wheel_event(
                Some(&MouseEvent {
                    x,
                    y,
                    modifiers: flags(modifiers),
                }),
                delta_x,
                delta_y,
            ),
            BrowserInput::Key {
                kind,
                modifiers,
                windows_key_code,
                native_key_code,
                is_system_key,
                character,
                unmodified_character,
            } => host.send_key_event(Some(&KeyEvent {
                size: std::mem::size_of::<cef::sys::cef_key_event_t>(),
                type_: match kind {
                    KeyKind::RawDown => KeyEventType::RAWKEYDOWN,
                    KeyKind::Up => KeyEventType::KEYUP,
                    KeyKind::Character => KeyEventType::CHAR,
                },
                modifiers: flags(modifiers),
                windows_key_code,
                native_key_code,
                is_system_key: i32::from(is_system_key),
                character,
                unmodified_character,
                focus_on_editable_field: 0,
            })),
        }
    }

    pub fn set_focus(&self, focused: bool) {
        if let Some(host) = self.browser.as_ref().and_then(ImplBrowser::host) {
            host.set_focus(i32::from(focused));
        }
    }

    pub fn reload(&self) {
        if let Some(browser) = self.browser.as_ref() {
            browser.reload();
        }
    }

    pub fn set_frame_rate(&self, fps: u32) -> Result<()> {
        ensure!(
            (1..=i32::MAX as u32).contains(&fps),
            "invalid browser FPS limit"
        );
        let host = self
            .browser
            .as_ref()
            .and_then(ImplBrowser::host)
            .context("browser has no host")?;
        host.set_windowless_frame_rate(fps as i32);
        Ok(())
    }

    pub fn next_pump_deadline(&self) -> Option<Instant> {
        self.runtime.deadline()
    }

    pub fn pump(&self) -> Result<()> {
        self.runtime.pump();
        let state = self.state.borrow();
        if let Some(error) = state.error.as_ref() {
            bail!("CEF: {error}");
        }
        if state.closed {
            bail!("CEF browser closed unexpectedly");
        }
        Ok(())
    }

    pub fn resize(&self, width: u32, height: u32, scale: f32) -> Result<()> {
        ensure!(
            width > 0
                && height > 0
                && width <= 16384
                && height <= 16384
                && scale.is_finite()
                && scale > 0.0,
            "invalid browser resize"
        );
        let mut state = self.state.borrow_mut();
        state
            .backend
            .as_mut()
            .context("browser backend is closed")?
            .resize(width, height, scale)?;
        state.width = width;
        state.height = height;
        state.scale = scale;
        drop(state);
        if let Some(host) = self.browser.as_ref().and_then(ImplBrowser::host) {
            host.notify_screen_info_changed();
            host.was_resized();
        }
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> Result<()> {
        self.state
            .borrow_mut()
            .backend
            .as_mut()
            .context("browser backend is closed")?
            .set_visible(visible)?;
        if let Some(host) = self.browser.as_ref().and_then(ImplBrowser::host) {
            host.was_hidden(i32::from(!visible));
        }
        Ok(())
    }

    pub fn evaluate(&self, script: &str) {
        if let Some(frame) = self.browser.as_ref().and_then(ImplBrowser::main_frame) {
            frame.execute_java_script(Some(&script.into()), None, 0);
        }
    }

    pub fn close(mut self) -> Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<()> {
        let Some(browser) = self.browser.take() else {
            return Ok(());
        };
        let Some(host) = browser.host() else {
            self.runtime.poisoned.set(true);
            std::mem::forget(browser);
            bail!("browser has no host");
        };
        host.close_browser(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.state.borrow().closed {
            self.runtime.pump();
            if self.state.borrow().closed {
                break;
            }
            if Instant::now() >= deadline {
                // Do not call cef_shutdown with live browser objects. Bounded process
                // exit releases platform capture; callers treat this as fatal.
                std::mem::forget(browser);
                self.runtime.poisoned.set(true);
                bail!("CEF browser did not close within five seconds");
            }
            let next = self.runtime.deadline().unwrap_or(deadline).min(deadline);
            thread::park_timeout(next.saturating_duration_since(Instant::now()));
        }
        drop(browser);
        self.state.borrow_mut().backend.take();
        while self.inbox.next_message().is_some() {}
        self.runtime.active.set(false);
        self.runtime.schedule.set_active(false);
        Ok(())
    }
}

impl Drop for OsrBrowser {
    fn drop(&mut self) {
        if let Err(error) = self.close_inner() {
            eprintln!("{error:#}");
        }
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[test]
    fn local_network_access_is_limited_to_manager_and_network_permissions() {
        let local = permission_bits(PermissionRequestTypes::LOCAL_NETWORK);
        let loopback = permission_bits(PermissionRequestTypes::LOOPBACK_NETWORK);
        let camera = permission_bits(PermissionRequestTypes::CAMERA_STREAM);
        let allowed = |origin: &str, permissions| {
            manager_network_permission(&origin.encode_utf16().collect::<Vec<_>>(), permissions)
        };
        assert!(allowed("vosulay://manager/", loopback));
        assert!(allowed("vosulay://manager", local | loopback));
        for origin in [
            "http://127.0.0.1:24050/",
            "https://manager/",
            "vosulay://manager.evil/",
            "vosulay://other/",
            "vosulay://manager/path",
            "vosulay://manager/?spoof",
            "vosulay://user@manager/",
        ] {
            assert!(!allowed(origin, loopback), "{origin}");
        }
        assert!(!allowed("vosulay://manager/", 0));
        assert!(!allowed("vosulay://manager/", camera));
        assert!(!allowed("vosulay://manager/", loopback | camera));
    }
}
