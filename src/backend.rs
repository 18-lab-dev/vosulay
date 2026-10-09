use anyhow::{Context, Result, anyhow, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Write,
    net::TcpStream,
    sync::{Arc, Mutex, OnceLock},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Builder,
    sync::{Notify, mpsc, oneshot, watch},
    task::JoinSet,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message, Utf8Bytes,
        protocol::{CloseFrame, Role, WebSocketConfig},
    },
};
use url::Url;

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const MAX_CLIENTS: usize = 128;
const COMMANDS: &str = "/websocket/commands";
type Upstream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone, Copy)]
enum Schema {
    Osu,
    Sc,
    V2,
    V2Precise,
}

impl Schema {
    fn index(self) -> usize {
        self as usize
    }
    fn path(self) -> &'static str {
        match self {
            Self::Osu => "/ws",
            Self::Sc => "/tokens",
            Self::V2 => "/websocket/v2",
            Self::V2Precise => "/websocket/v2/precise",
        }
    }
}

#[derive(Clone, Copy)]
enum Endpoint {
    Telemetry(Schema),
    // Selected-field subscriptions must follow vosu's precise publication cadence; projecting them
    // from the shared regular v2 stream delayed every update by up to POLL_RATE.
    Relay(&'static str),
}

fn endpoint(target: &str) -> Option<Endpoint> {
    match target.split('?').next()? {
        "/ws" => Some(Endpoint::Telemetry(Schema::Osu)),
        "/tokens" => Some(Endpoint::Telemetry(Schema::Sc)),
        "/websocket/v2" => Some(Endpoint::Telemetry(Schema::V2)),
        "/websocket/v2/precise" => Some(Endpoint::Telemetry(Schema::V2Precise)),
        "/websocket/v2/subscribe" => Some(Endpoint::Relay("/websocket/v2/subscribe")),
        "/websocket/commands" => Some(Endpoint::Relay(COMMANDS)),
        _ => None,
    }
}

pub(crate) fn supports_websocket(target: &str) -> bool {
    endpoint(target).is_some()
}

struct Snapshot {
    text: Utf8Bytes,
    document: OnceLock<Option<Value>>,
    projections: Mutex<HashMap<Arc<[Filter]>, Utf8Bytes>>,
}

impl Snapshot {
    fn document(&self) -> Option<&Value> {
        self.document
            .get_or_init(|| serde_json::from_str(&self.text).ok())
            .as_ref()
    }

