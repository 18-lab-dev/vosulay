//! Linux system clipboard for the editor's text fields.
//!
//! Windowless Chromium cannot use the clipboard while a native Wayland game
//! has keyboard focus: a Wayland compositor offers selections only to the
//! focused client. vosulay therefore reads and owns the clipboard itself,
//! through the wlr data-control protocol, which works without focus, or else
//! as an X11 client (see [`x11`]) where no data-control exists, as on GNOME.

mod x11;

use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

/// Preferred first; X11 names cover Xwayland-backed sources.
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_TEXT: usize = 1 << 20;

enum Command {
    Read(mpsc::Sender<Selection>),
    /// Replies once the selection request has been flushed to the compositor.
    Write(Arc<str>, mpsc::Sender<bool>),
}

enum Selection {
    Empty,
    Owned(Arc<str>),
    Pipe(OwnedFd),
    Text(String),
}

pub(crate) struct Clipboard {
    commands: mpsc::Sender<Command>,
    wake: OwnedFd,
}

impl Clipboard {
    /// `None` when neither data-control nor an X display is available.
    pub(crate) fn connect() -> Option<Self> {
        let (wake_read, wake) = pipe()?;
        let (commands, receiver) = mpsc::channel();
        let run: Box<dyn FnOnce() + Send> = match Service::connect() {
            Some(service) => Box::new(move || service.run(wake_read, receiver)),
            None => {
                let service = x11::Service::connect()?;
                Box::new(move || service.run(wake_read, receiver))
            }
        };
        thread::Builder::new()
            .name("vosulay-clipboard".into())
            .spawn(run)
            .ok()?;
        Some(Self { commands, wake })
    }

    pub(crate) fn read(&self) -> Result<String> {
        let (reply, selection) = mpsc::channel();
        self.send(Command::Read(reply))?;
        let selection = selection
            .recv_timeout(READ_TIMEOUT)
            .context("clipboard service did not answer")?;
        Ok(match selection {
            Selection::Empty => String::new(),
            Selection::Owned(text) => text.to_string(),
            Selection::Pipe(fd) => read_pipe(fd)?,
            Selection::Text(text) => text,
        })
    }

    /// Returns only after the service has claimed the clipboard; a cut
    /// deletes its selection on this success, so a stopped service must fail.
    pub(crate) fn write(&self, text: &str) -> Result<()> {
        let (reply, applied) = mpsc::channel();
        self.send(Command::Write(text.into(), reply))?;
        let applied = applied
            .recv_timeout(READ_TIMEOUT)
            .context("clipboard service did not store the text")?;
        ensure!(applied, "could not set the system clipboard");
        Ok(())
    }

    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .ok()
            .context("clipboard service stopped")?;
        wake(&self.wake);
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // The service sees the closed channel after this wake and exits.
        wake(&self.wake);
    }
}

#[derive(Default)]
struct State {
    selection: Option<ZwlrDataControlOfferV1>,
    source: Option<(ZwlrDataControlSourceV1, Arc<str>)>,
    finished: bool,
}

struct Service {
    connection: Connection,
    queue: EventQueue<State>,
    manager: ZwlrDataControlManagerV1,
    device: ZwlrDataControlDeviceV1,
    state: State,
}

impl Service {
    fn connect() -> Option<Self> {
        let connection = Connection::connect_to_env().ok()?;
        let (globals, queue) = registry_queue_init::<State>(&connection).ok()?;
        let handle = queue.handle();
        let seat: WlSeat = globals.bind(&handle, 1..=1, ()).ok()?;
        let manager: ZwlrDataControlManagerV1 = globals.bind(&handle, 1..=2, ()).ok()?;
        let device = manager.get_data_device(&seat, &handle, ());
        Some(Self {
            connection,
            queue,
            manager,
            device,
            state: State::default(),
        })
    }

