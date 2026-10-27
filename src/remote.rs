use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_tungstenite::tungstenite::{Utf8Bytes, handshake::server::create_response, http};
use url::Url;

use crate::{
    backend::{Broker, supports_websocket},
    config::{AppConfig, AppSettings, Placement, Profile},
};

mod manage;
mod tool;
pub use tool::ToolServer;

pub use manage::ManageEditor;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REQUEST: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
type ConfigLoader = dyn Fn() -> Result<AppConfig> + Send + Sync;

pub struct RemoteServer {
    stop: Arc<AtomicBool>,
    threads: Vec<(SocketAddr, JoinHandle<()>)>,
    browser_url: Url,
    browser_secret: String,
    preview_url: Option<Url>,
}

struct Backend {
    upstream: Url,
    broker: Broker,
    /// Per-run secret that only the in-game browser receives, never over HTTP. Command sockets
    /// presenting it may use vosu's broker token (see `broker_upgrade_target`).
    secret: String,
    http: ureq::Agent,
    editor: Option<Arc<dyn ManageEditor>>,
    tool_mode: bool,
    preview_url: Option<Url>,
}

impl RemoteServer {
    pub fn start(base_url: Url) -> Result<Self> {
        Self::start_from_path(base_url, crate::config::config_path()?, None)
    }

    /// `editor` serves the manage page (Settings → Advanced → Allow editing on this computer).
    pub fn start_from_path(
        base_url: Url,
        config_path: PathBuf,
        editor: Option<Arc<dyn ManageEditor>>,
    ) -> Result<Self> {
        let config = crate::config::load_from_path(&config_path)
            .context("failed to load configuration for overlay server")?;
        // Browser routing must work even when the optional LAN port is occupied.
        let local = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("failed to bind local backend broker")?;
        let address = listen_address(&config.settings);
        let mut listeners = vec![local];
        if crate::tool::Environment::from_env()?.is_none() {
            match TcpListener::bind(address) {
                Ok(listener) => listeners.push(listener),
                Err(error) => {
                    eprintln!("Remote overlay hosting at {address} is unavailable: {error}")
                }
            }
        }
        let credential = crate::backend::BrokerCredential::for_backend(&base_url);
        Self::start_with_listeners(
            listeners,
            base_url,
            Arc::new(move || crate::config::load_from_path(&config_path)),
            editor,
            credential,
        )
    }

    pub fn browser_url(&self) -> &Url {
        &self.browser_url
    }

    /// A browser-preview capability, disclosed only through authenticated manager routes.
    pub fn preview_url(&self) -> &Url {
        self.preview_url.as_ref().unwrap_or(&self.browser_url)
    }

    /// The in-game browser's broker secret, handed to its counter frames out of band.
    pub fn browser_secret(&self) -> &str {
        &self.browser_secret
    }

    #[cfg(test)]
    fn start_with_listener(
        listener: TcpListener,
        base_url: Url,
        loader: Arc<ConfigLoader>,
    ) -> Result<Self> {
        Self::start_with_listeners(vec![listener], base_url, loader, None, None)
    }

    fn start_with_listeners(
        listeners: Vec<TcpListener>,
        base_url: Url,
        loader: Arc<ConfigLoader>,
        editor: Option<Arc<dyn ManageEditor>>,
        credential: Option<crate::backend::BrokerCredential>,
    ) -> Result<Self> {
        let browser_url = Url::parse(&format!("http://{}/", listeners[0].local_addr()?))?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).context("could not generate the broker secret")?;
        let browser_secret: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let tool_mode = crate::tool::Environment::from_env()?.is_some();
        let preview_url = if tool_mode {
            getrandom::fill(&mut bytes).context("could not generate preview authority")?;
            let identity: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            // Two labels keep each DNS label below its 63-octet limit.
            Some(Url::parse(&format!(
                "http://{}.{}.preview.localhost:{}/",
                &identity[..32],
                &identity[32..],
                browser_url.port().expect("broker port")
            ))?)
        } else {
            None
        };
        let backend = Arc::new(Backend {
            broker: Broker::new(base_url.clone(), credential)?,
            secret: browser_secret.clone(),
            upstream: base_url,
            http: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .max_redirects(0)
                    .max_redirects_will_error(false)
                    .timeout_global(Some(TIMEOUT))
                    .build(),
            ),
            editor,
            tool_mode,
            preview_url: preview_url.clone(),
        });
        let mut server = Self {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::with_capacity(listeners.len()),
            browser_url,
            browser_secret,
            preview_url,
        };
        let active = Arc::new(AtomicUsize::new(0));
        for listener in listeners {
            listener.set_nonblocking(false)?;
            let mut wake_address = listener.local_addr()?;
            if wake_address.ip().is_unspecified() {
                wake_address.set_ip(if wake_address.is_ipv4() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                });
            }
            let stop = server.stop.clone();
            let backend = backend.clone();
            let loader = loader.clone();
            let active = active.clone();
            server.threads.push((
                wake_address,
                thread::Builder::new()
                    .name("vosulay-overlay-listener".to_owned())
                    .spawn(move || serve(listener, backend, loader, stop, active))?,
            ));
        }
        Ok(server)
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake blocking accepts locally, including wildcard-bound LAN listeners.
        for (address, _) in &self.threads {
            let _ = TcpStream::connect_timeout(address, TIMEOUT);
        }
        for (_, thread) in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn listen_address(settings: &AppSettings) -> SocketAddr {
    let host = settings
        .remote_listen_host
        .parse()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(host, settings.remote_listen_port)
}

fn serve(
    listener: TcpListener,
    backend: Arc<Backend>,
    loader: Arc<ConfigLoader>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                    let _ = stream.set_write_timeout(Some(TIMEOUT));
                    let _ = Response::plain(503, "Service Unavailable", "Server is busy\n")
                        .write(&mut stream, true);
                    continue;
                }
                active.fetch_add(1, Ordering::AcqRel);
                let backend = Arc::clone(&backend);
                let loader = Arc::clone(&loader);
                let request_active = Arc::clone(&active);
                if let Err(error) = thread::Builder::new()
                    .name("vosulay-remote-request".to_owned())
                    .spawn(move || {
                        let _active_request = ActiveRequest(request_active);
                        if let Err(error) = handle(&mut stream, &backend, &loader) {
                            eprintln!("Remote overlay request failed: {error}");
                        }
                    })
                {
                    active.fetch_sub(1, Ordering::AcqRel);
                    eprintln!("Could not start remote overlay request worker: {error}");
                }
            }
            Err(error) => {
                eprintln!("Remote overlay listener stopped accepting requests: {error}");
                break;
            }
        }
    }
    while active.load(Ordering::Acquire) != 0 {
        thread::sleep(Duration::from_millis(10));
    }
}

