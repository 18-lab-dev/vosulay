use super::resources::{ProductResources, read_bounded};
use crate::{
    api::fetch_installed_counters_with_timeout,
    config::{AppConfig, Profile, save_to_path},
    shell_policy::{overlay_url, profile_filename},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fmt::Write as _,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use url::Url;

pub(super) struct Work {
    pub document: String,
    pub message: Value,
}

pub(super) struct Reply {
    pub document: String,
    pub envelope: Value,
    pub dialog_closed: bool,
    task_finished: bool,
}

pub(super) struct EditorWorker {
    tx: Option<SyncSender<Work>>,
    rx: Receiver<Reply>,
    thread: Option<JoinHandle<()>>,
    counters: Arc<CounterQueue>,
    counter_tx: Option<CounterSender>,
    counter_thread: Option<JoinHandle<()>>,
    pending: Cell<usize>,
}

// At shutdown there can be one active fetch and one coalesced fetch to drain.
// Two two-second global HTTP deadlines leave headroom in the host's five-second drain.
const COUNTER_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct CounterState {
    generation: u64,
    pending: Option<(u64, String)>,
    reply: Option<(u64, Reply)>,
    closed: bool,
}

struct CounterQueue {
    state: Mutex<CounterState>,
    ready: Condvar,
    host: thread::Thread,
}

struct CounterSender(Arc<CounterQueue>);

impl Drop for CounterSender {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("counter queue poisoned");
        state.closed = true;
        self.0.ready.notify_one();
    }
}

fn counter_worker(queue: Arc<CounterQueue>, backend: Url) {
    loop {
        let (generation, document) = {
            let mut state = queue.state.lock().expect("counter queue poisoned");
            loop {
                if let Some(work) = state.pending.take() {
                    break work;
                }
                if state.closed {
                    return;
                }
                state = queue.ready.wait(state).expect("counter queue poisoned");
            }
        };
        let envelope = match fetch_installed_counters_with_timeout(&backend, COUNTER_TIMEOUT) {
            Ok(counters) => json!({"type": "counters", "counters": counters}),
            Err(error) => json!({"type": "error", "message": format!("{error:#}")}),
        };
        let mut state = queue.state.lock().expect("counter queue poisoned");
        if generation == state.generation {
            state.reply = Some((
                generation,
                Reply {
                    document,
                    envelope,
                    dialog_closed: false,
                    task_finished: false,
                },
            ));
            queue.host.unpark();
        }
    }
}

