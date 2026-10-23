use super::resources::{ProductResources, read_bounded};
use crate::{
    api::VosuClient,
    config::{AppConfig, Profile, save_to_path},
    shell_policy::{overlay_url, profile_filename},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
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
mod rulesets;

pub(super) struct Work {
    pub document: String,
    pub message: Value,
}

pub(super) struct Reply {
    pub document: String,
    pub envelope: Value,
    pub dialog_closed: bool,
    /// For whichever manager document is open (the manage page changed the configuration).
    pub broadcast: bool,
    task_finished: bool,
}

enum Task {
    Manager(Work),
    Remote(RemoteCall, SyncSender<Result<Value>>),
    Ruleset(Option<crate::ruleset::Ruleset>),
    OpenDashboard,
}

enum RemoteCall {
    State,
    Message(Value),
}

/// Shared with the manage page, which stops reaching the worker when it stops.
type TaskSender = Arc<Mutex<Option<SyncSender<Task>>>>;

pub(super) struct EditorWorker {
    tx: TaskSender,
    rx: Receiver<Reply>,
    thread: Option<JoinHandle<()>>,
    counters: Arc<CounterQueue>,
    counter_tx: Option<CounterSender>,
    counter_thread: Option<JoinHandle<()>>,
    vosu: Arc<VosuClient>,
    vosu_tx: Option<SyncSender<Work>>,
    vosu_thread: Option<JoinHandle<()>>,
    /// Errors for vosu requests that found the worker's queue full.
    vosu_busy: RefCell<VecDeque<Reply>>,
    pending: Cell<usize>,
    rulesets: Option<rulesets::Watcher>,
}

// The manage page waits at most this long for the worker (a save waits behind queued
// manager tasks); native file dialogs instead wait for the user.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);

/// Manager messages the manage page may send. Clipboard requests are absent: the
/// browser's own clipboard serves the page.
const PAGE_MESSAGES: &[&str] = &[
    "save-config",
    "choose-overlay-file",
    "import-profile",
    "export-profile",
    "import-tosu-layout",
    "authorize-profile-import",
    "open-settings",
];

/// The manager's vosu overlay settings requests. They wait on vosu's HTTP API, so they run beside
/// the task thread instead of holding up saves.
const VOSU_MESSAGES: &[&str] = &[
    "vosu-settings-load",
    "vosu-settings-save",
    "vosu-profile-action",
    "vosu-settings-link",
];

fn opens_dialog(kind: &str) -> bool {
    matches!(
        kind,
        "choose-overlay-file" | "import-profile" | "export-profile" | "import-tosu-layout"
    )
}

/// The envelope answering one vosu settings request; a `button` link only answers on failure.
fn vosu_reply(client: &VosuClient, message: &Value) -> Option<Value> {
    if message.get("type").and_then(Value::as_str) != Some("vosu-settings-link") {
        return Some(client.handle(message));
    }
    let opened = client
        .button_link(message)
        .and_then(|url| open::that_detached(url.as_str()).map_err(Into::into));
    opened.err().map(
        |error| json!({"type": "error", "message": format!("Could not open the link: {error:#}")}),
    )
}

struct RemoteEditor(TaskSender, Arc<VosuClient>);

impl RemoteEditor {
    fn call(&self, call: RemoteCall, timeout: Option<Duration>) -> Result<Value> {
        let (reply, response) = mpsc::sync_channel(1);
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("manager worker poisoned"))?
            .as_ref()
            .context("vosulay is shutting down")?
            .try_send(Task::Remote(call, reply))
            .map_err(|_| anyhow::anyhow!("vosulay is busy; try again"))?;
        match timeout {
            Some(timeout) => response
                .recv_timeout(timeout)
                .context("vosulay did not answer in time")?,
            None => response.recv().context("vosulay is shutting down")?,
        }
    }
}