struct ActiveRequest(Arc<AtomicUsize>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle(stream: &mut TcpStream, backend: &Backend, loader: &Arc<ConfigLoader>) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => {
            return Response::plain(error.status, error.reason, &error.message)
                .write(stream, error.suppress_body)
                .map_err(Into::into);
        }
    };
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |parts| parts.0);
    let route = path.strip_prefix('/').expect("validated request target");
    let local_address = stream.local_addr()?;
    let manage = manage::is_manage_host(request.host.as_deref(), local_address);
    let post_allowed = request.method == "POST" && manage && route == "__vosulay/manage/message";
    if request.method != "GET" && request.method != "HEAD" && !post_allowed {
        let mut response = Response::plain(405, "Method Not Allowed", "Use GET or HEAD\n");
        response
            .headers
            .push(("Allow".into(), "GET, HEAD".to_owned()));
        return response.write(stream, false).map_err(Into::into);
    }
    let peer = stream.peer_addr()?;
    let config = match loader() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Could not load peer authorization settings: {error:#}");
            return Response::plain(
                500,
                "Internal Server Error",
                "Could not load configuration\n",
            )
            .write(stream, request.method == "HEAD")
            .map_err(Into::into);
        }
    };
    let preview = backend
        .preview_url
        .as_ref()
        .filter(|url| preview_authority_is_allowed(&request, url, peer.ip(), local_address));
    let resource = preview_resource_route(route);
    let metadata_allowed = if let Some(preview) = preview {
        resource && origin_metadata_is_allowed(&request, preview, true)
    } else {
        let allow_opaque = if backend.tool_mode {
            resource && request.is_frame_navigation() && request.origin.as_deref() != Some("null")
        } else {
            route.starts_with("local/") || request.is_frame_navigation()
        };
        request_metadata_is_allowed(&request, local_address, allow_opaque)
    };
    if !metadata_allowed || !peer_is_allowed(peer.ip(), &config.settings.allowed_ips) {
        return Response::plain(403, "Forbidden", "Request is not authorized\n")
            .write(stream, request.method == "HEAD")
            .map_err(Into::into);
    }
    if manage {
        // Only the editor lives on the manage origin: no backend proxy, local files or sockets.
        let response = if request.header("upgrade").is_some() {
            Response::plain(404, "Not Found", "WebSocket route not found\n")
        } else {
            manage::respond(&request, route, &config, backend, peer, local_address)
                .unwrap_or_else(|error| Response::plain(error.status, error.reason, &error.message))
        };
        return response
            .write(stream, request.method == "HEAD")
            .map_err(Into::into);
    }
    if request.header("upgrade").is_some() {
        if request.method != "GET" || !supports_websocket(&request.target) {
            return Response::plain(404, "Not Found", "WebSocket route not found\n")
                .write(stream, request.method == "HEAD")
                .map_err(Into::into);
        }
        let (target, credentialed) =
            broker_upgrade_target(&request, peer.ip(), local_address, &backend.secret);
        return upgrade(stream, backend, &request, &target, credentialed);
    }
    let serving_url = Url::parse(&format!(
        "http://{}/",
        request.host.as_deref().expect("authorized authority")
    ))?;
    let result = match route {
        "overlay" => overlay_response(&serving_url, &backend.upstream, &config, &request.target),
        "__vosulay/view-state" => Ok(view_state(&config)),
        route if route.starts_with("local/") && preview.is_some() => {
            preview_local_response(&config, route, &backend.upstream, &serving_url)
        }
        route if route.starts_with("local/") => local_response(&config, route),
        "__vosulay/backend.js" => {
            let mut response = Response::bytes(
                200,
                "OK",
                "text/javascript; charset=utf-8",
                backend_script(&backend.upstream, &serving_url).into_bytes(),
            );
            response
                .headers
                .push(("Cache-Control".into(), "no-store".into()));
            Ok(response)
        }
        route if route.starts_with("__vosulay/") => {
            Err(HttpError::new(404, "Not Found", "Route not found\n"))
        }
        _ => proxy_response(backend, &serving_url, &request),
    };
    let mut response = match result {
        Ok(response) => response,
        Err(error) => Response::plain(error.status, error.reason, &error.message),
    };
    if preview.is_some() {
        opaque_counter_resource(&mut response);
    }
    response
        .write(stream, request.method == "HEAD")
        .map_err(Into::into)
}

fn preview_resource_route(route: &str) -> bool {
    !matches!(route, "overlay" | "manage" | "config" | "config.json")
        && !route.starts_with("__vosulay/")
        && !route.starts_with("v1/")
        && !route.starts_with("tools/")
}

fn preview_authority_is_allowed(
    request: &Request,
    preview: &Url,
    peer: IpAddr,
    local: SocketAddr,
) -> bool {
    normalize_ip(peer).is_loopback()
        && normalize_ip(local.ip()).is_loopback()
        && request.host.as_deref().is_some_and(|host| {
            tool::token_matches(
                host.as_bytes(),
                preview[url::Position::BeforeHost..url::Position::AfterPort].as_bytes(),
            )
        })
}
fn backend_script(upstream: &Url, broker: &Url) -> String {
    let arguments = serde_json::to_string(&[upstream.as_str(), broker.as_str()])
        .expect("URL strings serialize");
    format!(
        "({})(...{});",
        include_str!("../assets/backend.js"),
        arguments
    )
}

fn upgrade(
    stream: &mut TcpStream,
    backend: &Backend,
    request: &Request,
    target: &str,
    credentialed: bool,
) -> Result<()> {
    let mut handshake = http::Request::builder()
        .method(request.method.as_str())
        .uri(&request.target)
        .version(request.version);
    for (name, value) in &request.headers {
        handshake = handshake.header(name, value);
    }
    let handshake = handshake.body(())?;
    let response = match create_response(&handshake) {
        Ok(response) => response,
        Err(_) => {
            return Response::plain(400, "Bad Request", "Invalid WebSocket handshake\n")
                .write(stream, false)
                .map_err(Into::into);
        }
    };
    write!(stream, "HTTP/1.1 101 Switching Protocols\r\n")?;
    for (name, value) in response.headers() {
        write!(stream, "{name}: {}\r\n", value.to_str()?)?;
    }
    stream.write_all(b"\r\n")?;
    stream.flush()?;
    backend.broker.attach(
        stream.try_clone()?,
        request.prefetched.clone(),
        target,
        credentialed,
    )
}

fn hop_header(name: &str, connection: Option<&str>) -> bool {
    [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ]
    .iter()
    .any(|header| name.eq_ignore_ascii_case(header))
        || connection.is_some_and(|value| {
            value
                .split(',')
                .any(|header| name.eq_ignore_ascii_case(header.trim()))
        })
}

fn proxy_response(
    backend: &Backend,
    serving_url: &Url,
    request: &Request,
) -> std::result::Result<Response, HttpError> {
    if let Some((status, text)) = backend.broker.snapshot(&request.target) {
        return Ok(Response {
            status,
            reason: "OK",
            mime: "application/json".into(),
            headers: vec![("Cache-Control".into(), "no-store".into())],
            body: Body::Shared(text),
        });
    }
    let (path, query) = request
        .target
        .split_once('?')
        .map_or((request.target.as_str(), None), |(path, query)| {
            (path, Some(query))
        });
    let mut target = backend.upstream.clone();
    target.set_path(path);
    target.set_query(query);
    target.set_fragment(None);
    let mut outgoing = http::Request::builder()
        .method(request.method.as_str())
        .uri(target.as_str());
    for (name, value) in &request.headers {
        if hop_header(name, request.header("connection"))
            || [
                "host",
                "content-length",
                "accept-encoding",
                "origin",
                "referer",
            ]
            .iter()
            .any(|header| name.eq_ignore_ascii_case(header))
            || name.to_ascii_lowercase().starts_with("sec-fetch-")
        {
            continue;
        }
        outgoing = outgoing.header(name, value);
    }
    if request.origin.is_some() {
        outgoing = outgoing.header("Origin", backend.upstream.origin().ascii_serialization());
    }
    let outgoing = outgoing
        .body(())
        .map_err(|_| HttpError::new(400, "Bad Request", "Invalid backend request\n"))?;
    let upstream = backend.http.run(outgoing).map_err(|error| {
        HttpError::new(
            502,
            "Bad Gateway",
            format!("Backend request failed: {error}\n"),
        )
    })?;
    let status = upstream.status();
    let mime = upstream
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let length = upstream
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let connection = upstream
        .headers()
        .get("connection")
        .and_then(|value| value.to_str().ok());
    let mut headers = Vec::new();
    for (name, value) in upstream.headers() {
        if hop_header(name.as_str(), connection)
            || matches!(name.as_str(), "content-type" | "content-length")
        {
            continue;
        }
        let Ok(value) = value.to_str() else {
            continue;
        };
        let value = if name == http::header::LOCATION {
            match Url::parse(value) {
                Ok(url) => routed_backend_url(&url, serving_url, &backend.upstream)?.into(),
                Err(_) => value.to_owned(),
            }
        } else {
            value.to_owned()
        };
        headers.push((name.to_string(), value));
    }
    let (_, mut body) = upstream.into_parts();
    let body = if request.method == "GET"
        && status == http::StatusCode::OK
        && mime
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
    {
        let mut html = body
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_vec()
            .map_err(|_| HttpError::new(502, "Bad Gateway", "Could not read backend HTML\n"))?;
        let bootstrap = if backend.preview_url.as_ref() == Some(serving_url) {
            format!(
                "<script>{}</script>",
                backend_script(&backend.upstream, serving_url)
            )
        } else {
            format!(
                "<script src=\"{}__vosulay/backend.js\"></script>",
                escape_html(serving_url.as_str())
            )
        };
        insert_bootstrap(&mut html, &bootstrap);
        headers.retain(|(name, _)| {
            !matches!(name.as_str(), "etag" | "last-modified" | "cache-control")
        });
        headers.push(("Cache-Control".into(), "no-store".into()));
        Body::Bytes(html)
    } else {
        Body::Proxy(body.into_reader(), length)
    };
    Ok(Response {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("Upstream Response"),
        mime,
        headers,
        body,
    })
}

