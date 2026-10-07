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
use vosulay_browser::{BrowserOptions, OsrBrowser, execute_subprocess, scheme::MANAGER_URL};
use vosulay_frame::{BrowserInput, KeyKind, PlatformEvent};
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
        library: options.runtime.join("libvosulay_overlay.so"),
        pids: options.pids.clone(),
        process_names: options.process_names.clone(),
    })?;
    let mut worker = EditorWorker::start(
        config.clone(),
        options.config.clone(),
        options.backend.clone(),
        resources.clone(),
    )?;
    let parent = options
        .config
        .parent()
        .context("configuration has no parent directory")?;
    let initial_fps = super::frame_rate(options.fps_limit, config.settings.fps_limit, 60.0);
    let browser = unsafe {
        OsrBrowser::open(
            BrowserOptions {
                runtime: &options.runtime,
                sandbox_info,
                cache: &parent.join("cef-cache"),
                url: MANAGER_URL,
                backend_url: options.backend.as_str(),
                broker_url: Some(remote.browser_url().as_str()),
                resources,
                width: 640,
                height: 480,
                scale: 1.0,
                fps_limit: initial_fps.ceil() as u32,
                shared_texture: cfg!(windows),
                shared_array_buffer: config.settings.allow_unrestricted_shared_array_buffer,
            },
            platform.frame_backend(),
        )
    }?;
    browser.set_visible(false)?;
    let mut host = Host {
        browser: &browser,
        platform: &mut platform,
        worker: &mut worker,
        options: &options,
        document: String::new(),
        ready: false,
        attached: false,
        editing: false,
        visible: true,
        dialog: false,
        #[cfg(windows)]
        frame_interval: Duration::from_secs_f64(1.0 / initial_fps),
        #[cfg(windows)]
        next_frame: Instant::now(),
        next_ping: Instant::now(),
        nonce: 0,
        pending_ping: None,
        closing: None,
        persisted_fps_limit: config.settings.fps_limit,
        refresh_hz: 60.0,
        effective_fps: initial_fps,
        consumed: [false; 256],
    };
    let result = host.event_loop();
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
    browser.close()?;
    result.map(|()| 0)
}

