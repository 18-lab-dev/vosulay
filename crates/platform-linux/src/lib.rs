#![cfg(target_os = "linux")]

pub mod injector;

use anyhow::{Context, Result, anyhow, ensure};
use std::{
    collections::VecDeque,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use vosulay_frame::{BrowserFrameBackend, BrowserInput, KeyKind, PlatformEvent, Viewport};
use vosulay_transport::{
    Connection, FrameBackend, FrameMailbox, Listener, default_endpoint,
    protocol::{FORMAT_BGRA, Kind, Packet, pixel_bytes},
};

const DEADLINE: Duration = Duration::from_secs(5);
const MAX_PEERS: usize = 16;
const MAX_EVENTS: usize = 512;

#[derive(Clone, Debug)]
pub struct LinuxOptions {
    pub library: PathBuf,
    pub pids: Vec<u32>,
    pub process_names: Vec<String>,
}
struct Shared {
    frames: Arc<FrameMailbox>,
    events: Mutex<VecDeque<PlatformEvent>>,
    viewport: Mutex<Option<Viewport>>,
    fatal: Mutex<Option<String>>,
    focused: AtomicBool,
    capture: AtomicBool,
    stop: AtomicBool,
    epoch: Instant,
    wake: Arc<OwnedFd>,
    worker_done: AtomicBool,
    heartbeat: AtomicU64,
    host: Thread,
}
impl Shared {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }
    fn event(&self, event: PlatformEvent) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if events.len() == MAX_EVENTS {
            drop(events);
            self.fail("game input/control queue overflowed; releasing capture".to_owned());
            return;
        }
        if matches!(
            event,
            PlatformEvent::Input(BrowserInput::Move { leave: false, .. })
        ) && matches!(
            events.back(),
            Some(PlatformEvent::Input(BrowserInput::Move {
                leave: false,
                ..
            }))
        ) {
            *events.back_mut().expect("checked last event") = event;
        } else {
            events.push_back(event);
        }
        self.host.unpark();
    }
    fn fail(&self, error: String) {
        self.capture.store(false, Ordering::Release);
        self.frames.wake_control();
        let mut fatal = self.fatal.lock().unwrap_or_else(|error| error.into_inner());
        if fatal.is_none() {
            *fatal = Some(error);
        }
        self.host.unpark();
    }
}