fn routed_backend_url(
    source: &Url,
    broker: &Url,
    upstream: &Url,
) -> std::result::Result<Url, HttpError> {
    let loopback = |url: &Url| match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    let aliases = source.scheme() == upstream.scheme()
        && source.port_or_known_default() == upstream.port_or_known_default()
        && loopback(source)
        && loopback(upstream);
    let mut routed = source.clone();
    if source.origin() == upstream.origin() || aliases {
        routed.set_scheme(broker.scheme()).map_err(|()| {
            HttpError::new(500, "Internal Server Error", "Invalid broker scheme\n")
        })?;
        routed
            .set_host(broker.host_str())
            .map_err(|_| HttpError::new(500, "Internal Server Error", "Invalid broker host\n"))?;
        routed
            .set_port(broker.port())
            .map_err(|()| HttpError::new(500, "Internal Server Error", "Invalid broker port\n"))?;
    }
    Ok(routed)
}

// Tool counter frames deliberately have opaque origins. Their scripts, styles,
// modules and fonts are public overlay resources, not manager capabilities.
fn opaque_counter_resource(response: &mut Response) {
    response.headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("access-control-allow-origin")
            && !name.eq_ignore_ascii_case("access-control-allow-credentials")
            && !name.eq_ignore_ascii_case("cross-origin-resource-policy")
            && !name.eq_ignore_ascii_case("referrer-policy")
    });
    response.headers.extend([
        ("Access-Control-Allow-Origin".into(), "null".into()),
        ("Cross-Origin-Resource-Policy".into(), "cross-origin".into()),
        ("Referrer-Policy".into(), "no-referrer".into()),
    ]);
}

fn request_metadata_is_allowed(
    request: &Request,
    local_address: SocketAddr,
    allow_opaque_origin: bool,
) -> bool {
    let Some(authority) = request
        .host
        .as_deref()
        .and_then(|host| request_authority(host, local_address))
    else {
        return false;
    };
    origin_metadata_is_allowed(request, &authority, allow_opaque_origin)
}

fn origin_metadata_is_allowed(
    request: &Request,
    authority: &Url,
    allow_opaque_origin: bool,
) -> bool {
    let fetch_site_allowed = request.fetch_site.as_deref().is_none_or(|site| {
        matches!(site, "none" | "same-origin" | "same-site")
            || (allow_opaque_origin && site == "cross-site")
    });
    if !fetch_site_allowed {
        return false;
    }
    request.origin.as_deref().is_none_or(|origin| {
        (allow_opaque_origin && origin == "null")
            || Url::parse(origin).is_ok_and(|url| {
                url.scheme() == "http"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.origin() == authority.origin()
            })
    })
}

/// Query parameter in which in-game counter frames present the broker secret.
const BROKER_SECRET_PARAMETER: &str = "__vosulay_broker";

/// The upgrade target to forward, without any broker secret parameter, and whether vosu's broker
/// token may be presented for it. That needs all of:
/// - the per-run secret, which only the in-game browser receives (through CEF's renderer extra
///   info, never over HTTP), so a program that merely reaches this port cannot supply it;
/// - a loopback peer, so LAN viewers never qualify;
/// - `Origin` exactly this listener's origin and `Sec-Fetch-Site` absent or `same-origin`.
///   Browsers always send `Origin` on WebSocket upgrades and pages cannot set it, so another
///   site's page holding a leaked secret still cannot use it.
///
/// Counter frames can read the secret from their own socket URLs. They are trusted code under
/// vosu ADR 0013 and could already save through that socket, so it gives them nothing new.
fn broker_upgrade_target(
    request: &Request,
    peer: IpAddr,
    local_address: SocketAddr,
    secret: &str,
) -> (String, bool) {
    let (target, presented) = take_broker_secret(&request.target, secret);
    let Some(authority) = request
        .host
        .as_deref()
        .and_then(|host| request_authority(host, local_address))
    else {
        return (target, false);
    };
    let credentialed = presented
        && normalize_ip(peer).is_loopback()
        && request
            .fetch_site
            .as_deref()
            .is_none_or(|site| site == "same-origin")
        && request
            .origin
            .as_deref()
            .and_then(|origin| Url::parse(origin).ok())
            .is_some_and(|origin| {
                origin.scheme() == "http"
                    && origin.path() == "/"
                    && origin.origin() == authority.origin()
            });
    (target, credentialed)
}

/// Removes every broker secret parameter, keeping the rest of the query byte for byte: vosu
/// routes settings by the raw `l` value. Valid only when exactly one matching secret was present.
fn take_broker_secret(target: &str, secret: &str) -> (String, bool) {
    let Some((path, query)) = target.split_once('?') else {
        return (target.to_owned(), false);
    };
    let mut presented = 0;
    let mut valid = false;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let Some(rest) = pair.strip_prefix(BROKER_SECRET_PARAMETER) else {
                return true;
            };
            let Some(value) = rest.strip_prefix('=').or(rest.is_empty().then_some("")) else {
                return true;
            };
            presented += 1;
            valid = value.len() == secret.len()
                && value
                    .bytes()
                    .zip(secret.bytes())
                    .fold(0, |difference, (a, b)| difference | (a ^ b))
                    == 0;
            false
        })
        .collect();
    let target = if kept.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", kept.join("&"))
    };
    (target, presented == 1 && valid)
}

fn request_authority(host: &str, local_address: SocketAddr) -> Option<Url> {
    let url = Url::parse(&format!("http://{host}/")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() != Some(local_address.port())
    {
        return None;
    }
    let local_ip = normalize_ip(local_address.ip());
    let matches = match url.host()? {
        url::Host::Domain(domain) => {
            (domain.eq_ignore_ascii_case("localhost") || domain.eq_ignore_ascii_case(manage::HOST))
                && local_ip.is_loopback()
        }
        url::Host::Ipv4(address) => normalize_ip(IpAddr::V4(address)) == local_ip,
        url::Host::Ipv6(address) => normalize_ip(IpAddr::V6(address)) == local_ip,
    };
    matches.then_some(url)
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
    origin: Option<String>,
    fetch_site: Option<String>,
    version: http::Version,
    headers: Vec<(String, String)>,
    /// Only the manage page's message POSTs may have a body.
    body: Vec<u8>,
    prefetched: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn is_frame_navigation(&self) -> bool {
        self.header("sec-fetch-mode") == Some("navigate")
            && self.header("sec-fetch-dest") == Some("iframe")
            && self.origin.is_none()
    }
}

fn read_request(stream: &mut TcpStream) -> std::result::Result<Request, HttpError> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    let end = loop {
        if bytes.len() >= MAX_REQUEST {
            return Err(request_error(
                &bytes,
                431,
                "Request Header Fields Too Large",
                "Request headers are too large\n",
            ));
        }
        let remaining = (MAX_REQUEST - bytes.len()).min(buffer.len());
        let count = stream.read(&mut buffer[..remaining]).map_err(|_| {
            request_error(
                &bytes,
                408,
                "Request Timeout",
                "Request was not completed\n",
            )
        })?;
        if count == 0 {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Incomplete request\n",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 2;
        }
    };
    let source = std::str::from_utf8(&bytes[..end])
        .map_err(|_| request_error(&bytes, 400, "Bad Request", "Invalid headers\n"))?;
    let mut lines = source.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split_ascii_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if method.is_empty()
        || target.is_empty()
        || parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !target.starts_with('/')
        || target.starts_with("//")
        || target.contains('#')
    {
        return Err(request_error(
            &bytes,
            400,
            "Bad Request",
            "Invalid request line\n",
        ));
    }
    let mut host = None;
    let mut origin = None;
    let mut fetch_site = None;
    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Invalid header\n",
            ));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.contains(['\r', '\n', '\0'])
        {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Invalid header\n",
            ));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Request bodies are not supported\n",
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let declared = value.trim();
            let length = (!declared.is_empty()
                && declared.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| declared.parse::<usize>().ok())
            .flatten();
            let Some(length) = length.filter(|_| content_length.is_none()) else {
                return Err(request_error(
                    &bytes,
                    400,
                    "Bad Request",
                    "Invalid Content-Length\n",
                ));
            };
            if length != 0 && method != "POST" {
                return Err(request_error(
                    &bytes,
                    400,
                    "Bad Request",
                    "Request bodies are not supported\n",
                ));
            }
            if length > manage::MAX_BODY {
                return Err(request_error(
                    &bytes,
                    413,
                    "Content Too Large",
                    "Request body is too large\n",
                ));
            }
            content_length = Some(length);
        }
        let target = if name.eq_ignore_ascii_case("host") {
            Some((&mut host, "Invalid Host header\n"))
        } else if name.eq_ignore_ascii_case("origin") {
            Some((&mut origin, "Invalid Origin header\n"))
        } else if name.eq_ignore_ascii_case("sec-fetch-site") {
            Some((&mut fetch_site, "Invalid Sec-Fetch-Site header\n"))
        } else {
            None
        };
        if let Some((slot, message)) = target {
            let value = value.trim();
            if value.is_empty() || slot.is_some() {
                return Err(request_error(&bytes, 400, "Bad Request", message));
            }
            *slot = Some(value.to_owned());
        }
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    let mut prefetched = bytes[end + 2..].to_vec();
    let content_length = content_length.unwrap_or(0);
    let body = if content_length == 0 {
        Vec::new()
    } else {
        // Read the whole body before routing: answering early with unread request
        // bytes can make the client see a connection reset instead of the response.
        if prefetched.len() > content_length {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Request body is longer than declared\n",
            ));
        }
        let missing = (content_length - prefetched.len()) as u64;
        Read::by_ref(stream)
            .take(missing)
            .read_to_end(&mut prefetched)
            .map_err(|_| {
                request_error(
                    &bytes,
                    408,
                    "Request Timeout",
                    "Request body was not completed\n",
                )
            })?;
        if prefetched.len() != content_length {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Incomplete request body\n",
            ));
        }
        std::mem::take(&mut prefetched)
    };
    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        host,
        origin,
        fetch_site,
        version: if version == "HTTP/1.1" {
            http::Version::HTTP_11
        } else {
            http::Version::HTTP_10
        },
        headers,
        body,
        prefetched,
    })
}

