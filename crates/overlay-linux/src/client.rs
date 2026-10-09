//! Game-side control and persistent CPU frame consumption; no browser knowledge.
use std::{
    io,
    sync::{LazyLock, OnceLock},
    time::{Duration, Instant},
};
use vosulay_frame::{BrowserInput, KeyKind, hotkey::NativeHotkey};
use vosulay_transport::{
    Connection, Consumer, ReadFrame, default_endpoint,
    protocol::{FORMAT_BGRA, Kind, Packet, focus_timestamp, pixel_bytes},
};

static ENDPOINT: OnceLock<String> = OnceLock::new();
static DEFAULT_ENDPOINT: LazyLock<String> =
    LazyLock::new(|| std::env::var("VOSULAY_ENDPOINT").unwrap_or_else(|_| default_endpoint()));
pub(crate) fn set_endpoint(endpoint: &str) -> io::Result<()> {
    if endpoint.len() > 107 || endpoint.is_empty() || endpoint.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid overlay endpoint",
        ));
    }
    if let Some(existing) = ENDPOINT.get() {
        return if existing == endpoint {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "overlay endpoint already configured",
            ))
        };
    }
    ENDPOINT.set(endpoint.to_owned()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "overlay endpoint already configured",
        )
    })
}
fn endpoint() -> &'static str {
    ENDPOINT
        .get()
        .map(String::as_str)
        .unwrap_or_else(|| DEFAULT_ENDPOINT.as_str())
}

pub(crate) struct Client {
    connection: Option<Connection>,
    consumer: Option<Consumer>,
    pub(crate) surface: u64,
    extent: Option<(u32, u32)>,
    generation: u64,
    session: u64,
    visible: bool,
    capture: bool,
    focused: bool,
    focus_time: u64,
    focus_gain: u64,
    sent_focus: Option<u64>,
    last_receive: Instant,
    last_attempt: Option<Instant>,
    last_surface: Option<(u32, u32, u32, bool, u64)>,
    hotkeys: [Option<NativeHotkey>; 2],
    shortcut_keys: [Option<i32>; 2],
}