impl crate::remote::ManageEditor for RemoteEditor {
    fn state(&self) -> Result<Value> {
        self.call(RemoteCall::State, Some(REMOTE_TIMEOUT))
    }
    fn message(&self, message: Value) -> Result<Value> {
        let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
        if VOSU_MESSAGES.contains(&kind) {
            // Answered on the page's own request thread, so vosu's latency never holds the worker.
            return Ok(Value::Array(
                vosu_reply(&self.1, &message).into_iter().collect(),
            ));
        }
        let timeout = (!opens_dialog(kind)).then_some(REMOTE_TIMEOUT);
        self.call(RemoteCall::Message(message), timeout)
    }
    fn counters(&self) -> Value {
        self.1.counters_envelope(crate::api::FETCH_DEADLINE)
    }
}

// At shutdown there can be one active fetch and one coalesced fetch to drain.
// Two two-second global HTTP deadlines leave headroom in the host's five-second
// drain; the catalog and vosu profile requests share each deadline. vosu settings
// requests use the same bound per request.
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

fn counter_worker(queue: Arc<CounterQueue>, client: Arc<VosuClient>) {
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
        let envelope = client.counters_envelope(COUNTER_TIMEOUT);
        let mut state = queue.state.lock().expect("counter queue poisoned");
        if generation == state.generation {
            state.reply = Some((
                generation,
                Reply {
                    document,
                    envelope,
                    dialog_closed: false,
                    broadcast: false,
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
        let watched_backend = backend.clone();
        let (tx, requests) = mpsc::sync_channel::<Task>(64);
        let (replies, rx) = mpsc::sync_channel(64);
        let host = thread::current();
        let counters = Arc::new(CounterQueue {
            state: Mutex::default(),
            ready: Condvar::new(),
            host: host.clone(),
        });
        let counter_tx = CounterSender(counters.clone());
        let counter_queue = counters.clone();
        let vosu = Arc::new(VosuClient::new(backend.clone(), COUNTER_TIMEOUT));
        let counter_client = vosu.clone();
        let counter_thread = thread::Builder::new()
            .name("vosulay-manager-counters".into())
            .spawn(move || counter_worker(counter_queue, counter_client))
            .context("could not start counter worker")?;
        let (vosu_tx, vosu_requests) = mpsc::sync_channel::<Work>(16);
        let (vosu_client, vosu_replies, vosu_host) = (vosu.clone(), replies.clone(), host.clone());
        let vosu_thread = thread::Builder::new()
            .name("vosulay-manager-vosu".into())
            .spawn(move || {
                while let Ok(work) = vosu_requests.recv() {
                    let Some(envelope) = vosu_reply(&vosu_client, &work.message) else {
                        continue;
                    };
                    let reply = Reply {
                        document: work.document,
                        envelope,
                        dialog_closed: false,
                        broadcast: false,
                        task_finished: false,
                    };
                    if vosu_replies.send(reply).is_err() {
                        return;
                    }
                    vosu_host.unpark();
                }
            });
        let vosu_thread = match vosu_thread {
            Ok(thread) => thread,
            Err(error) => {
                drop(counter_tx);
                let _ = counter_thread.join();
                return Err(error).context("could not start vosu settings worker");
            }
        };
        let thread = thread::Builder::new().name("vosulay-manager-tasks".into()).spawn(move || {
            let mut state = State {
                config,
                path,
                backend,
                resources,
                revision: 0,
                ruleset: crate::ruleset::Selection::default(),
                granted: Default::default(),
                #[cfg(target_os = "linux")]
                clipboard: super::clipboard::Clipboard::connect(),
            };
            while let Ok(task) = requests.recv() {
                let work = match task {
                    Task::OpenDashboard => {
                        super::lifecycle::open_dashboard();
                        continue;
                    }
                    Task::Ruleset(ruleset) => {
                        let envelopes = match state.ruleset(ruleset) {
                            Ok(envelopes) => envelopes,
                            Err(error) => vec![json!({"type":"error","message":format!("Could not switch ruleset profile: {error:#}")})],
                        };
                        if envelopes.is_empty() { continue; }
                        for envelope in envelopes {
                            if replies.send(Reply { document: String::new(), envelope, dialog_closed: false, broadcast: true, task_finished: false }).is_err() { return; }
                        }
                        host.unpark();
                        continue;
                    }
                    Task::Manager(work) => work,
                    Task::Remote(call, reply) => {
                        let (result, changed) = state.remote(call);
                        // The page may have timed out and gone; the open manager still updates.
                        let _ = reply.send(result);
                        if let Some(envelope) = changed {
                            if replies.send(Reply { document: String::new(), envelope, dialog_closed: false, broadcast: true, task_finished: false }).is_err() { return; }
                            host.unpark();
                        }
                        continue;
                    }
                };
                let kind = work.message.get("type").and_then(Value::as_str).unwrap_or("");
                let dialog_closed = opens_dialog(kind);
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
                    if replies.send(Reply { document: work.document, envelope: Value::Null, dialog_closed, broadcast: false, task_finished: true }).is_err() { break; }
                    host.unpark();
                } else {
                    for (index, envelope) in envelopes.into_iter().enumerate() {
                        if replies.send(Reply { document: work.document.clone(), envelope, dialog_closed: dialog_closed && index == 0, broadcast: false, task_finished: index + 1 == count }).is_err() { return; }
                        host.unpark();
                    }
                }
            }
        });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                drop(counter_tx);
                drop(vosu_tx);
                // No requests have been exposed yet, so these workers exit without fetching.
                let _ = counter_thread.join();
                let _ = vosu_thread.join();
                return Err(error).context("could not start manager task worker");
            }
        };
        let mut worker = Self {
            tx: Arc::new(Mutex::new(Some(tx))),
            rx,
            thread: Some(thread),
            counters,
            counter_tx: Some(counter_tx),
            counter_thread: Some(counter_thread),
            vosu,
            vosu_tx: Some(vosu_tx),
            vosu_thread: Some(vosu_thread),
            vosu_busy: RefCell::default(),
            pending: Cell::new(0),
            rulesets: None,
        };
        match rulesets::Watcher::start(watched_backend, worker.tx.clone()) {
            Ok(watcher) => worker.rulesets = Some(watcher),
            Err(error) => {
                worker.stop();
                worker.join()?;
                return Err(error);
            }
        }
        Ok(worker)
    }

    pub fn submit(&self, work: Work) -> Result<()> {
        let tx = self
            .tx
            .lock()
            .map_err(|_| anyhow::anyhow!("manager worker poisoned"))?;
        let tx = tx.as_ref().context("manager worker is stopping")?;
        let kind = work
            .message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if VOSU_MESSAGES.contains(&kind) {
            let sender = self
                .vosu_tx
                .as_ref()
                .context("vosu settings worker is stopping")?;
            return match sender.try_send(work) {
                Ok(()) => Ok(()),
                // A page flooding vosu requests gets an answer, not a failed host loop.
                Err(mpsc::TrySendError::Full(work)) => {
                    let mut busy = self.vosu_busy.borrow_mut();
                    if busy.len() < 16 {
                        busy.push_back(Reply {
                            envelope: json!({
                                "type": "vosu-settings-error",
                                "request_id": work.message.get("request_id"),
                                "message": "vosulay is still busy with earlier vosu requests; try again",
                            }),
                            document: work.document,
                            dialog_closed: false,
                            broadcast: false,
                            task_finished: false,
                        });
                    }
                    Ok(())
                }
                Err(mpsc::TrySendError::Disconnected(_)) => bail!("vosu settings worker stopped"),
            };
        }
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
        if let Err(error) = tx.try_send(Task::Manager(work)) {
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
        if let Some(reply) = self.vosu_busy.borrow_mut().pop_front() {
            return Some(reply);
        }
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
        if let Some(watcher) = &mut self.rulesets {
            watcher.stop();
        }
        // Also disconnects the manage page, so the task thread can finish.
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
        self.counter_tx.take();
        self.vosu_tx.take();
    }
    /// The manage page's route into this worker (see `crate::remote::ManageEditor`).
    pub fn manage_editor(&self) -> Arc<dyn crate::remote::ManageEditor> {
        Arc::new(RemoteEditor(self.tx.clone(), self.vosu.clone()))
    }
    pub(super) fn open_dashboard(&self) -> Result<()> {
        self.tx
            .lock()
            .map_err(|_| anyhow::anyhow!("manager worker poisoned"))?
            .as_ref()
            .context("vosulay is shutting down")?
            .try_send(Task::OpenDashboard)
            .map_err(|_| anyhow::anyhow!("vosulay is busy; try again"))?;
        Ok(())
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
            && self
                .rulesets
                .as_ref()
                .is_none_or(rulesets::Watcher::finished)
            && self
                .counter_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            && self
                .vosu_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
    }
    pub fn join(&mut self) -> Result<()> {
        // Join all even if one panics; no owned thread may be left behind.
        let manager = self.thread.take().map(JoinHandle::join);
        let counters = self.counter_thread.take().map(JoinHandle::join);
        let vosu = self.vosu_thread.take().map(JoinHandle::join);
        if let Some(watcher) = &mut self.rulesets {
            watcher.join()?;
        }
        if manager.is_some_and(|result| result.is_err()) {
            bail!("manager task worker panicked");
        }
        if counters.is_some_and(|result| result.is_err()) {
            bail!("counter worker panicked");
        }
        if vosu.is_some_and(|result| result.is_err()) {
            bail!("vosu settings worker panicked");
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
    ruleset: crate::ruleset::Selection,
    /// `vosulay-overlay:` URLs from native file choices (picker, profile import) this run.
    granted: std::collections::HashSet<String>,
    #[cfg(target_os = "linux")]
    clipboard: Option<super::clipboard::Clipboard>,
}

impl State {
    fn ruleset(&mut self, ruleset: Option<crate::ruleset::Ruleset>) -> Result<Vec<Value>> {
        let availability_changed = self.ruleset.current != ruleset;
        let changed = self.ruleset.update(ruleset);
        if !changed && !availability_changed {
            return Ok(Vec::new());
        }
        let mut envelopes = vec![json!({"type":"ruleset","ruleset":ruleset})];
        if changed
            && let Some(profile) = self
                .config
                .profiles
                .iter()
                .find(|profile| profile.ruleset == ruleset)
            && profile.id != self.config.active_profile
        {
            let mut config = self.config.clone();
            config.active_profile.clone_from(&profile.id);
            save_to_path(&config, &self.path)?;
            self.config = config;
            self.revision += 1;
            envelopes.push(json!({"type":"config","config":self.config,"revision":self.revision}));
        }
        Ok(envelopes)
    }
    /// Whether vosulay serves the page's clipboard instead of Chromium.
    fn host_clipboard(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.clipboard.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Returns the page's reply and, when the page changed the configuration, the
    /// envelope for the open in-game manager.
    fn remote(&mut self, call: RemoteCall) -> (Result<Value>, Option<Value>) {
        match call {
            RemoteCall::State => (
                Ok(
                    json!({"config": self.config, "revision": self.revision, "ruleset": self.ruleset.current}),
                ),
                None,
            ),
            RemoteCall::Message(message) => {
                let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
                if !PAGE_MESSAGES.contains(&kind) {
                    return (Err(anyhow::anyhow!("unsupported manager message")), None);
                }
                let revision = self.revision;
                let envelopes = match self
                    .ensure_known_local_overlays(&message)
                    .and_then(|()| self.dispatch(&message))
                {
                    Ok(envelopes) => envelopes,
                    Err(error) if kind == "save-config" => vec![json!({
                        "type": "save-error", "save_id": message.get("save_id"),
                        "message": format!("{error:#}"), "code": "save", "config": self.config, "revision": self.revision,
                    })],
                    Err(error) => vec![json!({"type": "error", "message": format!("{error:#}")})],
                };
                let changed = (self.revision != revision).then(
                    || json!({"type": "config", "config": self.config, "revision": self.revision}),
                );
                (Ok(Value::Array(envelopes)), changed)
            }
        }
    }

    /// A `vosulay-overlay:` URL is a filesystem grant, so the page may only send ones
    /// already configured or chosen by the user in a native dialog during this run;
    /// otherwise a page script could grant itself a new filesystem root.
    fn ensure_known_local_overlays(&self, message: &Value) -> Result<()> {
        let profiles: Vec<&Value> = match message.get("type").and_then(Value::as_str) {
            Some("save-config") => message
                .pointer("/config/profiles")
                .and_then(Value::as_array)
                .map(|profiles| profiles.iter().collect())
                .unwrap_or_default(),
            Some("authorize-profile-import") => message.get("profile").into_iter().collect(),
            _ => return Ok(()),
        };
        let configured: std::collections::HashSet<&str> = self
            .config
            .profiles
            .iter()
            .flat_map(|profile| &profile.overlays)
            .filter_map(|overlay| overlay.url.as_ref())
            .filter(|url| url.scheme() == "vosulay-overlay")
            .map(Url::as_str)
            .collect();
        for profile in profiles {
            let overlays = profile.get("overlays").and_then(Value::as_array);
            for url in overlays.into_iter().flatten().filter_map(|overlay| {
                overlay
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(|url| Url::parse(url).ok())
            }) {
                ensure!(
                    url.scheme() != "vosulay-overlay"
                        || configured.contains(url.as_str())
                        || self.granted.contains(url.as_str()),
                    "local HTML overlays must be chosen with the HTML file button or a profile import"
                );
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, message: &Value) -> Result<Vec<Value>> {
        ensure!(message.is_object(), "invalid manager message");
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .context("manager message has no type")?;
        match kind {
            "ready" => Ok(vec![
                json!({"type": "config", "config": self.config, "revision": self.revision}),
                json!({"type":"ruleset","ruleset":self.ruleset.current}),
                // Elsewhere Chromium's own clipboard works for the page.
                json!({"type": "clipboard-support", "available": self.host_clipboard()}),
            ]),
            #[cfg(target_os = "linux")]
            "clipboard-read" => {
                let request = message
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 64)
                    .context("invalid clipboard request")?;
                let text = self
                    .clipboard
                    .as_ref()
                    .context("system clipboard unavailable")?
                    .read()?;
                Ok(vec![
                    json!({"type": "clipboard-text", "request_id": request, "text": text}),
                ])
            }
            #[cfg(target_os = "linux")]
            "clipboard-write" => {
                let request = message
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 64)
                    .context("invalid clipboard request")?;
                let text = message
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| text.len() <= 1 << 20)
                    .context("invalid clipboard text")?;
                self.clipboard
                    .as_ref()
                    .context("system clipboard unavailable")?
                    .write(text)?;
                // A cut deletes its selection only after this acknowledgement.
                Ok(vec![
                    json!({"type": "clipboard-written", "request_id": request}),
                ])
            }
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
                Ok(vec![json!({
                    "type": "saved",
                    "save_id": save_id,
                    "revision": self.revision,
                    "fps_limit": self.config.settings.fps_limit,
                    "allow_unrestricted_shared_array_buffer": self.config.settings.allow_unrestricted_shared_array_buffer,
                    "toggle_interaction": self.config.settings.hotkeys.toggle_interaction,
                    "toggle_edit": self.config.settings.hotkeys.toggle_edit,
                    "toggle_visibility": self.config.settings.hotkeys.toggle_visibility,
                    "exit_edit_mode_on_focus_loss": self.config.settings.exit_edit_mode_on_focus_loss,
                })])
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
                if let Ok(parsed) = Url::parse(&url) {
                    self.granted.insert(parsed.into());
                }
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
                let overlays = profile.get("overlays").and_then(Value::as_array);
                self.granted.extend(
                    overlays
                        .into_iter()
                        .flatten()
                        .filter_map(|overlay| overlay.get("url").and_then(Value::as_str))
                        .filter_map(|url| Url::parse(url).ok())
                        .filter(|url| url.scheme() == "vosulay-overlay")
                        .map(String::from),
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
                // vosu's overlay origin redirects /settings to its dashboard, but the lookup keeps the
                // opened address the one vosu reports. It shares the counter fetch's bound.
                let mut url = crate::api::settings_page(&self.backend, COUNTER_TIMEOUT)?;
                url.query_pairs_mut().append_pair("overlay", id);
                if let Some(profile) = message.get("profile").and_then(Value::as_str) {
                    ensure!(
                        crate::config::valid_vosu_profile(profile),
                        "invalid vosu profile name"
                    );
                    url.query_pairs_mut().append_pair("profile", profile);
                }
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
    fn manage_page_saves_full_config_but_cannot_grant_unchosen_local_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let placement = crate::config::Placement {
            counter_id: "accuracy".into(),
            ..crate::config::Placement::default()
        };
        let mut config = AppConfig::default();
        config.profiles[0].overlays.push(placement.clone());
        config.profiles.push(Profile {
            id: "other".into(),
            name: "Other".into(),
            overlays: vec![placement.clone()],
            ruleset: None,
        });
        config.normalize();
        let resources = Arc::new(ProductResources::new(&config).unwrap());
        let mut worker = EditorWorker::start(
            config.clone(),
            path.clone(),
            Url::parse("http://127.0.0.1:9/").unwrap(),
            resources,
        )
        .unwrap();
        let editor = worker.manage_editor();

        let state = editor.state().unwrap();
        assert_eq!(state["revision"], 0);
        assert_eq!(state["config"]["profiles"].as_array().unwrap().len(), 2);

        // Native-bridge-only requests stay with the in-game manager.
        assert!(editor.message(json!({"type": "clipboard-read"})).is_err());
        assert!(editor.message(json!({"type": "ready"})).is_err());

        let mut edited = config.clone();
        edited.profiles[1].name = "Renamed by page".into();
        edited.profiles[0].overlays[0].left = 321.0;
        edited.settings.fps_limit = 90;
        let save = |config: &AppConfig, id: &str, base: u64| {
            editor
                .message(json!({"type": "save-config", "save_id": id, "base_revision": base, "config": config}))
                .unwrap()
        };
        let reply = save(&edited, "a", 0);
        assert_eq!(reply[0]["type"], "saved");
        assert_eq!(reply[0]["revision"], 1);
        let saved = crate::config::load_from_path(&path).unwrap();
        assert_eq!(saved, edited);
        let deadline = Instant::now() + Duration::from_secs(1);
        let broadcast = loop {
            if let Some(reply) = worker.next_reply() {
                break reply;
            }
            assert!(
                Instant::now() < deadline,
                "the open manager must hear about page saves"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert!(broadcast.broadcast);
        assert_eq!(broadcast.envelope["type"], "config");
        assert_eq!(broadcast.envelope["revision"], 1);
        assert_eq!(broadcast.envelope["config"]["settings"]["fps_limit"], 90);

        let stale = save(&edited, "b", 0);
        assert_eq!(stale[0]["type"], "save-error");
        assert_eq!(stale[0]["code"], "conflict");

        let mut granting = edited.clone();
        granting.profiles[0].overlays[0].url = Some(
            Url::parse("vosulay-overlay://0123456789abcdef0123456789abcdef/Lw/x.html").unwrap(),
        );
        let grant = save(&granting, "c", 1);
        assert_eq!(grant[0]["type"], "save-error");
        assert_eq!(crate::config::load_from_path(&path).unwrap(), saved);
        let import = editor
            .message(json!({"type": "authorize-profile-import", "profile": granting.profiles[0]}))
            .unwrap();
        assert_eq!(import[0]["type"], "error");

        worker.stop();
        while !worker.finished() {
            while worker.next_reply().is_some() {}
            thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
    }

    #[test]
    fn startup_discovers_backend_without_manual_refresh() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let backend = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let expected_url = backend.join("nested%2Ffolder/").unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => return,
                };
                // The ruleset watcher may connect before the catalog request.
                // Exercise both consumers instead of assuming the first socket
                // is necessarily the counter worker.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                if first.starts_with("GET /api/counters/search/") {
                    let body =
                        include_str!("../../tests/fixtures/backend/vosu-counter-search.html");
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    return;
                }
                write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
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