pub struct LinuxController {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl LinuxController {
    pub fn start(options: LinuxOptions) -> Result<Self> {
        ensure!(
            options.library.is_file(),
            "missing packaged Linux compositor {}",
            options.library.display()
        );
        let endpoint = default_endpoint();
        let listener = Listener::bind(&endpoint)
            .context("starting Linux overlay endpoint (another vosulay may be running)")?;
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        ensure!(
            fd >= 0,
            "creating control wake descriptor: {}",
            std::io::Error::last_os_error()
        );
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let shared = Arc::new(Shared {
            frames: Arc::new(FrameMailbox::with_wake(wake.clone())),
            events: Mutex::new(VecDeque::with_capacity(MAX_EVENTS)),
            viewport: Mutex::new(None),
            fatal: Mutex::new(None),
            focused: AtomicBool::new(false),
            capture: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            wake,
            worker_done: AtomicBool::new(false),
            epoch: Instant::now(),
            heartbeat: AtomicU64::new(1),
            host: thread::current(),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("vosulay-linux-overlay".into())
            .spawn(move || {
                if let Err(error) = worker_main(options, endpoint, listener, &worker_shared) {
                    worker_shared.fail(format!("{error:#}"));
                }
                worker_shared.worker_done.store(true, Ordering::Release);
                worker_shared.host.unpark();
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub fn frame_backend(&self) -> Box<dyn BrowserFrameBackend> {
        Box::new(FrameBackend::new(self.shared.frames.clone()))
    }
    pub fn poll(&mut self) -> Option<PlatformEvent> {
        if let Some(error) = self
            .shared
            .fatal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            return Some(PlatformEvent::Fatal(error));
        }
        if let Some(event) = self
            .shared
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
        {
            return Some(event);
        }
        if let Some(viewport) = self
            .shared
            .viewport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            return Some(PlatformEvent::Viewport(viewport));
        }
        None
    }
    pub fn game_focused(&self) -> bool {
        self.shared.focused.load(Ordering::Acquire)
    }
    pub fn set_capture(&self, enabled: bool) -> Result<()> {
        if enabled {
            ensure!(
                self.game_focused(),
                "focus the attached game before editing"
            );
            ensure!(
                self.shared
                    .now()
                    .saturating_sub(self.shared.heartbeat.load(Ordering::Acquire))
                    < 2500,
                "manager renderer is not responsive"
            );
        }
        self.shared.capture.store(enabled, Ordering::Release);
        self.shared.frames.wake_control();
        Ok(())
    }
    /// Update only after a nonce-matched heartbeat from the trusted manager page.
    pub fn heartbeat(&self) {
        self.shared
            .heartbeat
            .store(self.shared.now(), Ordering::Release);
        self.shared.frames.wake_control();
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.shared.capture.store(false, Ordering::Release);
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            self.shared.frames.wake_control();
            let deadline = Instant::now() + DEADLINE;
            while !self.shared.worker_done.load(Ordering::Acquire) {
                ensure!(
                    Instant::now() < deadline,
                    "Linux overlay shutdown exceeded five seconds"
                );
                thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
            }
            worker
                .join()
                .map_err(|_| anyhow!("Linux overlay worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for LinuxController {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Peer {
    id: u64,
    connection: Connection,
    pid: u32,
    session: u64,
    surface: u64,
    viewport: Option<Viewport>,
    focus: FocusState,
    alive: bool,
    hello: bool,
    blocked: bool,
    sent_generation: u64,
    sent_visible: Option<bool>,
    sent_capture: Option<bool>,
    next_ping: Instant,
    toggle_down: bool,
}
impl Peer {
    fn send(&mut self, mut packet: Packet, descriptors: &[std::os::fd::BorrowedFd<'_>]) -> bool {
        packet.session = self.session;
        packet.surface = self.surface;
        match self.connection.send(&packet, descriptors) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                self.blocked = true;
                false
            }
            Err(_) => {
                self.alive = false;
                false
            }
        }
    }
    fn boolean(&mut self, kind: Kind, value: bool) -> bool {
        let mut packet = Packet::new(kind);
        packet.code = u32::from(value);
        self.send(packet, &[])
    }
}

#[derive(Default)]
struct FocusState {
    focused: bool,
    timestamp: u64,
    gained: u64,
}
impl FocusState {
    fn observe(&mut self, focused: bool, timestamp: u64, gained: u64) {
        if timestamp > self.timestamp {
            self.focused = focused;
            self.timestamp = timestamp;
            self.gained = gained;
        }
    }
}

#[derive(Default)]
struct FocusSelection {
    active: Option<u64>,
    newest: Option<(u64, u64)>,
}
impl FocusSelection {
    fn observe(&mut self, id: u64, gained: u64) {
        let order = (gained, id);
        if gained != 0 && self.newest.is_none_or(|newest| order > newest) {
            self.newest = Some(order);
            self.active = Some(id);
        }
    }
    fn remove(&mut self, id: u64) {
        if self.active == Some(id) {
            self.active = None;
        }
        // Keep the watermark: an old unfocused peer must not replace a dead game.
    }
}

fn worker_main(
    options: LinuxOptions,
    endpoint: String,
    listener: Listener,
    shared: &Shared,
) -> Result<()> {
    let mut peers: Vec<Peer> = Vec::with_capacity(MAX_PEERS);
    let mut next_id = 1_u64;
    let session = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64
        ^ (u64::from(std::process::id()) << 32);
    let session = session.max(1);
    let mut active = None;
    let mut selection = FocusSelection::default();
    let mut next_discovery = Instant::now();
    let mut attempted = Vec::<(u32, u64)>::new();
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        for _ in 0..MAX_PEERS {
            let Some(connection) = listener.accept()? else {
                break;
            };
            if peers.len() == MAX_PEERS {
                drop(connection);
                continue;
            }
            let pid = connection.peer_pid()?;
            if !supported(pid, &options) {
                continue;
            }
            peers.push(Peer {
                id: next_id,
                connection,
                pid,
                session,
                surface: 0,
                viewport: None,
                focus: FocusState::default(),
                alive: true,
                hello: false,
                blocked: false,
                sent_generation: 0,
                sent_visible: None,
                sent_capture: None,
                next_ping: now,
                toggle_down: false,
            });
            next_id = next_id
                .checked_add(1)
                .context("Linux peer identity exhausted")?;
        }
        for peer in &mut peers {
            for _ in 0..64 {
                let message = match peer.connection.recv() {
                    Ok(Some(message)) => message.packet,
                    Ok(None) => break,
                    Err(_) => {
                        peer.alive = false;
                        break;
                    }
                };
                if message.kind == Kind::Hello {
                    if message.value[0] != u64::from(peer.pid)
                        || message.surface == 0
                        || peer.surface != 0
                    {
                        peer.alive = false;
                        break;
                    }
                    peer.surface = message.surface;
                    continue;
                }
                if !peer.hello || message.session != session || message.surface != peer.surface {
                    continue;
                }
                match message.kind {
                    Kind::Surface => {
                        if pixel_bytes(message.width, message.height).is_err() {
                            peer.alive = false;
                            break;
                        }
                        let refresh_hz = message.value[1] as f64 / 1000.0;
                        let viewport = Viewport {
                            width: message.width,
                            height: message.height,
                            scale: 1.0,
                            refresh_hz: if refresh_hz.is_finite() && refresh_hz > 0.0 {
                                refresh_hz
                            } else {
                                60.0
                            },
                        };
                        if peer.viewport != Some(viewport) && active == Some(peer.id) {
                            *shared
                                .viewport
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(viewport);
                            shared.host.unpark();
                        }
                        peer.viewport = Some(viewport);
                        peer.focus.observe(
                            message.value[2] == 1,
                            message.sequence,
                            message.value[3],
                        );
                    }
                    Kind::Focus => {
                        peer.focus
                            .observe(message.code == 1, message.sequence, message.value[3]);
                    }
                    Kind::Input if active == Some(peer.id) => {
                        let input = message.browser_input()?;
                        if let BrowserInput::Key {
                            kind,
                            modifiers,
                            windows_key_code,
                            ..
                        } = input
                            && windows_key_code == 0x20
                        {
                            if kind == KeyKind::Up {
                                peer.toggle_down = false;
                            }
                            if kind == KeyKind::RawDown
                                && modifiers.control
                                && modifiers.shift
                                && !modifiers.alt
                                && peer.focus.focused
                            {
                                if !peer.toggle_down {
                                    shared.event(PlatformEvent::ShortcutToggleEdit);
                                    peer.toggle_down = true;
                                }
                                continue;
                            }
                        }
                        shared.event(PlatformEvent::Input(input));
                    }
                    Kind::SurfaceGone | Kind::Detach => {
                        peer.alive = false;
                        break;
                    }
                    Kind::Input => {}
                    _ => {
                        peer.alive = false;
                        break;
                    }
                }
            }
        }
        for peer in &peers {
            if !peer.alive {
                selection.remove(peer.id);
            }
        }
        for peer in &peers {
            if peer.alive && peer.viewport.is_some() {
                selection.observe(peer.id, peer.focus.gained);
            }
        }
        if active != selection.active {
            shared.capture.store(false, Ordering::Release);
            active = selection.active;
            if let Some(peer) = active.and_then(|id| peers.iter().find(|peer| peer.id == id)) {
                shared.event(PlatformEvent::Attached { pid: peer.pid });
                *shared
                    .viewport
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = peer.viewport;
                shared.host.unpark();
            } else {
                shared.focused.store(false, Ordering::Release);
                shared.event(PlatformEvent::Detached);
            }
        }
        let focused = active
            .and_then(|id| peers.iter().find(|peer| peer.id == id))
            .is_some_and(|peer| peer.focus.focused);
        if shared.focused.swap(focused, Ordering::AcqRel) != focused {
            shared.host.unpark();
        }
        if shared.capture.load(Ordering::Acquire)
            && (!focused
                || shared
                    .now()
                    .saturating_sub(shared.heartbeat.load(Ordering::Acquire))
                    >= DEADLINE.as_millis() as u64)
        {
            shared.capture.store(false, Ordering::Release);
            shared.event(PlatformEvent::CaptureEnded);
        }
        let capture = shared.capture.load(Ordering::Acquire);
        let ring = shared.frames.snapshot()?;
        for peer in &mut peers {
            peer.blocked = false;
            if !peer.alive {
                continue;
            }
            if !peer.hello {
                if peer.surface == 0 {
                    continue;
                }
                let mut hello = Packet::new(Kind::Hello);
                hello.value[0] = u64::from(std::process::id());
                if !peer.send(hello, &[]) {
                    continue;
                }
                peer.hello = true;
            }
            let visible = active == Some(peer.id) && shared.frames.visible();
            if peer.sent_visible != Some(visible) {
                if !peer.boolean(Kind::Visible, visible) {
                    continue;
                }
                peer.sent_visible = Some(visible);
            }
            let capturing = visible && focused && capture;
            if peer.sent_capture != Some(capturing) {
                if !peer.boolean(Kind::Capture, capturing) {
                    continue;
                }
                peer.sent_capture = Some(capturing);
            }
            if visible
                && let Some((generation, handle)) = &ring
                && peer.sent_generation != *generation
            {
                let mut packet = Packet::new(Kind::Ring);
                packet.generation = *generation;
                packet.width = handle.width();
                packet.height = handle.height();
                packet.stride = handle.width() * 4;
                packet.format = FORMAT_BGRA;
                packet.value[0] = handle.mapping_len() as u64;
                if !peer.send(packet, &[handle.descriptor()]) {
                    continue;
                }
                peer.sent_generation = *generation;
            }
            if now >= peer.next_ping && peer.send(Packet::new(Kind::Ping), &[]) {
                peer.next_ping = now + Duration::from_millis(100);
            }
        }
        for peer in &peers {
            if !peer.alive {
                selection.remove(peer.id);
            }
        }
        if active != selection.active {
            active = selection.active;
            shared.capture.store(false, Ordering::Release);
            shared.focused.store(false, Ordering::Release);
            shared.event(PlatformEvent::Detached);
        }
        peers.retain(|peer| peer.alive);
        if now >= next_discovery {
            discover_and_attach(&options, &endpoint, &peers, &mut attempted, shared);
            next_discovery = now + Duration::from_secs(1);
        }
        let mut deadline = next_discovery;
        for peer in &peers {
            if peer.hello && !peer.blocked {
                deadline = deadline.min(peer.next_ping);
            }
        }
        if capture {
            let remaining = (DEADLINE.as_millis() as u64).saturating_sub(
                shared
                    .now()
                    .saturating_sub(shared.heartbeat.load(Ordering::Acquire)),
            );
            deadline = deadline.min(Instant::now() + Duration::from_millis(remaining));
        }
        wait_control(&listener, &peers, &shared.wake, deadline)?;
    }
    for peer in &mut peers {
        let _ = peer.boolean(Kind::Capture, false);
        let _ = peer.send(Packet::new(Kind::Detach), &[]);
    }
    // Socket close immediately fails open at the next input poll/present; the
    // injected component also has its own five-second silence expiration.
    drop(peers);
    Ok(())
}

fn wait_control(
    listener: &Listener,
    peers: &[Peer],
    wake: &OwnedFd,
    deadline: Instant,
) -> Result<()> {
    let mut descriptors = [libc::pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; MAX_PEERS + 2];
    descriptors[0] = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    descriptors[1] = libc::pollfd {
        fd: wake.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    for (descriptor, peer) in descriptors[2..].iter_mut().zip(peers) {
        *descriptor = libc::pollfd {
            fd: peer.connection.as_raw_fd(),
            events: libc::POLLIN | if peer.blocked { libc::POLLOUT } else { 0 },
            revents: 0,
        };
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = remaining
        .as_millis()
        .saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ))
        .min(i32::MAX as u128) as i32;
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            (peers.len() + 2) as libc::nfds_t,
            timeout,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    if descriptors[1].revents & libc::POLLIN != 0 {
        let mut value = 0_u64;
        loop {
            let read = unsafe { libc::read(wake.as_raw_fd(), (&mut value as *mut u64).cast(), 8) };
            if read >= 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }
    Ok(())
}

fn supported(pid: u32, options: &LinuxOptions) -> bool {
    if options.pids.contains(&pid) {
        return true;
    }
    let path = PathBuf::from(format!("/proc/{pid}"));
    let Ok(executable) = std::fs::read_link(path.join("exe")) else {
        return false;
    };
    let Some(name) = executable.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let command_line = if wine_loader(name) {
        std::fs::read(path.join("cmdline")).unwrap_or_default()
    } else {
        Vec::new()
    };
    supported_image(name, &command_line, &options.process_names)
}

fn wine_loader(name: &str) -> bool {
    ["wine", "wine64", "wine-preloader", "wine64-preloader"]
        .iter()
        .any(|loader| name.eq_ignore_ascii_case(loader))
}

fn supported_image(executable: &str, command_line: &[u8], process_names: &[String]) -> bool {
    let matches = |name: &str| {
        ["osu!", "osu", "osulazer", "osu!.exe", "osulazer.exe"]
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
            || process_names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
    };
    if matches(executable) {
        return true;
    }
    // A launcher/script mentioning a game is not the game. Wine may expose
    // its loader as /proc/exe; only its program argv can select a game.
    wine_loader(executable)
        && command_line
            .split(|byte| *byte == 0)
            .take(2)
            .filter_map(|part| std::str::from_utf8(part).ok())
            .any(|part| matches(part.rsplit(['/', '\\']).next().unwrap_or(part)))
}

fn discover_and_attach(
    options: &LinuxOptions,
    endpoint: &str,
    peers: &[Peer],
    attempted: &mut Vec<(u32, u64)>,
    shared: &Shared,
) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id()
            || !supported(pid, options)
            || peers.iter().any(|peer| peer.pid == pid)
        {
            continue;
        }
        let Ok(identity) = injector::process_identity(pid) else {
            continue;
        };
        let key = (pid, identity.start_time);
        if attempted.contains(&key) {
            continue;
        }
        attempted.push(key);
        if let Err(error) = injector::attach(pid, &options.library, endpoint) {
            shared.event(PlatformEvent::Warning(format!("could not attach game {pid}: {error}; use LD_PRELOAD for restricted processes or the Vulkan layer at game startup")));
        }
    }
    attempted.retain(|(pid, start)| {
        injector::process_identity(*pid).is_ok_and(|identity| identity.start_time == *start)
    });
}

#[cfg(test)]
mod tests {
    use super::{FocusSelection, FocusState, supported_image};

    #[test]
    fn latest_focus_gain_wins_batched_a_b_a_in_either_peer_order() {
        let mut a = FocusState::default();
        a.observe(true, 100, 100);
        a.observe(false, 200, 100);
        a.observe(true, 300, 300);
        let mut b = FocusState::default();
        b.observe(true, 200, 200);
        b.observe(false, 300, 200);
        for order in [[(1, &a), (2, &b)], [(2, &b), (1, &a)]] {
            let mut selection = FocusSelection::default();
            for (id, state) in order {
                selection.observe(id, state.gained);
            }
            assert_eq!(selection.active, Some(1));
        }
    }

    #[test]
    fn unrelated_focus_loss_and_resize_retain_selected_game() {
        let mut selection = FocusSelection::default();
        selection.observe(1, 100);
        selection.observe(2, 200);
        let mut state = FocusState::default();
        state.observe(true, 200, 200);
        state.observe(false, 300, 200);
        selection.observe(2, state.gained);
        selection.observe(1, 100);
        selection.observe(3, 0);
        assert_eq!(selection.active, Some(2));
        assert!(!state.focused);
    }

    #[test]
    fn selected_death_clears_without_falling_back_to_old_focus() {
        let mut selection = FocusSelection::default();
        selection.observe(1, 100);
        selection.observe(2, 200);
        selection.remove(1);
        assert_eq!(selection.active, Some(2));
        selection.remove(2);
        selection.observe(1, 100);
        selection.observe(3, 0);
        assert_eq!(selection.active, None);
        selection.observe(3, 300);
        assert_eq!(selection.active, Some(3));
    }

    #[test]
    fn stale_surface_cannot_reverse_focus_and_coalesced_loss_retains_gain() {
        let mut state = FocusState::default();
        state.observe(false, 300, 200);
        state.observe(true, 200, 200);
        assert!(!state.focused);
        assert_eq!(state.gained, 200);
        let mut selection = FocusSelection::default();
        selection.observe(1, 100);
        selection.observe(2, state.gained);
        assert_eq!(selection.active, Some(2));
    }

    #[test]
    fn equal_timestamps_have_a_stable_peer_identity_tiebreak() {
        for order in [[1, 2], [2, 1]] {
            let mut selection = FocusSelection::default();
            for id in order {
                selection.observe(id, 100);
            }
            assert_eq!(selection.active, Some(2));
        }
    }

    #[test]
    fn discovers_native_games_and_explicit_process_names() {
        assert!(supported_image("osu!", b"", &[]));
        assert!(supported_image("OSULAZER", b"", &[]));
        assert!(supported_image(
            "custom-game",
            b"",
            &["custom-game".to_owned()]
        ));
    }

    #[test]
    fn wine_program_arguments_select_games_but_not_unrelated_targets() {
        assert!(supported_image(
            "wine64-preloader",
            b"C:\\Games\\osu!\\osu!.exe\0",
            &[]
        ));
        assert!(supported_image(
            "wine",
            b"wine\0/home/user/osulazer.exe\0",
            &[]
        ));
        assert!(!supported_image(
            "wine",
            b"wine\0notepad.exe\0osu!.exe\0",
            &[]
        ));
        assert!(!supported_image("wine64", b"wine64\0\xffosu!.exe\0", &[]));
    }

    #[test]
    fn never_injects_launchers_that_merely_mention_a_game() {
        assert!(!supported_image(
            "python3",
            b"python3\0proof.py\0/home/user/osu!\0",
            &[]
        ));
        assert!(!supported_image("bwrap", b"bwrap\0/home/user/osu!\0", &[]));
        assert!(!supported_image("sh", b"sh\0osu!.exe\0", &[]));
    }
}