impl EditorWorker {
    pub fn start(
        config: AppConfig,
        path: PathBuf,
        backend: Url,
        resources: Arc<ProductResources>,
    ) -> Result<Self> {
        let (tx, requests) = mpsc::sync_channel::<Work>(64);
        let (replies, rx) = mpsc::sync_channel(64);
        let host = thread::current();
        let counters = Arc::new(CounterQueue {
            state: Mutex::default(),
            ready: Condvar::new(),
            host: host.clone(),
        });
        let counter_tx = CounterSender(counters.clone());
        let counter_queue = counters.clone();
        let counter_backend = backend.clone();
        let counter_thread = thread::Builder::new()
            .name("vosulay-manager-counters".into())
            .spawn(move || counter_worker(counter_queue, counter_backend))
            .context("could not start counter worker")?;
        let thread = thread::Builder::new().name("vosulay-manager-tasks".into()).spawn(move || {
            let mut state = State { config, path, backend, resources, revision: 0 };
            while let Ok(work) = requests.recv() {
                let kind = work.message.get("type").and_then(Value::as_str).unwrap_or("");
                let dialog_closed = matches!(kind, "choose-overlay-file" | "import-profile" | "export-profile" | "import-tosu-layout");
                let result = state.dispatch(&work.message);
                let envelopes = match result {
                    Ok(envelopes) => envelopes,
                    Err(error) if kind == "save-config" => vec![json!({
                        "type": "save-error", "save_id": work.message.get("save_id"),
                        "message": format!("{error:#}"), "code": "save", "config": state.config, "revision": state.revision,
                    })],
                    Err(error) => vec![json!({"type": "error", "message": format!("{error:#}")})],
                };
                let count = envelopes.len();
                if count == 0 {
                    if replies.send(Reply { document: work.document, envelope: Value::Null, dialog_closed, task_finished: true }).is_err() { break; }
                    host.unpark();
                } else {
                    for (index, envelope) in envelopes.into_iter().enumerate() {
                        if replies.send(Reply { document: work.document.clone(), envelope, dialog_closed: dialog_closed && index == 0, task_finished: index + 1 == count }).is_err() { return; }
                        host.unpark();
                    }
                }
            }
        });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                drop(counter_tx);
                // No requests have been exposed yet, so this worker exits without fetching.
                let _ = counter_thread.join();
                return Err(error).context("could not start manager task worker");
            }
        };
        Ok(Self {
            tx: Some(tx),
            rx,
            thread: Some(thread),
            counters,
            counter_tx: Some(counter_tx),
            counter_thread: Some(counter_thread),
            pending: Cell::new(0),
        })
    }

    pub fn submit(&self, work: Work) -> Result<()> {
        let tx = self.tx.as_ref().context("manager worker is stopping")?;
        let kind = work
            .message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind == "refresh-counters" {
            let sender = self
                .counter_tx
                .as_ref()
                .context("counter worker is stopping")?;
            let mut state = sender.0.state.lock().expect("counter queue poisoned");
            state.generation = state.generation.wrapping_add(1);
            state.reply = None;
            state.pending = Some((state.generation, work.document));
            sender.0.ready.notify_one();
            return Ok(());
        }
        let document = (kind == "ready").then(|| work.document.clone());
        self.pending.set(self.pending.get() + 1);
        if let Err(error) = tx.try_send(work) {
            self.pending.set(self.pending.get() - 1);
            bail!("manager task queue unavailable: {error}");
        }
        if let Some(document) = document {
            // New documents invalidate old fetches and discover the backend without blocking saves.
            let mut state = self.counters.state.lock().expect("counter queue poisoned");
            state.generation = state.generation.wrapping_add(1);
            state.pending = Some((state.generation, document));
            state.reply = None;
            self.counters.ready.notify_one();
        }
        Ok(())
    }
    pub fn pending_tasks(&self) -> bool {
        self.pending.get() != 0
    }
    pub fn next_reply(&self) -> Option<Reply> {
        if let Ok(reply) = self.rx.try_recv() {
            if reply.task_finished {
                self.pending.set(self.pending.get() - 1);
            }
            return Some(reply);
        }
        let mut state = self.counters.state.lock().expect("counter queue poisoned");
        state
            .reply
            .take()
            .and_then(|(generation, reply)| (generation == state.generation).then_some(reply))
    }
    pub fn stop(&mut self) {
        self.tx.take();
        self.counter_tx.take();
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
            && self
                .counter_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
    }
    pub fn join(&mut self) -> Result<()> {
        // Join both even if either panics; neither owned thread may be left behind.
        let manager = self.thread.take().map(JoinHandle::join);
        let counters = self.counter_thread.take().map(JoinHandle::join);
        if manager.is_some_and(|result| result.is_err()) {
            bail!("manager task worker panicked");
        }
        if counters.is_some_and(|result| result.is_err()) {
            bail!("counter worker panicked");
        }
        Ok(())
    }
}

struct State {
    config: AppConfig,
    path: PathBuf,
    backend: Url,
    resources: Arc<ProductResources>,
    revision: u64,
}