fn request_error(
    bytes: &[u8],
    status: u16,
    reason: &'static str,
    message: &'static str,
) -> HttpError {
    let mut error = HttpError::new(status, reason, message);
    error.suppress_body = bytes.starts_with(b"HEAD ");
    error
}

fn overlay_response(
    base_url: &Url,
    upstream: &Url,
    config: &AppConfig,
    target: &str,
) -> std::result::Result<Response, HttpError> {
    let selected = parse_profile(target)?;
    let profile = select_profile(config, selected)?;
    let mut html = render_profile(profile, base_url, upstream)?;
    if selected.is_none() {
        let state_url = if target.starts_with("/v1/view") {
            "/tools/vosulay/api/v1/view/state"
        } else {
            "/__vosulay/view-state"
        };
        html.truncate(html.len() - "</body></html>".len());
        use std::fmt::Write as _;
        write!(
            html,
            "<script>({})(...{});</script></body></html>",
            include_str!("../assets/view-follow.js"),
            crate::ui::script_json(&serde_json::json!([profile.id, state_url]))
        )
        .map_err(|_| HttpError::new(500, "Internal Server Error", "Could not render follower\n"))?;
    }
    let mut response = Response::bytes(200, "OK", "text/html; charset=utf-8", html.into_bytes());
    response.headers.extend([
        ("Cache-Control".into(), "no-store".to_owned()),
        ("Referrer-Policy".into(), "no-referrer".to_owned()),
        ("X-Content-Type-Options".into(), "nosniff".to_owned()),
        ("Cross-Origin-Resource-Policy".into(), "same-origin".to_owned()),
        ("Content-Security-Policy".into(), "default-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; frame-src 'self' http: https:; frame-ancestors *".to_owned()),
    ]);
    Ok(response)
}

fn view_state(config: &AppConfig) -> Response {
    let mut response = Response::bytes(
        200,
        "OK",
        "application/json",
        serde_json::json!({"active_profile":config.active_profile})
            .to_string()
            .into_bytes(),
    );
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    response
}

fn parse_profile(target: &str) -> std::result::Result<Option<usize>, HttpError> {
    let Some((_, query)) = target.split_once('?') else {
        return Ok(None);
    };
    if query.is_empty() {
        return Ok(None);
    }
    let bytes = query.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || hex(bytes[index + 1]).is_none()
                || hex(bytes[index + 2]).is_none()
            {
                return Err(HttpError::new(
                    400,
                    "Bad Request",
                    "Malformed profile query\n",
                ));
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    let mut result = None;
    for (name, value) in url::form_urlencoded::parse(bytes) {
        if name != "profile" {
            return Err(HttpError::new(
                400,
                "Bad Request",
                "Unknown profile query parameter\n",
            ));
        }
        if result.is_some() {
            return Err(HttpError::new(
                400,
                "Bad Request",
                "Profile was specified more than once\n",
            ));
        }
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(HttpError::new(
                400,
                "Bad Request",
                "Profile must be a positive integer\n",
            ));
        }
        let number = value
            .parse::<usize>()
            .map_err(|_| HttpError::new(400, "Bad Request", "Profile number is too large\n"))?;
        if number == 0 || number.to_string() != value {
            return Err(HttpError::new(
                400,
                "Bad Request",
                "Profile numbers must use canonical positive integers\n",
            ));
        }
        result = Some(number);
    }
    Ok(result)
}

fn select_profile(
    config: &AppConfig,
    number: Option<usize>,
) -> std::result::Result<&Profile, HttpError> {
    match number {
        Some(number) => config
            .profiles
            .get(number - 1)
            .ok_or_else(|| HttpError::new(404, "Not Found", "Profile number is out of range\n")),
        None => config
            .profiles
            .iter()
            .find(|profile| profile.id == config.active_profile)
            .ok_or_else(|| {
                HttpError::new(
                    500,
                    "Internal Server Error",
                    "Active profile is unavailable\n",
                )
            }),
    }
}

fn render_profile(
    profile: &Profile,
    base_url: &Url,
    upstream: &Url,
) -> std::result::Result<String, HttpError> {
    let title = escape_html(&profile.name);
    let mut html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>html,body{{width:100%;height:100%;margin:0;overflow:hidden;background:transparent}}body{{position:relative}}div{{position:absolute;overflow:hidden;transform-origin:0 0}}iframe{{position:absolute;display:block;border:0}}</style></head><body>"
    );
    for item in &profile.overlays {
        if !item.enabled {
            continue;
        }
        let source = match placement_url(item, base_url, upstream) {
            Ok(source) => escape_html(&source),
            Err(_) if item.url.is_some() => continue,
            Err(error) => return Err(error),
        };
        let frame_title = escape_html(placement_title(item));
        let sandbox = match item.url.as_ref().map(Url::scheme) {
            Some("vosulay-overlay") => " sandbox=\"allow-scripts\"",
            Some("http" | "https") => " sandbox=\"allow-scripts allow-same-origin\"",
            _ => "",
        };
        use std::fmt::Write as _;
        // The wrapper is the visible (cropped) region; the frame keeps its full size behind it.
        write!(html, "<div style=\"left:{}px;top:{}px;width:{}px;height:{}px;transform:scale({}) rotate({}deg);opacity:{};z-index:{}\"><iframe src=\"{source}\" title=\"{frame_title}\" allow=\"autoplay\" loading=\"eager\"{sandbox} referrerpolicy=\"no-referrer\" style=\"left:{}px;top:{}px;width:{}px;height:{}px\"></iframe></div>",
            item.left, item.top,
            item.width - item.crop_left - item.crop_right,
            item.height - item.crop_top - item.crop_bottom,
            item.scale, item.rotation, item.opacity, item.z_index,
            0.0 - item.crop_left, 0.0 - item.crop_top, item.width, item.height)
            .map_err(|_| HttpError::new(500, "Internal Server Error", "Could not render profile\n"))?;
    }
    html.push_str("</body></html>");
    Ok(html)
}

fn placement_title(item: &Placement) -> &str {
    if let Some(name) = item.name.as_deref() {
        return name;
    }
    if let Some(url) = &item.url {
        return url
            .host_str()
            .filter(|host| !host.is_empty())
            .unwrap_or("HTML overlay");
    }
    &item.counter_id
}

