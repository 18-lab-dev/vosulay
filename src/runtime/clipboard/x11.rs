//! X11 `CLIPBOARD` backend for sessions without data-control.
//!
//! GNOME's Xwayland bridge mirrors every Wayland clipboard owner into
//! `CLIPBOARD` and adopts X11 owners as the Wayland clipboard, with no focus
//! check, so an ordinary X11 client can serve the editor there; plain X11
//! sessions work the same way. Selections larger than one request use the
//! INCR protocol, which is not implemented: such reads fail and such writes
//! are refused, both far beyond the editor's 256 KiB bridge limit.

use super::{Command, MAX_TEXT, READ_TIMEOUT, Selection, drain};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, mpsc},
    time::Instant,
};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE,
    connection::{Connection as _, RequestConnection as _},
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
            SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window,
            WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    utf8: Atom,
    text_plain: Atom,
    /// Our window's property that conversions are delivered into.
    property: Atom,
}

struct PendingRead {
    reply: mpsc::Sender<Selection>,
    deadline: Instant,
    target: usize,
}

pub(super) struct Service {
    connection: RustConnection,
    window: Window,
    atoms: Atoms,
    owned: Option<Arc<str>>,
    read: Option<PendingRead>,
}

impl Service {
    pub(super) fn connect() -> Option<Self> {
        let (connection, screen) = RustConnection::connect(None).ok()?;
        let root = connection.setup().roots.get(screen)?.root;
        let window = connection.generate_id().ok()?;
        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .ok()?;
        let atom = |name: &[u8]| -> Option<Atom> {
            Some(connection.intern_atom(false, name).ok()?.reply().ok()?.atom)
        };
        let atoms = Atoms {
            clipboard: atom(b"CLIPBOARD")?,
            targets: atom(b"TARGETS")?,
            utf8: atom(b"UTF8_STRING")?,
            text_plain: atom(b"text/plain;charset=utf-8")?,
            property: atom(b"VOSULAY_CLIPBOARD")?,
        };
        connection.flush().ok()?;
        Some(Self {
            connection,
            window,
            atoms,
            owned: None,
            read: None,
        })
    }

    /// Text targets asked for in order; STRING is Latin-1.
    fn read_targets(&self) -> [Atom; 3] {
        [
            self.atoms.utf8,
            self.atoms.text_plain,
            AtomEnum::STRING.into(),
        ]
    }

    pub(super) fn run(mut self, wake: OwnedFd, commands: mpsc::Receiver<Command>) {
        loop {
            loop {
                match self.connection.poll_for_event() {
                    Ok(Some(event)) => self.event(event),
                    Ok(None) => break,
                    Err(_) => return,
                }
            }
            if self.connection.flush().is_err() {
                return;
            }
            // Dropping an expired request's reply fails its read.
            if self
                .read
                .as_ref()
                .is_some_and(|read| read.deadline <= Instant::now())
            {
                self.read = None;
            }
            let timeout = self.read.as_ref().map_or(-1, |read| {
                let remaining = read.deadline.saturating_duration_since(Instant::now());
                remaining.as_millis().min(i32::MAX as u128) as i32 + 1
            });
            let mut fds = [
                libc::pollfd {
                    fd: self.connection.stream().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) } < 0 {
                continue;
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                return;
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
                if let Some(text) = &self.owned {
                    let _ = reply.send(Selection::Owned(text.clone()));
                    return;
                }
                let owner = self
                    .connection
                    .get_selection_owner(self.atoms.clipboard)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .map_or(NONE, |reply| reply.owner);
                if owner == NONE {
                    let _ = reply.send(Selection::Empty);
                    return;
                }
                // The editor worker is serial; a newer read replaces any stale one.
                self.read = Some(PendingRead {
                    reply,
                    deadline: Instant::now() + READ_TIMEOUT,
                    target: 0,
                });
                self.convert(0);
            }
            Command::Write(text, reply) => {
                let claimed = self
                    .connection
                    .set_selection_owner(self.window, self.atoms.clipboard, CURRENT_TIME)
                    .is_ok()
                    && self
                        .connection
                        .get_selection_owner(self.atoms.clipboard)
                        .ok()
                        .and_then(|cookie| cookie.reply().ok())
                        .is_some_and(|reply| reply.owner == self.window);
                if claimed {
                    self.owned = Some(text);
                }
                let _ = reply.send(claimed && self.connection.flush().is_ok());
            }
        }
    }

    fn convert(&mut self, target: usize) {
        let atom = self.read_targets()[target];
        if self
            .connection
            .convert_selection(
                self.window,
                self.atoms.clipboard,
                atom,
                self.atoms.property,
                CURRENT_TIME,
            )
            .is_err()
        {
            self.read = None;
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::SelectionNotify(event)
                if event.requestor == self.window && event.selection == self.atoms.clipboard =>
            {
                self.converted(event.property);
            }
            Event::SelectionRequest(event) => self.serve(event),
            Event::SelectionClear(event) if event.selection == self.atoms.clipboard => {
                self.owned = None;
            }
            _ => {}
        }
    }

    fn converted(&mut self, property: Atom) {
        let targets = self.read_targets();
        let Some(read) = self.read.take() else {
            return;
        };
        if property == NONE {
            let target = read.target + 1;
            if target < targets.len() {
                self.read = Some(PendingRead { target, ..read });
                self.convert(target);
            } else {
                let _ = read.reply.send(Selection::Empty);
            }
            return;
        }
        let target = targets[read.target];
        let Some(value) = self
            .connection
            .get_property(
                true,
                self.window,
                self.atoms.property,
                AtomEnum::ANY,
                0,
                (MAX_TEXT / 4) as u32,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return;
        };
        // INCR transfers and oversized text fail the read by dropping its reply.
        if value.format != 8 || value.bytes_after != 0 {
            return;
        }
        let text = if target == u32::from(AtomEnum::STRING) {
            value.value.iter().map(|&byte| char::from(byte)).collect()
        } else {
            String::from_utf8_lossy(&value.value).into_owned()
        };
        let _ = read.reply.send(Selection::Text(text));
    }

    fn serve(&self, request: SelectionRequestEvent) {
        // Obsolete clients use the target as the property.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let stored = match &self.owned {
            Some(text)
                if request.owner == self.window && request.selection == self.atoms.clipboard =>
            {
                if request.target == self.atoms.targets {
                    let targets = [self.atoms.targets, self.atoms.utf8, self.atoms.text_plain];
                    self.connection
                        .change_property32(
                            PropMode::REPLACE,
                            request.requestor,
                            property,
                            AtomEnum::ATOM,
                            &targets,
                        )
                        .is_ok()
                } else if (request.target == self.atoms.utf8
                    || request.target == self.atoms.text_plain)
                    && text.len() + 64 <= self.connection.maximum_request_bytes()
                {
                    self.connection
                        .change_property8(
                            PropMode::REPLACE,
                            request.requestor,
                            property,
                            request.target,
                            text.as_bytes(),
                        )
                        .is_ok()
                } else {
                    false
                }
            }
            _ => false,
        };
        let _ = self.connection.send_event(
            false,
            request.requestor,
            EventMask::NO_EVENT,
            SelectionNotifyEvent {
                response_type: SELECTION_NOTIFY_EVENT,
                sequence: 0,
                time: request.time,
                requestor: request.requestor,
                selection: request.selection,
                target: request.target,
                property: if stored { property } else { NONE },
            },
        );
    }
}