    fn filtered(&self, filters: &Arc<[Filter]>) -> Result<Utf8Bytes> {
        if filters.is_empty() {
            return Ok(self.text.clone());
        }
        let mut cache = self
            .projections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(text) = cache.get(filters) {
            return Ok(text.clone());
        }
        let document = self.document().context("invalid backend snapshot")?;
        let mut bytes = Vec::new();
        project(&mut bytes, Property::Json(document), filters)?;
        let text: Utf8Bytes = String::from_utf8(bytes)?.into();
        cache.insert(filters.clone(), text.clone());
        Ok(text)
    }
}

struct Shared {
    // Each receiver retains only the newest complete snapshot, not a frame queue.
    frames: watch::Sender<Option<Arc<Snapshot>>>,
    demand: Notify,
}

struct Attach {
    stream: TcpStream,
    prefetched: Vec<u8>,
    target: String,
    endpoint: Endpoint,
}

pub(crate) struct Broker {
    shared: [Arc<Shared>; 4],
    clients: mpsc::Sender<Attach>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Broker {
    pub(crate) fn new(upstream: Url) -> Result<Self> {
        ensure!(
            matches!(upstream.scheme(), "http" | "https"),
            "unsupported backend URL scheme"
        );
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let shared = std::array::from_fn(|_| {
            Arc::new(Shared {
                frames: watch::channel(None).0,
                demand: Notify::new(),
            })
        });
        let (clients, receiver) = mpsc::channel(MAX_CLIENTS);
        let (stop, stopped) = oneshot::channel();
        let state = shared.clone();
        let thread = thread::Builder::new()
            .name("vosulay-backend".into())
            .spawn(move || {
                runtime.block_on(run(upstream, state, receiver, stopped));
            })
            .context("starting backend broker")?;
        Ok(Self {
            shared,
            clients,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub(crate) fn snapshot(&self, target: &str) -> Option<(u16, Utf8Bytes)> {
        let schema = match target.split('?').next()? {
            "/json" => Schema::Osu,
            "/json/sc" => Schema::Sc,
            "/json/v2" => Schema::V2,
            "/json/v2/precise" => Schema::V2Precise,
            _ => return None,
        };
        let latest = self.shared[schema.index()].frames.borrow();
        Some((200, latest.as_ref()?.text.clone()))
    }

    pub(crate) fn attach(
        &self,
        stream: TcpStream,
        prefetched: Vec<u8>,
        target: &str,
    ) -> Result<()> {
        let endpoint = endpoint(target).context("unsupported websocket target")?;
        stream.set_nonblocking(true)?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        stream.set_nodelay(true)?;
        self.clients
            .try_send(Attach {
                stream,
                prefetched,
                target: target.into(),
                endpoint,
            })
            .map_err(|_| anyhow!("backend broker is busy or stopped"))
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_MESSAGE + 4096)
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}

async fn run(
    base: Url,
    shared: [Arc<Shared>; 4],
    mut incoming: mpsc::Receiver<Attach>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut sources = JoinSet::new();
    for schema in [Schema::Osu, Schema::Sc, Schema::V2, Schema::V2Precise] {
        sources.spawn(source(base.clone(), schema, shared[schema.index()].clone()));
    }
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            Some(attach) = incoming.recv() => {
                if clients.len() >= MAX_CLIENTS { continue; }
                let state = match attach.endpoint {
                    Endpoint::Telemetry(schema) => Some(shared[schema.index()].clone()),
                    Endpoint::Relay(_) => None,
                };
                // Subscribe before waking the producer, including before the client task is polled.
                let frames = state.as_ref().map(|state| state.frames.subscribe());
                if let Some(state) = state { state.demand.notify_one(); }
                clients.spawn(client(attach, base.clone(), frames));
            },
            Some(result) = clients.join_next(), if !clients.is_empty() => {
                if let Ok(Err(error)) = result { eprintln!("Overlay backend client disconnected: {error}"); }
            }
        }
    }
    clients.abort_all();
    sources.abort_all();
    while clients.join_next().await.is_some() {}
    while sources.join_next().await.is_some() {}
}

fn upstream_url(base: &Url, path: &str, query: Option<&str>) -> Url {
    let mut url = base.clone();
    url.set_scheme(if base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    })
    .expect("HTTP backend scheme");
    url.set_path(path);
    url.set_query(query);
    url.set_fragment(None);
    url
}

async fn connect(url: &Url) -> Result<Upstream> {
    Ok(timeout(
        IO_TIMEOUT,
        connect_async_with_config(url.as_str(), Some(socket_config()), true),
    )
    .await??
    .0)
}

async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    message: Message,
) -> Result<()> {
    timeout(IO_TIMEOUT, socket.send(message)).await??;
    Ok(())
}

async fn source(base: Url, schema: Schema, shared: Arc<Shared>) {
    let url = upstream_url(&base, schema.path(), None);
    loop {
        while shared.frames.receiver_count() == 0 {
            shared.demand.notified().await;
        }
        let connection = tokio::select! {
            _ = shared.frames.closed() => continue,
            result = connect(&url) => result,
        };
        if let Ok(mut socket) = connection {
            loop {
                let received = tokio::select! {
                    _ = shared.frames.closed() => { let _ = timeout(IO_TIMEOUT, socket.close(None)).await; break; },
                    received = socket.next() => received,
                };
                match received {
                    Some(Ok(Message::Text(text))) => {
                        shared.frames.send_replace(Some(Arc::new(Snapshot {
                            text,
                            document: OnceLock::new(),
                            projections: Mutex::new(HashMap::new()),
                        })));
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if send(&mut socket, Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
        // Never serve a stale snapshot after backend loss or after the last subscriber leaves.
        shared.frames.send_replace(None);
        if shared.frames.receiver_count() != 0 {
            tokio::select! { _ = shared.frames.closed() => {}, _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
        }
    }
}

async fn client(
    attach: Attach,
    base: Url,
    frames: Option<watch::Receiver<Option<Arc<Snapshot>>>>,
) -> Result<()> {
    let stream = tokio::net::TcpStream::from_std(attach.stream)?;
    let mut socket = WebSocketStream::from_partially_read(
        stream,
        attach.prefetched,
        Role::Server,
        Some(socket_config()),
    )
    .await;
    let query = attach.target.split_once('?').map(|(_, query)| query);
    if let Endpoint::Relay(path) = attach.endpoint {
        return relay(
            &mut socket,
            connect(&upstream_url(&base, path, query)).await?,
        )
        .await;
    }
    let commands_url = upstream_url(&base, COMMANDS, query);
    let mut frames = frames.expect("telemetry receiver");
    let sc = matches!(attach.endpoint, Endpoint::Telemetry(Schema::Sc));
    let mut filters: Arc<[Filter]> = Arc::from([]);
    let mut commands: Option<Upstream> = None;
    let mut ready = true;
    loop {
        if ready {
            ready = false;
            let frame = frames.borrow_and_update().clone();
            if let Some(frame) = frame {
                send(&mut socket, Message::Text(frame.filtered(&filters)?)).await?;
            }
        }
        tokio::select! {
            result = frames.changed() => { result?; ready = true; },
            received = socket.next() => {
                match received {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(source) = filter_command(&text, sc) {
                            if let Some(value) = parse_filters(source) { filters = value.into(); ready = true; }
                        } else if text.contains(':') {
                            if commands.is_none() { commands = Some(connect(&commands_url).await?); }
                            send(commands.as_mut().expect("command channel"), Message::Text(text)).await?;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => send(&mut socket, Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => { let _ = socket.close(None).await; return Ok(()); },
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(Message::Binary(_))) => {
                        send(&mut socket, Message::Close(Some(CloseFrame { code: 1003.into(), reason: "".into() }))).await?;
                        return Ok(());
                    }
                    _ => {}
                }
            },
            received = async { match commands.as_mut() { Some(socket) => socket.next().await, None => std::future::pending().await } } => {
                match received {
                    Some(Ok(Message::Text(text))) => send(&mut socket, Message::Text(text)).await?,
                    Some(Ok(Message::Ping(payload))) => send(commands.as_mut().expect("command channel"), Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => commands = None,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {}
                }
            }
        }
    }
}

async fn relay(
    socket: &mut WebSocketStream<tokio::net::TcpStream>,
    mut upstream: Upstream,
) -> Result<()> {
    loop {
        tokio::select! {
            received = socket.next() => match received {
                Some(Ok(message)) => { let closed = message.is_close(); send(&mut upstream, message).await?; if closed { return Ok(()); } },
                Some(Err(error)) => return Err(error.into()),
                None => return Ok(()),
            },
            received = upstream.next() => match received {
                Some(Ok(message)) => { let closed = message.is_close(); send(socket, message).await?; if closed { return Ok(()); } },
                Some(Err(error)) => return Err(error.into()),
                None => return Ok(()),
            }
        }
    }
}

fn array_index(key: &str) -> Option<usize> {
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    key.parse().ok()
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Filter {
    field: String,
    keys: Option<Vec<Filter>>,
    keep_null: bool,
}

fn filter_command(text: &str, sc: bool) -> Option<&str> {
    if let Some(legacy) = text.strip_prefix("applyFilters:") {
        let payload = legacy.split_once(':').map(|(_, payload)| payload);
        Some(
            payload
                .filter(|value| value.starts_with('['))
                .unwrap_or(legacy),
        )
    } else if sc && !text.contains(':') && text.trim_start().starts_with('[') {
        Some(text)
    } else {
        None
    }
}

fn parse_filters(text: &str) -> Option<Vec<Filter>> {
    fn parse(values: &[Value], depth: usize, nodes: &mut usize) -> Option<Vec<Filter>> {
        if depth > 8 {
            return None;
        }
        let mut filters: Vec<Filter> = Vec::new();
        for value in values {
            *nodes += 1;
            if *nodes > 256 {
                return None;
            }
            let mut filter = if let Some(field) = value.as_str() {
                Filter {
                    field: field.into(),
                    keys: None,
                    keep_null: true,
                }
            } else if let Some(field) = value.get("field").and_then(field_key) {
                let Some(keys) = value.get("keys").and_then(Value::as_array) else {
                    continue;
                };
                Filter {
                    field,
                    keys: Some(parse(keys, depth + 1, nodes)?),
                    keep_null: false,
                }
            } else {
                continue;
            };
            if let Some(old) = filters.iter_mut().find(|old| old.field == filter.field) {
                // Nested filters skip null rather than erasing an earlier whole-field value.
                if filter.keys.is_some() {
                    filter.keep_null |= old.keep_null;
                }
                *old = filter;
            } else {
                filters.push(filter);
            }
        }
        Some(filters)
    }
    let values: Value = serde_json::from_str(text).ok()?;
    parse(values.as_array()?, 0, &mut 0)
}

fn field_key(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Number(value) if value.as_f64() == Some(0.0) => None,
        Value::String(value) if value.is_empty() => None,
        _ => Some(js_string(value)),
    }
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.as_f64().expect("JSON number").to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

#[derive(Clone, Copy)]
enum Property<'a> {
    Json(&'a Value),
    Length(usize),
    Unit(u16),
}

impl<'a> Property<'a> {
    fn get(self, key: &str) -> Option<Self> {
        match self {
            Self::Json(Value::Object(object)) => object.get(key).map(Self::Json),
            Self::Json(Value::Array(array)) if key == "length" => Some(Self::Length(array.len())),
            Self::Json(Value::Array(array)) => array.get(array_index(key)?).map(Self::Json),
            Self::Json(Value::String(text)) if key == "length" => {
                Some(Self::Length(text.encode_utf16().count()))
            }
            Self::Json(Value::String(text)) => {
                text.encode_utf16().nth(array_index(key)?).map(Self::Unit)
            }
            Self::Unit(_) if key == "length" => Some(Self::Length(1)),
            Self::Unit(_) if key == "0" => Some(self),
            _ => None,
        }
    }
}

fn project(bytes: &mut Vec<u8>, source: Property<'_>, filters: &[Filter]) -> Result<()> {
    bytes.push(b'{');
    let mut first = true;
    for filter in filters {
        let Some(value) = source.get(&filter.field) else {
            continue;
        };
        let null = matches!(value, Property::Json(Value::Null));
        if null && !filter.keep_null {
            continue;
        }
        if !first {
            bytes.push(b',');
        }
        first = false;
        serde_json::to_writer(&mut *bytes, &filter.field)?;
        bytes.push(b':');
        if let Some(keys) = &filter.keys
            && !null
        {
            project(bytes, value, keys)?;
        } else {
            match value {
                Property::Json(value) => serde_json::to_writer(&mut *bytes, value)?,
                Property::Length(length) => serde_json::to_writer(&mut *bytes, &length)?,
                Property::Unit(unit) => write!(bytes, "\"\\u{unit:04x}\"")?,
            }
        }
    }
    bytes.push(b'}');
    Ok(())
}

#[cfg(test)]
mod tests;