fn placement_url(
    item: &Placement,
    base_url: &Url,
    upstream: &Url,
) -> std::result::Result<String, HttpError> {
    let Some(custom) = item.url.as_ref() else {
        let mut url = base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.set_path("/");
        url.path_segments_mut()
            .map_err(|()| HttpError::new(500, "Internal Server Error", "Invalid vosu URL\n"))?
            .push(&item.counter_id)
            .push("");
        if let Some(profile) = &item.vosu_profile {
            url.query_pairs_mut().append_pair("vosuProfile", profile);
        }
        return Ok(url.into());
    };
    match custom.scheme() {
        "http" | "https" => Ok(routed_backend_url(custom, base_url, upstream)?.into()),
        "vosulay-overlay" => {
            let token = custom
                .host_str()
                .filter(|token| is_token(token))
                .ok_or_else(|| {
                    HttpError::new(500, "Internal Server Error", "Invalid local capability\n")
                })?;
            let Some((root_encoded, relative_path)) = custom
                .path()
                .strip_prefix('/')
                .and_then(|path| path.split_once('/'))
            else {
                return Err(HttpError::new(
                    500,
                    "Internal Server Error",
                    "Invalid local path\n",
                ));
            };
            if custom.port().is_some() || root_encoded.is_empty() || relative_path.is_empty() {
                return Err(HttpError::new(
                    500,
                    "Internal Server Error",
                    "Invalid local path\n",
                ));
            }
            let mut result = format!("/local/{token}/{relative_path}");
            if let Some(query) = custom.query() {
                result.push('?');
                result.push_str(query);
            }
            if let Some(fragment) = custom.fragment() {
                result.push('#');
                result.push_str(fragment);
            }
            Ok(result)
        }
        _ => Err(HttpError::new(
            500,
            "Internal Server Error",
            "Unsupported overlay URL\n",
        )),
    }
}

fn escape_html(value: &str) -> String {
    value.chars().fold(
        String::with_capacity(value.len()),
        |mut output, character| {
            output.push_str(match character {
                '&' => "&amp;",
                '"' => "&quot;",
                '<' => "&lt;",
                '>' => "&gt;",
                _ => {
                    output.push(character);
                    return output;
                }
            });
            output
        },
    )
}

fn insert_bootstrap(html: &mut Vec<u8>, bootstrap: &str) {
    // Preserve doctype/BOM and standards mode.
    let start = if html.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let start = html[start..]
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |offset| start + offset);
    let insertion = if html
        .get(start..start + 9)
        .is_some_and(|value| value.eq_ignore_ascii_case(b"<!doctype"))
    {
        html[start..]
            .iter()
            .position(|byte| *byte == b'>')
            .map_or(start, |offset| start + offset + 1)
    } else {
        start
    };
    html.splice(insertion..insertion, bootstrap.bytes());
}

fn preview_local_response(
    config: &AppConfig,
    path: &str,
    upstream: &Url,
    serving: &Url,
) -> std::result::Result<Response, HttpError> {
    let mut response = local_response(config, path)?;
    if response.mime == "text/html" {
        let Body::File(file, _) = &mut response.body else {
            unreachable!("local file response")
        };
        let mut html = Vec::new();
        file.take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut html)
            .map_err(|_| HttpError::new(403, "Forbidden", "Local resource is not readable\n"))?;
        if html.len() > 32 * 1024 * 1024 {
            return Err(HttpError::new(
                413,
                "Payload Too Large",
                "Local HTML is too large\n",
            ));
        }
        insert_bootstrap(
            &mut html,
            &format!("<script>{}</script>", backend_script(upstream, serving)),
        );
        response.body = Body::Bytes(html);
    }
    Ok(response)
}

fn local_response(config: &AppConfig, path: &str) -> std::result::Result<Response, HttpError> {
    let resource = resolve_local(config, path)?;
    let file = File::open(&resource.path)
        .map_err(|_| HttpError::new(403, "Forbidden", "Local resource is not readable\n"))?;
    let mut response = Response::file(resource.mime, file, resource.length);
    response.headers.extend([
        ("Cache-Control".into(), "no-store".to_owned()),
        ("Referrer-Policy".into(), "no-referrer".to_owned()),
        ("X-Content-Type-Options".into(), "nosniff".to_owned()),
        (
            "Cross-Origin-Resource-Policy".into(),
            "cross-origin".to_owned(),
        ),
        ("Access-Control-Allow-Origin".into(), "null".to_owned()),
    ]);
    Ok(response)
}

#[derive(Debug)]
struct LocalResource {
    path: PathBuf,
    mime: &'static str,
    length: u64,
}

fn resolve_local(config: &AppConfig, path: &str) -> std::result::Result<LocalResource, HttpError> {
    let mut parts = path.strip_prefix("local/").unwrap_or_default().split('/');
    let token = parts.next().unwrap_or_default();
    if !is_token(token) {
        return Err(HttpError::new(
            403,
            "Forbidden",
            "Invalid local capability\n",
        ));
    }
    let segments = parts
        .map(decode_segment)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if segments.is_empty() {
        return Err(HttpError::new(400, "Bad Request", "Missing local path\n"));
    }
    let mut root_encoded = None;
    for url in config
        .profiles
        .iter()
        .flat_map(|profile| &profile.overlays)
        .filter_map(|item| item.url.as_ref())
        .filter(|url| {
            url.scheme() == "vosulay-overlay"
                && url.host_str() == Some(token)
                && url.port().is_none()
        })
    {
        let Some(candidate) = url.path_segments().and_then(|mut values| values.next()) else {
            continue;
        };
        if root_encoded.is_some_and(|root| root != candidate) {
            return Err(HttpError::new(
                403,
                "Forbidden",
                "Local capability is ambiguous\n",
            ));
        }
        root_encoded = Some(candidate);
    }
    let root_encoded = root_encoded
        .ok_or_else(|| HttpError::new(403, "Forbidden", "Local capability is not configured\n"))?;
    let root_bytes = decode_base64(root_encoded)
        .ok_or_else(|| HttpError::new(403, "Forbidden", "Invalid local root\n"))?;
    let root_text = std::str::from_utf8(&root_bytes)
        .map_err(|_| HttpError::new(403, "Forbidden", "Invalid local root\n"))?;
    let root = fs::canonicalize(root_text)
        .map_err(|_| HttpError::new(404, "Not Found", "Local root not found\n"))?;
    if !root.is_dir() || root != Path::new(root_text) {
        return Err(HttpError::new(
            403,
            "Forbidden",
            "Local root is not canonical\n",
        ));
    }
    let mut requested = root.clone();
    for segment in segments {
        requested.push(segment);
    }
    let target = fs::canonicalize(requested)
        .map_err(|_| HttpError::new(404, "Not Found", "Local resource not found\n"))?;
    if target == root || !target.starts_with(&root) {
        return Err(HttpError::new(
            403,
            "Forbidden",
            "Local resource escapes its root\n",
        ));
    }
    let metadata = fs::metadata(&target)
        .map_err(|_| HttpError::new(404, "Not Found", "Local resource not found\n"))?;
    if !metadata.is_file() {
        return Err(HttpError::new(
            403,
            "Forbidden",
            "Local resource is not a file\n",
        ));
    }
    Ok(LocalResource {
        mime: mime_type(&target),
        path: target,
        length: metadata.len(),
    })
}

fn decode_segment(value: &str) -> std::result::Result<String, HttpError> {
    if value.is_empty() {
        return Err(HttpError::new(400, "Bad Request", "Empty path segment\n"));
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(HttpError::new(
                    400,
                    "Bad Request",
                    "Invalid percent encoding\n",
                ));
            }
            let (Some(a), Some(b)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) else {
                return Err(HttpError::new(
                    400,
                    "Bad Request",
                    "Invalid percent encoding\n",
                ));
            };
            output.push(a << 4 | b);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    let value = String::from_utf8(output)
        .map_err(|_| HttpError::new(400, "Bad Request", "Invalid UTF-8 path\n"))?;
    if matches!(value.as_str(), "." | "..") || value.contains(['/', '\\', '\0']) {
        return Err(HttpError::new(400, "Bad Request", "Unsafe path segment\n"));
    }
    Ok(value)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
fn is_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.contains('=') || value.len() % 4 == 1 {
        return None;
    }
    let mut output = Vec::new();
    let (mut accumulator, mut bits) = (0_u32, 0_u32);
    for byte in value.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        accumulator = accumulator << 6 | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((accumulator >> bits) as u8);
            accumulator &= (1_u32 << bits).wrapping_sub(1);
        }
    }
    (accumulator == 0).then_some(output)
}

fn mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

