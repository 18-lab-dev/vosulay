use super::{
    Options, ProductResources,
    editor::{EditorWorker, Work},
    receive,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use vosulay_browser::{
    BrowserOptions, CefRuntime, OsrBrowser, RuntimeOptions, execute_subprocess, scheme::MANAGER_URL,
};
use vosulay_frame::{BrowserInput, KeyKind, PlatformEvent, Viewport};
#[cfg(target_os = "linux")]
use vosulay_linux::{LinuxController as PlatformController, LinuxOptions};
#[cfg(windows)]
use vosulay_windows::{WindowsController as PlatformController, WindowsOptions};

/// # Safety
/// Main-thread entry. Windows supplies the live packaged CEF sandbox context;
/// Linux supplies null and uses the packaged CEF subprocess executable.
pub unsafe fn run(sandbox_info: *mut u8) -> Result<i32> {
    if let Some(code) = unsafe { execute_subprocess(sandbox_info) } {
        return Ok(code);
    }
    let Some(options) = Options::parse()? else {
        return Ok(0);
    };
    let mut config = crate::config::load_from_path(&options.config)?;
    #[cfg(target_os = "linux")]
    super::launchers::refresh(&options.overlay, &options.lookup);
    super::tosu::migrate(&mut config, &options.config);
    let resources = Arc::new(ProductResources::new(&config)?);
    let remote = crate::remote::RemoteServer::start_from_path(
        options.backend.clone(),
        options.config.clone(),
    )
    .context("could not start overlay backend broker")?;
    #[cfg(windows)]
    let mut platform = PlatformController::start(WindowsOptions {
        dll_dir: options.runtime.clone(),
        pids: options.pids.clone(),
        process_names: options.process_names.clone(),
    })?;
    #[cfg(target_os = "linux")]
    let mut platform = PlatformController::start(LinuxOptions {
        library: options.overlay.clone(),
        pids: options.pids.clone(),
        process_names: options.process_names.clone(),
    })?;
    let mut worker = EditorWorker::start(
        config.clone(),
        options.config.clone(),
        options.backend.clone(),
        resources.clone(),
    )?;
    let initial_fps = super::frame_rate(options.fps_limit, config.settings.fps_limit, 60.0);
    let mut host = Host {
        browser: None,
        runtime: None,
        sandbox_info,
        resources,
        broker_url: remote.browser_url().to_string(),
        platform: &mut platform,
        worker: &mut worker,
        options: &options,
        document: String::new(),
        ready: false,
        attached: false,
        editing: false,
        visible: true,
        dialog: false,
        next_ping: Instant::now(),
        nonce: 0,
        pending_ping: None,
        closing: None,
        persisted_fps_limit: config.settings.fps_limit,
        refresh_hz: 60.0,
        effective_fps: initial_fps,
        consumed: [false; 256],
        viewport: Viewport {
            width: 640,
            height: 480,
            scale: 1.0,
            refresh_hz: 60.0,
        },
        shared_array_buffer: config.settings.allow_unrestricted_shared_array_buffer,
        pending_detach: false,
        flushed: None,
    };
    let result = host.event_loop();
    let browser = host.browser.take();
    let runtime = host.runtime.take();
    drop(host);
    // Input is released before browser/worker cleanup, even if the renderer died.
    let release = platform.set_capture(false);
    worker.stop();
    let until = Instant::now() + Duration::from_secs(5);
    while !worker.finished() {
        while worker.next_reply().is_some() {}
        if Instant::now() >= until {
            bail!("manager tasks did not stop within five seconds");
        }
        thread::sleep(Duration::from_millis(1));
    }
    worker.join()?;
    release?;
    platform.shutdown()?;
    if let Some(browser) = browser {
        browser.close()?;
    }
    drop(runtime);
    result.map(|()| 0)
}

#[derive(Clone, Copy)]
enum Closing {
    Reload,
    Exit,
    ImportTosu,
    Detach,
}
struct Host<'a> {
    browser: Option<OsrBrowser>,
    runtime: Option<CefRuntime>,
    sandbox_info: *mut u8,
    resources: Arc<ProductResources>,
    broker_url: String,
    platform: &'a mut PlatformController,
    worker: &'a mut EditorWorker,
    options: &'a Options,
    document: String,
    ready: bool,
    attached: bool,
    editing: bool,
    visible: bool,
    dialog: bool,
    persisted_fps_limit: u32,
    refresh_hz: f64,
    effective_fps: f64,
    next_ping: Instant,
    nonce: u64,
    pending_ping: Option<u64>,
    closing: Option<(Closing, String, Instant)>,
    consumed: [bool; 256],
    viewport: Viewport,
    shared_array_buffer: bool,
    pending_detach: bool,
    flushed: Option<Closing>,
}
impl Host<'_> {
    fn receive(&self, envelope: &Value) -> Result<()> {
        if let Some(browser) = self.browser.as_ref() {
            receive(browser, envelope)?;
        }
        Ok(())
    }

    fn open(&mut self) -> Result<()> {
        if self.browser.is_some() {
            return Ok(());
        }
        if self.runtime.is_none() {
            let parent = self
                .options
                .config
                .parent()
                .context("configuration has no parent directory")?;
            // The main-thread bootstrap context remains live for the entire host.
            self.runtime = Some(unsafe {
                CefRuntime::initialize(RuntimeOptions {
                    runtime: &self.options.runtime,
                    sandbox_info: self.sandbox_info,
                    cache: &parent.join("cef-cache"),
                    resources: self.resources.clone(),
                })
            }?);
        }
        let browser = self.runtime.as_ref().unwrap().open(
            BrowserOptions {
                url: MANAGER_URL,
                backend_url: self.options.backend.as_str(),
                broker_url: Some(&self.broker_url),
                width: self.viewport.width,
                height: self.viewport.height,
                scale: self.viewport.scale,
                fps_limit: self.effective_fps.ceil() as u32,
                shared_texture: cfg!(windows),
                shared_array_buffer: self.shared_array_buffer,
            },
            self.platform.frame_backend(),
        )?;
        browser.set_visible(self.visible && self.attached)?;
        self.browser = Some(browser);
        Ok(())
    }

    fn detach(&mut self) -> Result<()> {
        self.editing(false)?;
        if let Some(browser) = self.browser.take() {
            browser.close()?;
        }
        self.ready = false;
        self.document.clear();
        self.pending_ping = None;
        self.pending_detach = false;
        self.consumed.fill(false);
        Ok(())
    }
    fn apply_fps(&mut self) -> Result<()> {
        if let Some(browser) = self.browser.as_ref() {
            browser.set_frame_rate(self.effective_fps.ceil() as u32)?;
        }
        Ok(())
    }
    fn editing(&mut self, enabled: bool) -> Result<()> {
        let enabled = enabled && self.attached && self.visible && self.ready;
        if enabled && !self.dialog {
            if let Err(error) = self.platform.set_capture(true) {
                self.receive(&json!({"type":"error", "message":format!("{error:#}")}))?;
                return Ok(());
            }
        } else {
            #[cfg(windows)]
            if !enabled && self.editing {
                self.platform.end_edit_capture()?;
            } else {
                self.platform.set_capture(false)?;
            }
            #[cfg(target_os = "linux")]
            self.platform.set_capture(false)?;
        }
        self.editing = enabled;
        self.consumed.fill(false);
        if let Some(browser) = self.browser.as_ref() {
            browser.set_focus(enabled);
        }
        self.receive(&json!({"type":"editing", "editing":enabled}))?;
        #[cfg(windows)]
        self.platform.set_tray_state(self.editing, self.visible);
        Ok(())
    }
    fn flush(&mut self, action: Closing) -> Result<bool> {
        if self.closing.is_some() || self.flushed.is_some() || self.dialog {
            return Ok(false);
        }
        self.editing(false)?;
        if !self.ready {
            if self.worker.pending_tasks() {
                self.flushed = Some(action);
                return Ok(false);
            }
            return Ok(true);
        }
        self.nonce += 1;
        let id = format!("host-{}", self.nonce);
        self.receive(&json!({"type":"flush-save", "request_id":id}))?;
        self.closing = Some((action, id, Instant::now()));
        Ok(false)
    }
    fn import_tosu(&mut self) -> Result<()> {
        self.dialog = true;
        self.worker.submit(Work {
            document: self.document.clone(),
            message: json!({"type": "import-tosu-layout"}),
        })?;
        self.editing(true)
    }
    fn reload(&mut self) {
        self.ready = false;
        self.document.clear();
        self.pending_ping = None;
        if let Some(browser) = self.browser.as_ref() {
            browser.reload();
        }
    }
    fn input(&mut self, input: BrowserInput) -> Result<()> {
        if !self.editing || self.dialog || self.closing.is_some() || self.flushed.is_some() {
            return Ok(());
        }
        if let BrowserInput::Key {
            kind,
            modifiers,
            windows_key_code: code,
            character,
            ..
        } = input
        {
            if (0..256).contains(&code) && self.consumed[code as usize] {
                if kind == KeyKind::Up {
                    self.consumed[code as usize] = false;
                }
                return Ok(());
            }
            if kind == KeyKind::Character && self.consumed[0x49] && character == 9 {
                return Ok(());
            }
            if kind == KeyKind::RawDown
                && modifiers.control
                && !modifiers.shift
                && !modifiers.alt
                && code == 0x49
            {
                self.consumed[0x49] = true;
                self.receive(&json!({"type": "toggle-interaction"}))?;
                return Ok(());
            }
        }
        if let Some(browser) = self.browser.as_ref() {
            browser.send_input(input);
        }
        Ok(())
    }
    fn event_loop(&mut self) -> Result<()> {
        loop {
            if let Some(browser) = self.browser.as_ref() {
                browser.pump()?;
            } else if let Some(runtime) = self.runtime.as_ref() {
                runtime.pump();
            }
            while let Some(event) = self.platform.poll() {
                match event {
                    PlatformEvent::Attached { .. } => {
                        self.attached = true;
                        self.pending_detach = false;
                        if self
                            .closing
                            .as_ref()
                            .is_some_and(|(action, _, _)| matches!(action, Closing::Detach))
                        {
                            self.closing = None;
                        }
                        if matches!(self.flushed, Some(Closing::Detach)) {
                            self.flushed = None;
                        }
                        self.open()?;
                        self.browser.as_ref().unwrap().set_visible(self.visible)?;
                    }
                    PlatformEvent::Detached => {
                        self.attached = false;
                        self.editing(false)?;
                        self.pending_ping = None;
                        self.pending_detach = self.browser.is_some();
                        if let Some(browser) = self.browser.as_ref() {
                            browser.set_visible(false)?;
                        }
                    }
                    PlatformEvent::CaptureEnded => self.editing(false)?,
                    PlatformEvent::Viewport(viewport) => {
                        self.viewport = viewport;
                        self.refresh_hz = viewport.refresh_hz;
                        if let Some(browser) = self.browser.as_ref() {
                            browser.resize(viewport.width, viewport.height, viewport.scale)?;
                        }
                        self.effective_fps = super::frame_rate(
                            self.options.fps_limit,
                            self.persisted_fps_limit,
                            self.refresh_hz,
                        );
                        self.apply_fps()?;
                    }
                    PlatformEvent::Input(input) => self.input(input)?,
                    PlatformEvent::ToggleEdit | PlatformEvent::ShortcutToggleEdit => {
                        if !self.dialog && self.closing.is_none() && self.flushed.is_none() {
                            self.editing(!self.editing)?;
                        }
                    }
                    PlatformEvent::ToggleVisibility => {
                        self.editing(false)?;
                        self.visible = !self.visible;
                        if let Some(browser) = self.browser.as_ref() {
                            browser.set_visible(self.visible && self.attached)?;
                        }
                        #[cfg(windows)]
                        self.platform.set_tray_state(self.editing, self.visible);
                    }
                    PlatformEvent::Reload => {
                        if self.flush(Closing::Reload)? {
                            self.reload();
                        }
                    }
                    PlatformEvent::Exit => {
                        if self.flush(Closing::Exit)? {
                            return Ok(());
                        }
                    }
                    PlatformEvent::Warning(error) => eprintln!("Vosulay: {error}"),
                    PlatformEvent::Fatal(error) => bail!("{error}"),
                }
            }
            while let Some(raw) = self.browser.as_ref().and_then(OsrBrowser::next_message) {
                let message: Value =
                    serde_json::from_str(&raw).context("invalid manager bridge message")?;
                let kind = message
                    .get("type")
                    .and_then(Value::as_str)
                    .context("manager message has no type")?;
                let document = message
                    .get("__vosulay_document_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if kind == "ready" {
                    if document.is_empty() || document.len() > 128 {
                        bail!("invalid manager document identity");
                    }
                    self.document = document.to_owned();
                    self.ready = true;
                    self.apply_fps()?;
                    self.next_ping = Instant::now();
                    self.worker.submit(Work {
                        document: self.document.clone(),
                        message: message.clone(),
                    })?;
                    continue;
                }
                if !self.ready || document != self.document {
                    continue;
                }
                match kind {
                    "heartbeat" => {
                        if self.pending_ping.is_some()
                            && message.get("nonce").and_then(Value::as_u64) == self.pending_ping
                        {
                            self.pending_ping = None;
                            if self.attached {
                                self.platform.heartbeat();
                            }
                        }
                    }
                    "set-editing" => self.editing(
                        message
                            .get("editing")
                            .and_then(Value::as_bool)
                            .context("invalid edit state")?,
                    )?,
                    "editing-applied" => {}
                    "import-tosu-layout" => {
                        if self.flush(Closing::ImportTosu)? {
                            self.import_tosu()?;
                        }
                    }
                    "flush-complete" | "flush-failed" => {
                        let id = message
                            .get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if self
                            .closing
                            .as_ref()
                            .is_some_and(|(_, pending, _)| pending == id)
                        {
                            let (action, _, _) = self.closing.take().unwrap();
                            if kind == "flush-failed" {
                                if matches!(action, Closing::Detach) {
                                    self.pending_detach = false;
                                }
                                self.receive(
                                    &json!({"type":"error", "message":"Could not save edits; requested action cancelled"}),
                                )?;
                            } else {
                                self.flushed = Some(action);
                            }
                        }
                    }
                    _ => {
                        if matches!(
                            kind,
                            "choose-overlay-file" | "import-profile" | "export-profile"
                        ) {
                            if self.dialog || self.closing.is_some() || self.flushed.is_some() {
                                continue;
                            }
                            self.platform.set_capture(false)?;
                            self.dialog = true;
                        }
                        self.worker.submit(Work {
                            document: self.document.clone(),
                            message,
                        })?;
                    }
                }
            }
            while let Some(reply) = self.worker.next_reply() {
                if reply.document != self.document {
                    continue;
                }
                let settings = reply
                    .envelope
                    .get("config")
                    .and_then(|config| config.get("settings"));
                if let Some(enabled) = reply
                    .envelope
                    .get("allow_unrestricted_shared_array_buffer")
                    .or_else(|| {
                        settings.and_then(|settings| {
                            settings.get("allow_unrestricted_shared_array_buffer")
                        })
                    })
                    .and_then(Value::as_bool)
                {
                    self.shared_array_buffer = enabled;
                }
                if let Some(limit) = reply
                    .envelope
                    .get("fps_limit")
                    .or_else(|| settings.and_then(|settings| settings.get("fps_limit")))
                    .and_then(Value::as_u64)
                {
                    self.persisted_fps_limit =
                        limit.min(u64::from(crate::config::MAX_FPS_LIMIT)) as u32;
                    let fps = super::frame_rate(
                        self.options.fps_limit,
                        self.persisted_fps_limit,
                        self.refresh_hz,
                    );
                    if self.effective_fps != fps {
                        self.effective_fps = fps;
                        self.apply_fps()?;
                    }
                }
                if !reply.envelope.is_null() {
                    self.receive(&reply.envelope)?;
                }
                if reply.dialog_closed {
                    self.dialog = false;
                    if self.editing && self.attached && self.platform.game_focused() {
                        self.platform.set_capture(true)?;
                    }
                }
            }
            if !self.dialog && !self.worker.pending_tasks() {
                if let Some(action) = self.flushed.take() {
                    match action {
                        Closing::Reload => self.reload(),
                        Closing::Exit => return Ok(()),
                        Closing::ImportTosu => self.import_tosu()?,
                        Closing::Detach if !self.attached => self.detach()?,
                        Closing::Detach => {}
                    }
                }
                if self.pending_detach
                    && self.closing.is_none()
                    && self.flushed.is_none()
                    && !self.dialog
                    && self.flush(Closing::Detach)?
                {
                    self.detach()?;
                }
            }
            let now = Instant::now();
            if self.attached && self.ready && now >= self.next_ping {
                self.nonce += 1;
                self.pending_ping = Some(self.nonce);
                if let Some(browser) = self.browser.as_ref() {
                    browser.evaluate(&format!(
                        "window.__VOSULAY_POST_MESSAGE__({{type:'heartbeat',nonce:{}}});",
                        self.nonce
                    ));
                }
                self.next_ping = now + Duration::from_secs(1);
            }
            if self
                .closing
                .as_ref()
                .is_some_and(|(_, _, started)| started.elapsed() >= Duration::from_secs(5))
            {
                let (action, _, _) = self.closing.take().unwrap();
                if matches!(action, Closing::Detach) {
                    self.pending_detach = false;
                }
                self.receive(
                    &json!({"type":"error", "message":"Could not save edits within five seconds; requested action cancelled"}),
                )?;
            }
            // Native publishers and CEF scheduling callbacks wake this thread.
            let mut next = self
                .runtime
                .as_ref()
                .and_then(CefRuntime::next_pump_deadline);
            if self.attached && self.ready {
                next = Some(next.map_or(self.next_ping, |deadline| deadline.min(self.next_ping)));
            }
            if let Some((_, _, started)) = &self.closing {
                let closing = *started + Duration::from_secs(5);
                next = Some(next.map_or(closing, |deadline| deadline.min(closing)));
            }
            if let Some(next) = next {
                thread::park_timeout(next.saturating_duration_since(Instant::now()));
            } else {
                thread::park();
            }
        }
    }
}