impl State {
    fn dispatch(&mut self, message: &Value) -> Result<Vec<Value>> {
        ensure!(message.is_object(), "invalid manager message");
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .context("manager message has no type")?;
        match kind {
            "ready" => Ok(vec![
                json!({"type": "config", "config": self.config, "revision": self.revision}),
            ]),
            "save-config" => {
                let save_id = message
                    .get("save_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .context("invalid save ID")?;
                let revision = message
                    .get("base_revision")
                    .and_then(Value::as_u64)
                    .context("invalid configuration revision")?;
                if revision != self.revision {
                    return Ok(vec![
                        json!({"type": "save-error", "save_id": save_id, "message": "Configuration changed before this save", "code": "conflict", "config": self.config, "revision": self.revision}),
                    ]);
                }
                let mut config: AppConfig = serde_json::from_value(
                    message
                        .get("config")
                        .context("missing configuration")?
                        .clone(),
                )?;
                config.normalize();
                self.resources.authorize_profiles(&config.profiles)?;
                save_to_path(&config, &self.path)?;
                self.config = config;
                self.revision += 1;
                Ok(vec![
                    json!({"type": "saved", "save_id": save_id, "revision": self.revision, "fps_limit": self.config.settings.fps_limit, "allow_unrestricted_shared_array_buffer": self.config.settings.allow_unrestricted_shared_array_buffer, "toggle_interaction": self.config.settings.hotkeys.toggle_interaction}),
                ])
            }
            "choose-overlay-file" => {
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose HTML overlay")
                    .add_filter("HTML files", &["html", "htm"])
                    .pick_file()
                else {
                    return Ok(Vec::new());
                };
                ensure!(
                    extension_is(&path, &["html", "htm"]),
                    "selected overlay must be HTML"
                );
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).context("could not generate overlay capability")?;
                let mut token = String::with_capacity(32);
                for byte in bytes {
                    write!(token, "{byte:02x}").expect("writing to a String cannot fail");
                }
                let url = overlay_url(
                    path.to_str().context("overlay path is not valid Unicode")?,
                    &token,
                )?;
                self.resources.authorize_url(&url)?;
                Ok(vec![
                    json!({"type": "overlay-file", "url": url, "name": path.file_stem().and_then(|name| name.to_str()).unwrap_or("HTML overlay")}),
                ])
            }
            "import-profile" => {
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Import profile")
                    .add_filter("JSON profile files", &["json"])
                    .pick_file()
                else {
                    return Ok(Vec::new());
                };
                ensure!(
                    extension_is(&path, &["json"]),
                    "selected profile must be JSON"
                );
                let profile: Value = serde_json::from_slice(&read_bounded(&path, 1024 * 1024)?)?;
                ensure!(
                    profile.is_object() && profile.get("overlays").is_some_and(Value::is_array),
                    "invalid profile"
                );
                Ok(vec![json!({"type": "profile-import", "profile": profile})])
            }
            "import-tosu-layout" => {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Import tosu layout")
                    .add_filter("Tosu layout JSON", &["json"])
                    .set_file_name("__ingame__.values.json");
                if let Some(source) = super::tosu::sources().first()
                    && let Some(directory) = source.parent()
                {
                    dialog = dialog.set_directory(directory);
                }
                let Some(path) = dialog.pick_file() else {
                    return Ok(Vec::new());
                };
                let layout =
                    crate::migration::parse_tosu_layout(&read_bounded(&path, 1024 * 1024)?)?;
                let count = layout.profiles.len();
                let mut config = self.config.clone();
                crate::migration::append_layout(&mut config, layout);
                self.resources.authorize_profiles(&config.profiles)?;
                save_to_path(&config, &self.path)?;
                self.config = config;
                self.revision += 1;
                Ok(vec![
                    json!({"type": "config", "config": self.config, "revision": self.revision}),
                    json!({"type": "tosu-layout-imported", "profiles": count}),
                ])
            }
            "authorize-profile-import" => {
                let profile: Profile = serde_json::from_value(
                    message.get("profile").context("missing profile")?.clone(),
                )?;
                self.resources
                    .authorize_profiles(std::slice::from_ref(&profile))?;
                Ok(vec![
                    json!({"type": "profile-import-authorized", "profile": profile}),
                ])
            }
            "export-profile" => {
                let profile: Profile = serde_json::from_value(
                    message.get("profile").context("missing profile")?.clone(),
                )?;
                let encoded = serde_json::to_string_pretty(&profile)?;
                let filename = profile_filename(&encoded);
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Export profile")
                    .set_file_name(filename)
                    .add_filter("JSON profile files", &["json"])
                    .save_file()
                else {
                    return Ok(Vec::new());
                };
                ensure!(
                    extension_is(&path, &["json"]),
                    "export filename must end with .json"
                );
                let mut temporary = tempfile::NamedTempFile::new_in(
                    path.parent().context("export has no parent directory")?,
                )?;
                temporary.write_all(encoded.as_bytes())?;
                temporary.write_all(b"\n")?;
                temporary.as_file().sync_all()?;
                temporary.persist(&path).map_err(|error| error.error)?;
                Ok(vec![json!({"type": "profile-exported"})])
            }
            "open-settings" => {
                let id = message
                    .get("counter_id")
                    .and_then(Value::as_str)
                    .context("missing counter identifier")?;
                let mut url = self.backend.join("/settings")?;
                url.query_pairs_mut().append_pair("overlay", id);
                open::that_detached(url.as_str())?;
                Ok(Vec::new())
            }
            _ => bail!("unknown manager task: {kind}"),
        }
    }
}