pub fn peer_is_allowed(peer: IpAddr, allowed: &str) -> bool {
    let peer = normalize_ip(peer);
    if peer.is_loopback() {
        return true;
    }
    allowed
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .any(|token| {
            if let Some(pattern) = Ipv4Pattern::parse(token) {
                return matches!(peer, IpAddr::V4(address) if pattern.matches(address));
            }
            token
                .parse::<IpAddr>()
                .is_ok_and(|address| normalize_ip(address) == peer)
        })
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = address else {
        return address;
    };
    let octets = v6.octets();
    if octets[..10] == [0; 10] && octets[10..12] == [0xff, 0xff] {
        IpAddr::V4(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else {
        IpAddr::V6(v6)
    }
}

#[derive(Clone, Copy)]
struct Ipv4Pattern([Option<u8>; 4]);

impl Ipv4Pattern {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.trim().split('.');
        let mut result = [None; 4];
        for item in &mut result {
            let part = parts.next()?;
            *item = if part == "*" {
                None
            } else {
                if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                let parsed = part.parse::<u8>().ok()?;
                if parsed.to_string() != part {
                    return None;
                }
                Some(parsed)
            };
        }
        parts.next().is_none().then_some(Self(result))
    }

    fn matches(self, address: Ipv4Addr) -> bool {
        self.0
            .into_iter()
            .zip(address.octets())
            .all(|(expected, actual)| expected.is_none_or(|expected| expected == actual))
    }
}

#[derive(Debug)]
struct HttpError {
    status: u16,
    reason: &'static str,
    message: String,
    suppress_body: bool,
}
impl HttpError {
    fn new(status: u16, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            message: message.into(),
            suppress_body: false,
        }
    }
}

enum Body {
    Bytes(Vec<u8>),
    File(File, u64),
    Shared(Utf8Bytes),
    Proxy(ureq::BodyReader<'static>, Option<u64>),
}
struct Response {
    status: u16,
    reason: &'static str,
    mime: String,
    headers: Vec<(String, String)>,
    body: Body,
}
impl Response {
    fn plain(status: u16, reason: &'static str, value: &str) -> Self {
        Self::bytes(
            status,
            reason,
            "text/plain; charset=utf-8",
            value.as_bytes().to_vec(),
        )
    }
    fn bytes(status: u16, reason: &'static str, mime: &'static str, value: Vec<u8>) -> Self {
        Self {
            status,
            reason,
            mime: mime.to_owned(),
            headers: Vec::new(),
            body: Body::Bytes(value),
        }
    }
    fn file(mime: &'static str, file: File, length: u64) -> Self {
        Self {
            status: 200,
            reason: "OK",
            mime: mime.to_owned(),
            headers: Vec::new(),
            body: Body::File(file, length),
        }
    }
    fn write(self, stream: &mut TcpStream, head: bool) -> io::Result<()> {
        let length = match &self.body {
            Body::Bytes(value) => Some(value.len() as u64),
            Body::Shared(value) => Some(value.len() as u64),
            Body::File(_, length) => Some(*length),
            Body::Proxy(_, length) => *length,
        };
        write!(
            stream,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n",
            self.status, self.reason, self.mime
        )?;
        match length {
            Some(length) => write!(stream, "Content-Length: {length}\r\n")?,
            None => stream.write_all(b"Transfer-Encoding: chunked\r\n")?,
        }
        for (name, value) in self.headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        if !head {
            match self.body {
                Body::Bytes(value) => stream.write_all(&value)?,
                Body::Shared(value) => stream.write_all(value.as_bytes())?,
                Body::File(mut file, length) => {
                    io::copy(&mut Read::by_ref(&mut file).take(length), stream)?;
                }
                Body::Proxy(mut reader, Some(length)) => {
                    io::copy(&mut reader.by_ref().take(length), stream)?;
                }
                Body::Proxy(mut reader, None) => {
                    let mut buffer = [0_u8; 32 * 1024];
                    loop {
                        let count = reader.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        write!(stream, "{count:x}\r\n")?;
                        stream.write_all(&buffer[..count])?;
                        stream.write_all(b"\r\n")?;
                    }
                    stream.write_all(b"0\r\n\r\n")?;
                }
            }
        }
        stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Shutdown};

    fn base() -> Url {
        Url::parse("http://127.0.0.1:24050/").expect("URL")
    }

    #[test]
    fn opaque_resource_policy_replaces_upstream_headers_without_credentials() {
        let mut response = Response::plain(200, "OK", "counter resource");
        response.headers.extend([
            (
                "ACCESS-CONTROL-ALLOW-ORIGIN".into(),
                "http://upstream".into(),
            ),
            ("access-control-allow-credentials".into(), "true".into()),
            ("Cross-Origin-Resource-Policy".into(), "same-origin".into()),
            ("Cache-Control".into(), "no-cache".into()),
        ]);
        opaque_counter_resource(&mut response);
        assert_eq!(
            response.headers,
            [
                ("Cache-Control".into(), "no-cache".into()),
                ("Access-Control-Allow-Origin".into(), "null".into()),
                ("Cross-Origin-Resource-Policy".into(), "cross-origin".into()),
                ("Referrer-Policy".into(), "no-referrer".into()),
            ]
        );
    }

    #[test]
    fn preview_authority_requires_exact_capability_port_and_loopback_peer() {
        let preview = Url::parse(&format!(
            "http://{}.{}.preview.localhost:5050/",
            "1".repeat(32),
            "2".repeat(32)
        ))
        .unwrap();
        let mut request = Request {
            method: "GET".into(),
            target: "/counter/style.css".into(),
            host: Some(preview[url::Position::BeforeHost..url::Position::AfterPort].into()),
            origin: Some("null".into()),
            fetch_site: Some("cross-site".into()),
            version: http::Version::HTTP_11,
            headers: vec![],
            body: vec![],
            prefetched: vec![],
        };
        let local = "127.0.0.1:5050".parse().unwrap();
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(preview_authority_is_allowed(
            &request, &preview, peer, local
        ));
        assert!(!preview_authority_is_allowed(
            &request,
            &preview,
            "192.0.2.1".parse().unwrap(),
            local
        ));
        assert!(!preview_authority_is_allowed(
            &request,
            &preview,
            peer,
            "192.0.2.1:5050".parse().unwrap()
        ));
        assert!(!request_metadata_is_allowed(&request, local, true));
        for host in [
            "127.0.0.1:5050",
            "guessed.preview.localhost:5050",
            &format!(
                "{}.{}.preview.localhost:5051",
                "1".repeat(32),
                "2".repeat(32)
            ),
        ] {
            request.host = Some(host.into());
            assert!(!preview_authority_is_allowed(
                &request, &preview, peer, local
            ));
        }
        for route in [
            "overlay",
            "__vosulay/view-state",
            "__vosulay/backend.js",
            "__vosulay/manage/message",
            "v1/manage/state",
            "tools/vosulay/api/v1/manage/state",
            "config.json",
        ] {
            assert!(!preview_resource_route(route), "{route}");
        }
    }

    #[test]
    fn broker_token_needs_the_in_game_secret_this_machine_and_this_origin() {
        let listener: SocketAddr = "127.0.0.1:5050".parse().unwrap();
        let loopback: IpAddr = Ipv4Addr::LOCALHOST.into();
        let secret = "5".repeat(64);
        let with =
            |query: &str| format!("/websocket/commands?l=%2F%40vosu-profile%2Fstream%2Fc{query}");
        let request =
            |target: String, host: &str, origin: Option<&str>, site: Option<&str>| Request {
                method: "GET".into(),
                target,
                host: Some(host.into()),
                origin: origin.map(Into::into),
                fetch_site: site.map(Into::into),
                version: http::Version::HTTP_11,
                headers: Vec::new(),
                body: Vec::new(),
                prefetched: Vec::new(),
            };
        let presented = with(&format!("&__vosulay_broker={secret}"));
        let own = |target: String| {
            request(
                target,
                "127.0.0.1:5050",
                Some("http://127.0.0.1:5050"),
                Some("same-origin"),
            )
        };
        // The secret never reaches vosu, and the counter's own query stays byte for byte.
        assert_eq!(
            broker_upgrade_target(&own(presented.clone()), loopback, listener, &secret),
            (with(""), true)
        );
        assert_eq!(
            broker_upgrade_target(
                &request(
                    format!("/websocket/commands?__vosulay_broker={secret}"),
                    "localhost:5050",
                    Some("http://localhost:5050"),
                    None
                ),
                loopback,
                listener,
                &secret
            ),
            ("/websocket/commands".to_owned(), true)
        );
        // A program that reaches the port and claims this listener's origin, without the secret.
        for target in [
            with(""),
            with("&__vosulay_broker="),
            with(&format!("&__vosulay_broker={}", "6".repeat(64))),
            with(&format!(
                "&__vosulay_broker={secret}&__vosulay_broker={secret}"
            )),
        ] {
            let forged = request(
                target,
                "127.0.0.1:5050",
                Some("http://127.0.0.1:5050"),
                None,
            );
            let (forwarded, credentialed) =
                broker_upgrade_target(&forged, loopback, listener, &secret);
            assert!(!credentialed, "{}", forged.target);
            assert!(!forwarded.contains("__vosulay_broker"), "{forwarded}");
        }
        // With the secret: a LAN viewer, and pages that are not this listener's.
        assert!(
            !broker_upgrade_target(
                &own(presented.clone()),
                "192.168.1.20".parse().unwrap(),
                listener,
                &secret
            )
            .1
        );
        for (host, origin, site) in [
            ("127.0.0.1:5050", None, None),
            (
                "127.0.0.1:5050",
                Some("http://evil.example"),
                Some("cross-site"),
            ),
            (
                "127.0.0.1:5050",
                Some("http://127.0.0.1:24050"),
                Some("same-site"),
            ),
            ("127.0.0.1:5050", Some("null"), Some("cross-site")),
            ("evil.example:5050", Some("http://evil.example:5050"), None),
        ] {
            let foreign = request(presented.clone(), host, origin, site);
            assert!(
                !broker_upgrade_target(&foreign, loopback, listener, &secret).1,
                "{host} {origin:?}"
            );
        }
    }