    fn run(mut self, wake: OwnedFd, commands: mpsc::Receiver<Command>) {
        loop {
            if self.queue.dispatch_pending(&mut self.state).is_err() || self.state.finished {
                return;
            }
            if self.connection.flush().is_err() {
                return;
            }
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let mut fds = [
                libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
                drop(guard);
                continue;
            }
            if fds[0].revents != 0 {
                if guard.read().is_err() {
                    return;
                }
            } else {
                drop(guard);
            }
            if fds[1].revents != 0 {
                drain(&wake);
                loop {
                    match commands.try_recv() {
                        Ok(command) => self.handle(command),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Read(reply) => {
                let _ = reply.send(self.selection());
            }
            Command::Write(text, reply) => {
                let handle = self.queue.handle();
                let source = self.manager.create_data_source(&handle, text.clone());
                for kind in TEXT_TYPES {
                    source.offer(kind.into());
                }
                self.device.set_selection(Some(&source));
                if let Some((previous, _)) = self.state.source.replace((source, text)) {
                    previous.destroy();
                }
                let _ = reply.send(self.connection.flush().is_ok());
            }
        }
    }

    fn selection(&self) -> Selection {
        if let Some((_, text)) = &self.state.source {
            // Reading our own offer would wait on this thread's send handler.
            return Selection::Owned(text.clone());
        }
        let Some(offer) = &self.state.selection else {
            return Selection::Empty;
        };
        let kind = {
            let offered = offer
                .data::<Mutex<Vec<String>>>()
                .map(|kinds| kinds.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_default();
            TEXT_TYPES
                .into_iter()
                .find(|kind| offered.iter().any(|offered| offered == kind))
        };
        let Some(kind) = kind else {
            return Selection::Empty;
        };
        let Some((read, write)) = pipe() else {
            return Selection::Empty;
        };
        offer.receive(kind.into(), write.as_fd());
        // The source writes once the request reaches the compositor; our copy
        // of the write end must close so the reader sees end-of-file.
        if self.connection.flush().is_err() {
            return Selection::Empty;
        }
        drop(write);
        Selection::Pipe(read)
    }
}

fn read_pipe(fd: OwnedFd) -> Result<String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("clipboard owner did not send its text in time");
        }
        let mut ready = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut ready, 1, timeout) } <= 0 {
            continue;
        }
        let count = file.read(&mut chunk).context("could not read clipboard")?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_TEXT {
            bail!("clipboard text is larger than 1 MiB");
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return None;
    }
    // SAFETY: pipe2 returned two new descriptors owned by nothing else.
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn wake(fd: &OwnedFd) {
    unsafe { libc::write(fd.as_raw_fd(), [0_u8].as_ptr().cast(), 1) };
}

fn drain(fd: &OwnedFd) {
    let mut buffer = [0_u8; 64];
    while unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlManagerV1,
        _: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::Selection { id } => {
                if let Some(previous) = std::mem::replace(&mut state.selection, id) {
                    previous.destroy();
                }
            }
            // Only the clipboard is used; primary-selection offers are released.
            zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                offer.destroy();
            }
            zwlr_data_control_device_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, Mutex::new(Vec::new())),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, Mutex<Vec<String>>> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        kinds: &Mutex<Vec<String>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            kinds
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Arc<str>> for State {
    fn event(
        state: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        text: &Arc<str>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { fd, .. } => {
                // A slow reader must not stall this thread's other requests.
                let text = text.clone();
                let _ = thread::Builder::new()
                    .name("vosulay-clipboard-send".into())
                    .spawn(move || {
                        let mut file = std::fs::File::from(fd);
                        let _ = clear_nonblocking(&file)
                            .and_then(|()| file.write_all(text.as_bytes()).map_err(Into::into));
                    });
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                source.destroy();
                if state
                    .source
                    .as_ref()
                    .is_some_and(|(owned, _)| owned == source)
                {
                    state.source = None;
                }
            }
            _ => {}
        }
    }
}

fn clear_nonblocking(file: &std::fs::File) -> Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        bail!("could not prepare clipboard transfer");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A service that stops after taking a write must not report success.
    #[test]
    fn write_fails_when_the_service_stops_before_applying_it() {
        let (wake_read, wake) = pipe().unwrap();
        let (commands, receiver) = mpsc::channel();
        let service = thread::spawn(move || {
            let _wake = wake_read;
            // Dropping the command drops its reply sender unanswered.
            drop(receiver.recv().unwrap());
        });
        let clipboard = Clipboard { commands, wake };
        assert!(clipboard.write("cut text").is_err());
        service.join().unwrap();
    }
}