fn extension_is(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Read, Write},
        net::TcpListener,
        time::Instant,
    };

    #[test]
    fn save_completes_while_counter_response_is_stalled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let backend = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4];
            stream.read_exact(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100\r\n\r\n<",
                )
                .unwrap();
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = AppConfig::default();
        config.normalize();
        let resources = Arc::new(ProductResources::new(&config).unwrap());
        let mut worker =
            EditorWorker::start(config.clone(), path.clone(), backend, resources).unwrap();
        worker
            .submit(Work {
                document: "manager".into(),
                message: json!({"type":"refresh-counters"}),
            })
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        config.profiles[0].name = "Saved while backend stalled".into();
        worker
            .submit(Work {
                document: "manager".into(),
                message: json!({
                    "type":"save-config", "save_id":"change", "base_revision":0, "config":config,
                }),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let reply = loop {
            if let Some(reply) = worker.next_reply() {
                break Some(reply);
            }
            if Instant::now() >= deadline {
                break None;
            }
            thread::sleep(Duration::from_millis(1));
        };
        release_tx.send(()).unwrap();
        worker.stop();
        while !worker.finished() {
            while worker.next_reply().is_some() {}
            thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
        server.join().unwrap();
        let reply = reply.expect("a stalled counter body must not block configuration saves");
        assert_eq!(reply.document, "manager");
        assert_eq!(reply.envelope["type"], "saved");
        assert_eq!(reply.envelope["save_id"], "change");
        assert_eq!(crate::config::load_from_path(&path).unwrap(), config);
    }

    #[test]
    fn startup_discovers_backend_without_manual_refresh() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let backend = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let expected_url = backend.join("nested%2Ffolder/").unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return,
                }
            };
            // Winsock inherits the listener's nonblocking mode; Unix does not.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            {
                // Consume headers before closing, avoiding a reset with unread
                // request bytes still pending in the socket receive buffer.
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                }
            }
            let body = include_str!("../../tests/fixtures/backend/vosu-counter-search.html");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let resources = Arc::new(ProductResources::new(&config).unwrap());
        let mut worker = EditorWorker::start(
            config,
            directory.path().join("config.json"),
            backend,
            resources,
        )
        .unwrap();
        worker
            .submit(Work {
                document: "startup".into(),
                message: json!({"type":"ready"}),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let catalog = loop {
            if let Some(reply) = worker.next_reply()
                && reply.envelope["type"] == "counters"
            {
                break Some(reply);
            }
            if Instant::now() >= deadline {
                break None;
            }
            thread::sleep(Duration::from_millis(1));
        };
        worker.stop();
        while !worker.finished() {
            while worker.next_reply().is_some() {}
            thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
        server.join().unwrap();
        let catalog = catalog.expect("startup must discover counters without an editor refresh");
        assert_eq!(catalog.document, "startup");
        let counters: Vec<crate::api::InstalledCounter> =
            serde_json::from_value(catalog.envelope["counters"].clone()).unwrap();
        assert_eq!(counters.len(), 1);
        assert_eq!(counters[0].id, "nested/folder");
        assert_eq!(counters[0].url, expected_url);
    }
}