    #[test]
    fn listen_address_uses_configured_host_and_port() {
        let settings = AppSettings {
            remote_listen_host: "127.0.0.1".to_owned(),
            remote_listen_port: 8080,
            ..AppSettings::default()
        };
        assert_eq!(
            listen_address(&settings),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080)
        );
    }

    #[test]
    fn listen_address_falls_back_to_loopback_for_unparseable_host() {
        let settings = AppSettings {
            remote_listen_host: "not-an-ip".to_owned(),
            remote_listen_port: 5050,
            ..AppSettings::default()
        };
        assert_eq!(
            listen_address(&settings),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5050)
        );
    }

    #[test]
    fn peer_matching_covers_loopback_exact_wildcard_and_tokens() {
        assert!(peer_is_allowed(IpAddr::V4(Ipv4Addr::LOCALHOST), "absolute"));
        assert!(peer_is_allowed(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)),
            "192.168.*.*"
        ));
        assert!(!peer_is_allowed(
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            "localhost,absolute"
        ));
        assert!(!peer_is_allowed(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)),
            "192.168.*"
        ));
    }
    #[test]
    fn profiles_are_active_or_one_based_and_bad_queries_reject() {
        let mut config = AppConfig::default();
        config.profiles.push(Profile {
            id: "two".to_owned(),
            name: "Two".to_owned(),
            overlays: Vec::new(),
            ruleset: None,
        });
        config.active_profile = "two".to_owned();
        assert_eq!(
            (
                select_profile(&config, None).expect("active").id.as_str(),
                select_profile(&config, Some(1)).expect("one").id.as_str()
            ),
            ("two", "default")
        );
        assert_eq!(
            parse_profile("/overlay?profile=0")
                .expect_err("zero")
                .status,
            400
        );
        assert_eq!(
            parse_profile("/overlay?profile=1&profile=2")
                .expect_err("duplicate")
                .status,
            400
        );
        assert_eq!(
            select_profile(&config, Some(3)).expect_err("range").status,
            404
        );
    }
    #[test]
    fn rendering_preserves_geometry_crop_and_counter_url() {
        let profile = Profile {
            id: "x".to_owned(),
            name: "X".to_owned(),
            overlays: vec![Placement {
                counter_id: "a/b".to_owned(),
                left: 12.5,
                top: -3.0,
                width: 640.0,
                height: 360.0,
                scale: 1.25,
                opacity: 0.75,
                rotation: -15.0,
                crop_top: 20.0,
                crop_right: 40.0,
                crop_left: 100.0,
                z_index: 7,
                ..Placement::default()
            }],
            ruleset: None,
        };
        let html = render_profile(&profile, &base(), &base()).expect("render");
        assert!(html.contains("http://127.0.0.1:24050/a%2Fb/"));
        assert!(html.contains("allow=\"autoplay\""));
        assert!(html.contains("<div style=\"left:12.5px;top:-3px;width:500px;height:340px;transform:scale(1.25) rotate(-15deg);opacity:0.75;z-index:7\">"));
        assert!(
            html.contains(
                "style=\"left:-100px;top:-20px;width:640px;height:360px\"></iframe></div>"
            )
        );
    }

    #[test]
    fn pinned_counter_loads_with_its_vosu_profile() {
        let placement = Placement {
            counter_id: "a/b".to_owned(),
            ..Placement::default()
        };
        let profile = Profile {
            id: "x".to_owned(),
            name: "X".to_owned(),
            overlays: vec![
                Placement {
                    vosu_profile: Some("stream".to_owned()),
                    ..placement.clone()
                },
                placement,
            ],
            ruleset: None,
        };
        let html = render_profile(&profile, &base(), &base()).expect("render");
        assert!(html.contains("src=\"http://127.0.0.1:24050/a%2Fb/?vosuProfile=stream\""));
        assert!(html.contains("src=\"http://127.0.0.1:24050/a%2Fb/\""));
    }

    #[test]
    fn disabled_counter_is_not_rendered_or_loaded_remotely() {
        let profile = Profile {
            id: "disabled".to_owned(),
            name: "Disabled".to_owned(),
            overlays: vec![Placement {
                counter_id: "secret-counter".to_owned(),
                enabled: false,
                ..Placement::default()
            }],
            ruleset: None,
        };
        let html = render_profile(&profile, &base(), &base()).expect("render");
        assert!(!html.contains("secret-counter"));
        assert!(!html.contains("<iframe"));
    }

    #[test]
    fn custom_frame_sandboxes_isolate_forwarded_local_sources() {
        let profile = Profile {
            id: "custom".to_owned(),
            name: "Custom".to_owned(),
            overlays: vec![
                Placement {
                    url: Some(
                        Url::parse(
                            "vosulay-overlay://0123456789abcdef0123456789abcdef/cm9vdA/index.html",
                        )
                        .expect("local URL"),
                    ),
                    name: Some("local".to_owned()),
                    ..Placement::default()
                },
                Placement {
                    url: Some(Url::parse("https://overlay.example/widget").expect("remote URL")),
                    name: Some("remote".to_owned()),
                    ..Placement::default()
                },
            ],
            ruleset: None,
        };

        let html = render_profile(&profile, &base(), &base()).expect("render");
        assert!(html.contains(
            "src=\"/local/0123456789abcdef0123456789abcdef/index.html\" title=\"local\" allow=\"autoplay\" loading=\"eager\" sandbox=\"allow-scripts\""
        ));
        assert!(html.contains(
            "src=\"https://overlay.example/widget\" title=\"remote\" allow=\"autoplay\" loading=\"eager\" sandbox=\"allow-scripts allow-same-origin\""
        ));
    }
    fn encoded(value: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let (mut out, mut acc, mut bits) = (String::new(), 0_u32, 0_u32);
        for byte in value {
            acc = acc << 8 | u32::from(*byte);
            bits += 8;
            while bits >= 6 {
                bits -= 6;
                out.push(A[((acc >> bits) & 63) as usize] as char);
                acc &= (1_u32 << bits).wrapping_sub(1);
            }
        }
        if bits > 0 {
            out.push(A[((acc << (6 - bits)) & 63) as usize] as char);
        }
        out
    }
    #[test]
    fn local_capability_and_traversal_are_rejected() {
        let directory = tempfile::tempdir().expect("temp");
        fs::write(directory.path().join("index.html"), b"ok").expect("write");
        let root = fs::canonicalize(directory.path()).expect("root");
        let token = "0123456789abcdef0123456789abcdef";
        let root64 = encoded(root.as_os_str().as_encoded_bytes());
        let url =
            Url::parse(&format!("vosulay-overlay://{token}/{root64}/index.html")).expect("url");
        let mut config = AppConfig::default();
        config.profiles[0].overlays.push(Placement {
            url: Some(url),
            ..Placement::default()
        });
        assert_eq!(
            resolve_local(&AppConfig::default(), &format!("local/{token}/index.html"))
                .expect_err("capability")
                .status,
            403
        );
        assert_eq!(
            resolve_local(&config, &format!("local/{token}/%2e%2e/x"))
                .expect_err("traversal")
                .status,
            400
        );
        assert_eq!(
            resolve_local(&config, &format!("local/{token}/index.html"))
                .expect("file")
                .mime,
            "text/html; charset=utf-8"
        );
        let remote_url =
            placement_url(&config.profiles[0].overlays[0], &base(), &base()).expect("remote URL");
        assert_eq!(remote_url, format!("/local/{token}/index.html"));
        assert!(!remote_url.contains(&root64));
    }

    #[test]
    fn opaque_local_frame_requests_receive_scoped_resource_headers() {
        let directory = tempfile::tempdir().expect("temp");
        fs::write(directory.path().join("external.js"), b"window.loaded=true").expect("write");
        let root = fs::canonicalize(directory.path()).expect("root");
        let capability = "0123456789abcdef0123456789abcdef";
        let root64 = encoded(root.as_os_str().as_encoded_bytes());
        let mut config = AppConfig::default();
        config.profiles[0].overlays.push(Placement {
            url: Some(
                Url::parse(&format!(
                    "vosulay-overlay://{capability}/{root64}/index.html"
                ))
                .expect("URL"),
            ),
            ..Placement::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = RemoteServer::start_with_listener(
            listener,
            base(),
            Arc::new(move || Ok(config.clone())),
        )
        .expect("server");
        let send = |target: &str| {
            let mut client = TcpStream::connect(address).expect("connect");
            let request = format!(
                "GET {target} HTTP/1.1\r\nHost: {address}\r\nOrigin: null\r\nSec-Fetch-Site: cross-site\r\n\r\n"
            );
            client.write_all(request.as_bytes()).expect("write");
            client.shutdown(Shutdown::Write).expect("shutdown");
            let mut response = String::new();
            client.read_to_string(&mut response).expect("read");
            response
        };

        let local = send(&format!("/local/{capability}/external.js"));
        assert!(local.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(local.contains("Cross-Origin-Resource-Policy: cross-origin\r\n"));
        assert!(local.contains("Access-Control-Allow-Origin: null\r\n"));
        assert!(local.ends_with("window.loaded=true"));

        let overlay = send("/overlay");
        assert!(overlay.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        drop(server);
    }
    #[test]
    fn request_authority_accepts_destination_ip_and_loopback_names_only() {
        let local = "127.0.0.1:5050".parse().expect("socket address");

        assert!(request_authority("127.0.0.1:5050", local).is_some());
        assert!(request_authority("localhost:5050", local).is_some());
        assert!(request_authority("manage.localhost:5050", local).is_some());
        assert!(request_authority("other.localhost:5050", local).is_none());
        assert!(request_authority("attacker.example:5050", local).is_none());
        assert!(request_authority("127.0.0.1:5051", local).is_none());

        // A LAN client reaching the LAN address cannot name the loopback-only hosts.
        let lan = "192.168.1.20:5050".parse().expect("socket address");
        assert!(request_authority("192.168.1.20:5050", lan).is_some());
        assert!(request_authority("localhost:5050", lan).is_none());
        assert!(request_authority("manage.localhost:5050", lan).is_none());
        assert!(request_authority("192.168.1.21:5050", lan).is_none());
    }

    #[test]
    fn shutdown_releases_idle_loopback_and_wildcard_listeners() {
        let bind = |ip| {
            // Keep these ports below Linux/Windows' default ephemeral ranges:
            // bind(0) can be reassigned to another parallel test after shutdown.
            // Random candidates also keep concurrent test processes independent.
            for _ in 0..32 {
                let mut random = [0; 2];
                getrandom::fill(&mut random).expect("random test port");
                let port = 20_000 + u16::from_ne_bytes(random) % 12_001;
                if (24050..=24052).contains(&port)
                    || [
                        crate::config::DEFAULT_REMOTE_LISTEN_PORT,
                        crate::config::DEFAULT_DEVTOOLS_PORT,
                    ]
                    .contains(&port)
                {
                    continue;
                }
                match TcpListener::bind((ip, port)) {
                    // Keep the successfully probed socket; never release it
                    // between candidate selection and handing it to the server.
                    Ok(listener) => return Ok(listener),
                    Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "no free test port in 20000-32000 after 32 attempts",
            ))
        };
        fn occupant(address: SocketAddr) -> String {
            #[cfg(target_os = "linux")]
            {
                let table = fs::read_to_string("/proc/net/tcp").unwrap_or_default();
                let suffix = format!(":{:04X}", address.port());
                let mut evidence = String::new();
                for row in table.lines().skip(1) {
                    let fields: Vec<_> = row.split_whitespace().collect();
                    if fields.len() < 10 || !fields[1].ends_with(&suffix) {
                        continue;
                    }
                    evidence.push_str(row);
                    evidence.push('\n');
                    let inode = format!("socket:[{}]", fields[9]);
                    for process in fs::read_dir("/proc").into_iter().flatten().flatten() {
                        if !process
                            .file_name()
                            .to_string_lossy()
                            .bytes()
                            .all(|byte| byte.is_ascii_digit())
                        {
                            continue;
                        }
                        for fd in fs::read_dir(process.path().join("fd"))
                            .into_iter()
                            .flatten()
                            .flatten()
                        {
                            if fs::read_link(fd.path())
                                .is_ok_and(|link| link.as_os_str() == inode.as_str())
                            {
                                let name = fs::read_to_string(process.path().join("comm"))
                                    .unwrap_or_default();
                                evidence.push_str(&format!(
                                    "pid={} name={}",
                                    process.file_name().to_string_lossy(),
                                    name
                                ));
                                break;
                            }
                        }
                    }
                }
                evidence
            }
            #[cfg(not(target_os = "linux"))]
            {
                format!("occupant diagnostics unavailable for {address}")
            }
        }
        let listeners = vec![
            bind(Ipv4Addr::LOCALHOST).expect("local listener"),
            bind(Ipv4Addr::UNSPECIFIED).expect("LAN listener"),
        ];
        let addresses: Vec<_> = listeners
            .iter()
            .map(|listener| listener.local_addr().expect("listener address"))
            .collect();
        let server = RemoteServer::start_with_listeners(
            listeners,
            base(),
            Arc::new(|| Ok(AppConfig::default())),
            None,
            None,
        )
        .expect("server");
        for address in &addresses {
            assert_eq!(
                TcpListener::bind(address)
                    .expect_err("server still owns its listener")
                    .kind(),
                io::ErrorKind::AddrInUse
            );
        }
        let (stopped, completion) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(server);
            let _ = stopped.send(());
        });
        completion
            .recv_timeout(TIMEOUT)
            .expect("idle listener shutdown must not wait for an HTTP request");
        let _rebound: Vec<_> = addresses
            .into_iter()
            .map(|address| {
                TcpListener::bind(address).unwrap_or_else(|error| {
                    panic!(
                        "listener {address} not released: {error}; occupant:\n{}",
                        occupant(address)
                    )
                })
            })
            .collect();
    }

    #[test]
    fn ephemeral_listener_serves_overlay() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let mut config = AppConfig::default();
        config.profiles[0].overlays.push(Placement {
            counter_id: "counter".to_owned(),
            ..Placement::default()
        });
        let server = RemoteServer::start_with_listener(
            listener,
            base(),
            Arc::new(move || Ok(config.clone())),
        )
        .expect("server");
        let mut client = TcpStream::connect(address).expect("connect");
        let request = format!("GET /overlay HTTP/1.1\r\nHost: {address}\r\n\r\n");
        client.write_all(request.as_bytes()).expect("write");
        client.shutdown(Shutdown::Write).expect("shutdown");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        drop(server);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("allow=\"autoplay\""));
        assert!(response.contains(&format!("src=\"http://{address}/counter/\"")));
    }
}

#[cfg(test)]
mod tests_broker;

#[cfg(test)]
mod profile_follow_tests {
    use super::*;
    #[test]
    fn explicit_url_profiles_never_follow_the_active_profile() {
        let mut config = AppConfig::default();
        config.profiles.push(Profile {
            id: "mania".into(),
            name: "Mania".into(),
            overlays: vec![],
            ruleset: Some(crate::ruleset::Ruleset::Mania),
        });
        config.active_profile = "mania".into();
        let base = Url::parse("http://127.0.0.1:5050/").unwrap();
        for (target, follows) in [
            ("/overlay", true),
            ("/overlay?profile=1", false),
            ("/v1/view", true),
            ("/v1/view?profile=1", false),
        ] {
            let response = overlay_response(&base, &base, &config, target).unwrap();
            let Body::Bytes(bytes) = response.body else {
                panic!("HTML response");
            };
            let html = String::from_utf8(bytes).unwrap();
            assert_eq!(html.contains("setInterval"), follows, "{target}");
            assert!(html.contains(if follows {
                "<title>Mania</title>"
            } else {
                "<title>Default</title>"
            }));
            if target == "/v1/view" {
                assert!(html.contains("/tools/vosulay/api/v1/view/state"));
            }
        }
    }
}