impl Client {
    pub(crate) fn new(surface: u64) -> Self {
        Self {
            connection: None,
            consumer: None,
            surface,
            extent: None,
            generation: 0,
            session: 0,
            visible: false,
            capture: false,
            focused: false,
            focus_time: focus_timestamp(),
            focus_gain: 0,
            sent_focus: None,
            last_receive: Instant::now(),
            last_attempt: None,
            last_surface: None,
            hotkeys: [None; 2],
            shortcut_keys: [None; 2],
        }
    }
    fn connect(&mut self, now: Instant) {
        if self.connection.is_some()
            || self
                .last_attempt
                .is_some_and(|attempt| now.duration_since(attempt) < Duration::from_millis(500))
        {
            return;
        }
        self.last_attempt = Some(now);
        let Ok(connection) = Connection::connect(endpoint()) else {
            return;
        };
        let mut hello = Packet::new(Kind::Hello);
        hello.value[0] = u64::from(std::process::id());
        hello.surface = self.surface;
        if connection.send(&hello, &[]).is_err() {
            return;
        }
        self.connection = Some(connection);
        self.last_receive = now;
        self.last_surface = None;
        self.sent_focus = None;
    }
    pub(crate) fn disconnect(&mut self) {
        self.capture = false;
        self.visible = false;
        self.consumer = None;
        self.connection = None;
        self.last_surface = None;
        self.sent_focus = None;
        self.session = 0;
        self.generation = 0;
        self.hotkeys = [None; 2];
        self.shortcut_keys = [None; 2];
    }
    /// Bounded, nonblocking servicing from a game present or input poll.
    pub(crate) fn service(&mut self) {
        let now = Instant::now();
        self.connect(now);
        for _ in 0..16 {
            let Some(connection) = self.connection.as_ref() else {
                break;
            };
            let received = match connection.recv() {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(_error) => {
                    self.disconnect();
                    break;
                }
            };
            self.last_receive = now;
            if let Err(error) = self.receive(received) {
                eprintln!("Vosulay: invalid game control: {error}");
                self.disconnect();
                break;
            }
        }
        if now.duration_since(self.last_receive) >= Duration::from_secs(5) {
            self.disconnect();
        }
        self.flush_focus();
    }
    fn receive(&mut self, mut received: vosulay_transport::Received) -> io::Result<()> {
        let packet = received.packet;
        if packet.kind == Kind::Hello {
            if packet.session == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing host session",
                ));
            }
            if self.session != packet.session {
                self.consumer = None;
                self.visible = false;
                self.capture = false;
                // A new session starts with current focus, not a stale gain
                // retained from a dead attachment.
                self.focus_time = focus_timestamp().max(self.focus_time.saturating_add(1));
                self.focus_gain = if self.focused { self.focus_time } else { 0 };
                self.sent_focus = None;
                self.last_surface = None;
            }
            self.session = packet.session;
            return Ok(());
        }
        if packet.session != self.session || (packet.surface != 0 && packet.surface != self.surface)
        {
            return Ok(());
        }
        match packet.kind {
            Kind::Ring => {
                if packet.format != FORMAT_BGRA
                    || packet.stride != packet.width.saturating_mul(4)
                    || packet.generation < self.generation
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid ring metadata",
                    ));
                }
                pixel_bytes(packet.width, packet.height)?;
                let memory = received.take_handle(0).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "missing ring memory")
                })?;
                self.consumer = Some(Consumer::from_handle(memory, packet.width, packet.height)?);
                self.generation = packet.generation;
            }
            Kind::Visible => {
                self.visible = packet.code != 0;
                if !self.visible {
                    self.capture = false;
                }
            }
            Kind::Capture => self.capture = packet.code != 0 && self.visible,
            Kind::Detach => self.disconnect(),
            Kind::Ping => {}
            Kind::Configure => {
                self.hotkeys = [
                    NativeHotkey::from_wire_code(packet.value[0]),
                    NativeHotkey::from_wire_code(packet.value[1]),
                ];
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected host control",
                ));
            }
        }
        Ok(())
    }
    fn send(&mut self, mut packet: Packet) -> bool {
        if self.session == 0 {
            return false;
        }
        let Some(connection) = self.connection.as_ref() else {
            return false;
        };
        packet.session = self.session;
        packet.surface = self.surface;
        match connection.send(&packet, &[]) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(_error) => {
                self.disconnect();
                false
            }
        }
    }
    /// Surface notifications are retried if the bounded socket was full.
    pub(crate) fn present(&mut self, width: u32, height: u32, refresh_hz: f64, focused: bool) {
        self.service();
        if pixel_bytes(width, height).is_err() {
            return;
        }
        self.extent = Some((width, height));
        self.focus(focused);
        let refresh = if refresh_hz.is_finite() && refresh_hz > 0.0 {
            (refresh_hz * 1000.0).round().min(u32::MAX as f64) as u32
        } else {
            0
        };
        let state = (width, height, refresh, focused, self.focus_time);
        if self.last_surface != Some(state) {
            let mut packet = Packet::new(Kind::Surface);
            packet.sequence = self.focus_time;
            packet.width = width;
            packet.height = height;
            packet.stride = width * 4;
            packet.value[0] = self.surface;
            packet.value[1] = u64::from(refresh);
            packet.value[2] = u64::from(focused);
            packet.value[3] = self.focus_gain;
            if self.send(packet) {
                self.last_surface = Some(state);
                self.sent_focus = Some(self.focus_time);
            }
        }
    }
    pub(crate) fn focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.shortcut_keys = [None; 2];
            self.focus_time = focus_timestamp().max(self.focus_time.saturating_add(1));
            if focused {
                self.focus_gain = self.focus_time;
            }
        }
        self.flush_focus();
    }
    fn flush_focus(&mut self) {
        if self.sent_focus == Some(self.focus_time) {
            return;
        }
        let mut packet = Packet::new(Kind::Focus);
        packet.code = u32::from(self.focused);
        packet.sequence = self.focus_time;
        packet.value[3] = self.focus_gain;
        if self.send(packet) {
            self.sent_focus = Some(self.focus_time);
        }
    }
    pub(crate) fn visible(&self) -> bool {
        self.visible && self.connection.is_some()
    }
    pub(crate) fn capturing(&self) -> bool {
        self.capture
            && self.visible()
            && self.focused
            && self.last_receive.elapsed() < Duration::from_secs(5)
    }
    pub(crate) fn frame_generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn latest(&mut self) -> Option<ReadFrame> {
        if !self.visible() {
            return None;
        }
        self.consumer.as_mut()?.latest()
    }
    pub(crate) fn replay_latest(&mut self) -> Option<ReadFrame> {
        if !self.visible() {
            return None;
        }
        self.consumer.as_mut()?.replay_latest()
    }
    /// Normal-mode keyboard events let the host recognize configured shortcuts
    /// without stealing game input. Capture is dropped on undeliverable input.
    pub(crate) fn route_input(&mut self, input: BrowserInput) -> bool {
        self.service();
        let captured = self.capturing();
        let mut shortcut = matches!(
            input,
            BrowserInput::Key {
                kind: KeyKind::Character,
                ..
            }
        ) && self.shortcut_keys.iter().any(Option::is_some);
        if self.focused
            && let BrowserInput::Key {
                kind,
                modifiers,
                windows_key_code: code,
                ..
            } = input
        {
            for (index, hotkey) in self.hotkeys.iter().enumerate() {
                if self.shortcut_keys[index] == Some(code) {
                    shortcut = true;
                    if kind == KeyKind::Up {
                        self.shortcut_keys[index] = None;
                    }
                } else if kind == KeyKind::RawDown
                    && hotkey.is_some_and(|hotkey| hotkey.matches(modifiers, code))
                {
                    self.shortcut_keys[index] = Some(code);
                    shortcut = true;
                }
            }
        }
        if (captured || matches!(input, BrowserInput::Key { .. }))
            && !self.send(Packet::input(self.surface, input))
        {
            if captured {
                self.capture = false;
            }
            return false;
        }
        captured || shortcut
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.send(Packet::new(Kind::SurfaceGone));
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::Client;

    #[test]
    fn repeated_focus_observations_preserve_transition_timestamp() {
        let mut client = Client::new(1);
        client.focus(true);
        let gained = client.focus_time;
        client.focus(true);
        assert_eq!(client.focus_time, gained);
        assert_eq!(client.focus_gain, gained);
        client.focus(false);
        let lost = client.focus_time;
        client.focus(false);
        assert_eq!(client.focus_time, lost);
        assert!(lost > gained);
        assert_eq!(client.focus_gain, gained);
    }

    #[test]
    fn undelivered_gain_then_loss_keeps_latest_gain_for_surface_retry() {
        let mut client = Client::new(1);
        client.focus(true);
        let gained = client.focus_gain;
        client.focus(false);
        assert_eq!(client.sent_focus, None);
        assert!(!client.focused);
        assert_eq!(client.focus_gain, gained);
        assert!(client.focus_time > gained);
        client.focus(true);
        assert!(client.focus_gain > gained);
        assert_eq!(client.focus_gain, client.focus_time);
    }
}