#[derive(Clone, Copy)]
enum Closing {
    Reload,
    Exit,
    ImportTosu,
}
struct Host<'a> {
    browser: &'a OsrBrowser,
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
    #[cfg(windows)]
    frame_interval: Duration,
    #[cfg(windows)]
    next_frame: Instant,
    next_ping: Instant,
    nonce: u64,
    pending_ping: Option<u64>,
    closing: Option<(Closing, String, Instant)>,
    consumed: [bool; 256],
}
impl Host<'_> {
    fn apply_fps(&mut self) -> Result<()> {
        self.browser
            .set_frame_rate(self.effective_fps.ceil() as u32)?;
        #[cfg(windows)]
        {
            self.frame_interval = Duration::from_secs_f64(1.0 / self.effective_fps);
            self.next_frame = Instant::now();
        }
        Ok(())
    }
    fn editing(&mut self, enabled: bool) -> Result<()> {
        let enabled = enabled && self.attached && self.visible && self.ready;
        if enabled && !self.dialog {
            if let Err(error) = self.platform.set_capture(true) {
                receive(
                    self.browser,
                    &json!({"type":"error", "message":format!("{error:#}")}),
                )?;
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
        self.browser.set_focus(enabled);
        receive(self.browser, &json!({"type":"editing", "editing":enabled}))?;
        #[cfg(windows)]
        self.platform.set_tray_state(self.editing, self.visible);
        Ok(())
    }
    fn flush(&mut self, action: Closing) -> Result<bool> {
        if self.closing.is_some() || self.dialog {
            return Ok(false);
        }
        self.editing(false)?;
        if !self.ready {
            return Ok(true);
        }
        self.nonce += 1;
        let id = format!("host-{}", self.nonce);
        receive(self.browser, &json!({"type":"flush-save", "request_id":id}))?;
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
        self.browser.reload();
    }
    fn input(&mut self, input: BrowserInput) -> Result<()> {
        if !self.editing || self.dialog || self.closing.is_some() {
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
            if kind == KeyKind::Character
                && ((self.consumed[0x49] && character == 9)
                    || (self.consumed[0x52] && character == 18))
            {
                return Ok(());
            }
            if kind == KeyKind::RawDown
                && modifiers.control
                && !modifiers.shift
                && !modifiers.alt
                && matches!(code, 0x49 | 0x52)
            {
                self.consumed[code as usize] = true;
                receive(
                    self.browser,
                    &json!({"type":if code == 0x49 { "toggle-interaction" } else { "refresh-selected-overlay" }}),
                )?;
                return Ok(());
            }
        }
        self.browser.send_input(input);
        Ok(())
    }
    fn event_loop(&mut self) -> Result<()> {
        loop {
            self.browser.pump()?;
            while let Some(event) = self.platform.poll() {
                match event {
                    PlatformEvent::Attached { .. } => {
                        self.attached = true;
                        self.browser.set_visible(self.visible)?;
                        #[cfg(windows)]
                        {
                            self.next_frame = Instant::now();
                        }
                    }
                    PlatformEvent::Detached => {
                        self.attached = false;
                        self.editing(false)?;
                        self.browser.set_visible(false)?;
                    }
                    PlatformEvent::CaptureEnded => self.editing(false)?,
                    PlatformEvent::Viewport(viewport) => {
                        self.browser
                            .resize(viewport.width, viewport.height, viewport.scale)?;
                        self.refresh_hz = viewport.refresh_hz;
                        self.effective_fps = super::frame_rate(
                            self.options.fps_limit,
                            self.persisted_fps_limit,
                            self.refresh_hz,
                        );
                        self.apply_fps()?;
                    }
                    PlatformEvent::Input(input) => self.input(input)?,
                    PlatformEvent::ToggleEdit | PlatformEvent::ShortcutToggleEdit => {
                        if !self.dialog && self.closing.is_none() {
                            self.editing(!self.editing)?;
                        }
                    }
                    PlatformEvent::ToggleVisibility => {
                        self.editing(false)?;
                        self.visible = !self.visible;
                        self.browser.set_visible(self.visible && self.attached)?;
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
            while let Some(raw) = self.browser.next_message() {
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
                            self.platform.heartbeat();
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
                                receive(
                                    self.browser,
                                    &json!({"type":"error", "message":"Could not save edits; requested action cancelled"}),
                                )?;
                            } else {
                                match action {
                                    Closing::Reload => self.reload(),
                                    Closing::Exit => return Ok(()),
                                    Closing::ImportTosu => self.import_tosu()?,
                                }
                            }
                        }
                    }
                    _ => {
                        if matches!(
                            kind,
                            "choose-overlay-file" | "import-profile" | "export-profile"
                        ) {
                            if self.dialog || self.closing.is_some() {
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
                if let Some(limit) = reply.envelope.get("fps_limit").and_then(Value::as_u64) {
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
                    receive(self.browser, &reply.envelope)?;
                }
                if reply.dialog_closed {
                    self.dialog = false;
                    if self.editing && self.platform.game_focused() {
                        self.platform.set_capture(true)?;
                    }
                }
            }
            let now = Instant::now();
            if self.ready && now >= self.next_ping {
                self.nonce += 1;
                self.pending_ping = Some(self.nonce);
                self.browser.evaluate(&format!(
                    "window.__VOSULAY_POST_MESSAGE__({{type:'heartbeat',nonce:{}}});",
                    self.nonce
                ));
                self.next_ping = now + Duration::from_secs(1);
            }
            if self
                .closing
                .as_ref()
                .is_some_and(|(_, _, started)| started.elapsed() >= Duration::from_secs(5))
            {
                self.closing = None;
                receive(
                    self.browser,
                    &json!({"type":"error", "message":"Could not save edits within five seconds; reload/exit cancelled"}),
                )?;
            }
            #[cfg(windows)]
            if self.attached && self.visible && now >= self.next_frame {
                self.browser.begin_frame();
                // One due frame, never a catch-up burst or an unbounded queue.
                self.next_frame = next_frame_deadline(self.next_frame, now, self.frame_interval);
            }
            // Native publishers and CEF scheduling callbacks wake this thread.
            let mut next = now + Duration::from_millis(100);
            if let Some(deadline) = self.browser.next_pump_deadline() {
                next = next.min(deadline);
            }
            if self.ready {
                next = next.min(self.next_ping);
            }
            #[cfg(windows)]
            if self.attached && self.visible {
                next = next.min(self.next_frame);
            }
            if let Some((_, _, started)) = &self.closing {
                next = next.min(*started + Duration::from_secs(5));
            }
            thread::park_timeout(next.saturating_duration_since(Instant::now()));
        }
    }
}

#[cfg(any(windows, test))]
// Keep the frame clock's phase across polling jitter, skipping every missed
// deadline in one step instead of drifting or generating catch-up bursts.
fn next_frame_deadline(previous: Instant, now: Instant, interval: Duration) -> Instant {
    let remainder = now.duration_since(previous).as_nanos() % interval.as_nanos();
    now + interval - Duration::from_nanos(remainder as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixty_fps_requests_every_fourth_240_hz_present_despite_poll_jitter() {
        let start = Instant::now();
        let interval = Duration::from_secs_f64(1.0 / 60.0);
        let mut deadline = start;
        let mut requested = Vec::new();
        for index in 0..240 {
            let now = start
                + Duration::from_secs_f64(f64::from(index) / 240.0)
                + Duration::from_micros(100);
            if now >= deadline {
                requested.push(index);
                deadline = next_frame_deadline(deadline, now, interval);
            }
        }
        assert_eq!(requested, (0..240).step_by(4).collect::<Vec<_>>());
    }

    #[test]
    fn stalled_clock_skips_missed_frames_without_catchup_or_phase_drift() {
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let delayed = start + Duration::from_millis(503);
        let next = next_frame_deadline(start, delayed, interval);
        assert_eq!(next, start + Duration::from_millis(512));
        assert!(next > delayed);
        assert_eq!(
            next_frame_deadline(next, next, interval),
            start + Duration::from_millis(528)
        );
    }
}
