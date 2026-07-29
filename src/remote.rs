use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use url::Url;

use crate::config::{AppConfig, Placement, Profile};

pub const DEFAULT_VOSU_URL: &str = "http://127.0.0.1:24050";
const ADDRESS: &str = "0.0.0.0:5050";
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REQUEST: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
type ConfigLoader = dyn Fn() -> Result<AppConfig> + Send + Sync;

pub struct RemoteServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteServer {
    pub fn start(base_url: Url) -> Result<Self> {
        let listener = TcpListener::bind(ADDRESS)
            .with_context(|| format!("failed to bind remote overlay server to {ADDRESS}"))?;
        Self::start_with_listener(listener, base_url, Arc::new(AppConfig::load))
    }

    fn start_with_listener(
        listener: TcpListener,
        base_url: Url,
        loader: Arc<ConfigLoader>,
    ) -> Result<Self> {
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("vosulay-remote-listener".to_owned())
            .spawn(move || serve(listener, base_url, loader, thread_stop))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, base_url: Url, loader: Arc<ConfigLoader>, stop: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                    let _ = stream.set_write_timeout(Some(TIMEOUT));
                    let _ = Response::plain(503, "Service Unavailable", "Server is busy\n")
                        .write(&mut stream, true);
                    continue;
                }
                active.fetch_add(1, Ordering::AcqRel);
                let base_url = base_url.clone();
                let loader = Arc::clone(&loader);
                let request_active = Arc::clone(&active);
                if let Err(error) = thread::Builder::new()
                    .name("vosulay-remote-request".to_owned())
                    .spawn(move || {
                        let _active_request = ActiveRequest(request_active);
                        if let Err(error) = handle(&mut stream, &base_url, &loader) {
                            eprintln!("Remote overlay request failed: {error}");
                        }
                    })
                {
                    active.fetch_sub(1, Ordering::AcqRel);
                    eprintln!("Could not start remote overlay request worker: {error}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
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

fn handle(stream: &mut TcpStream, base_url: &Url, loader: &Arc<ConfigLoader>) -> Result<()> {
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
    let config = if path == "/overlay" || path.starts_with("/local/") {
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
        if !peer_is_allowed(stream.peer_addr()?.ip(), &config.settings.allowed_ips) {
            return Response::plain(403, "Forbidden", "Peer is not allowed\n")
                .write(stream, request.method == "HEAD")
                .map_err(Into::into);
        }
        Some(config)
    } else {
        None
    };
    if request.method != "GET" && request.method != "HEAD" {
        let mut response = Response::plain(405, "Method Not Allowed", "Use GET or HEAD\n");
        response.headers.push(("Allow", "GET, HEAD".to_owned()));
        return response.write(stream, false).map_err(Into::into);
    }
    let result = if path == "/overlay" {
        overlay_response(
            base_url,
            request.host.as_deref(),
            config
                .as_ref()
                .ok_or_else(|| anyhow!("missing protected route config"))?,
            &request.target,
        )
    } else if path.starts_with("/local/") {
        local_response(
            config
                .as_ref()
                .ok_or_else(|| anyhow!("missing protected route config"))?,
            path,
        )
    } else {
        Ok(Response::plain(404, "Not Found", "Route not found\n"))
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => Response::plain(error.status, error.reason, &error.message),
    };
    response
        .write(stream, request.method == "HEAD")
        .map_err(Into::into)
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
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
        if name.eq_ignore_ascii_case("transfer-encoding")
            || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
        {
            return Err(request_error(
                &bytes,
                400,
                "Bad Request",
                "Request bodies are not supported\n",
            ));
        }
        if name.eq_ignore_ascii_case("host") {
            let value = value.trim();
            if value.is_empty() || host.is_some() {
                return Err(request_error(
                    &bytes,
                    400,
                    "Bad Request",
                    "Invalid Host header\n",
                ));
            }
            host = Some(value.to_owned());
        }
    }
    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        host,
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
    request_host: Option<&str>,
    config: &AppConfig,
    target: &str,
) -> std::result::Result<Response, HttpError> {
    let profile = select_profile(config, parse_profile(target)?)?;
    let mut response = Response::bytes(
        200,
        "OK",
        "text/html; charset=utf-8",
        render_profile(profile, &vosu_url_for_request(base_url, request_host))?.into_bytes(),
    );
    response.headers.extend([
        ("Cache-Control", "no-store".to_owned()), ("Referrer-Policy", "no-referrer".to_owned()),
        ("X-Content-Type-Options", "nosniff".to_owned()), ("Cross-Origin-Resource-Policy", "cross-origin".to_owned()),
        ("Content-Security-Policy", "default-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; style-src 'unsafe-inline'; frame-src 'self' http: https:; frame-ancestors *".to_owned()),
    ]);
    Ok(response)
}

fn parse_profile(target: &str) -> std::result::Result<Option<usize>, HttpError> {
    let Some((path, query)) = target.split_once('?') else {
        return Ok(None);
    };
    if path != "/overlay" {
        return Err(HttpError::new(
            400,
            "Bad Request",
            "Malformed profile query\n",
        ));
    }
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

fn vosu_url_for_request(base_url: &Url, request_host: Option<&str>) -> Url {
    let loopback = match base_url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => normalize_ip(IpAddr::V6(address)).is_loopback(),
        None => false,
    };
    let Some(request_host) = request_host.filter(|_| loopback) else {
        return base_url.clone();
    };
    let Ok(request_url) = Url::parse(&format!("http://{request_host}/")) else {
        return base_url.clone();
    };
    if !request_url.username().is_empty()
        || request_url.password().is_some()
        || request_url.path() != "/"
        || request_url.query().is_some()
        || request_url.fragment().is_some()
    {
        return base_url.clone();
    }
    let Some(host) = request_url.host_str() else {
        return base_url.clone();
    };
    let mut result = base_url.clone();
    if result.set_host(Some(host)).is_err() {
        return base_url.clone();
    }
    result
}

fn render_profile(profile: &Profile, base_url: &Url) -> std::result::Result<String, HttpError> {
    let title = escape_html(&profile.name);
    let mut html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>html,body{{width:100%;height:100%;margin:0;overflow:hidden;background:transparent}}body{{position:relative}}iframe{{display:block;border:0}}</style></head><body>"
    );
    for item in &profile.overlays {
        let source = match placement_url(item, base_url) {
            Ok(source) => escape_html(&source),
            Err(_) if item.url.is_some() => continue,
            Err(error) => return Err(error),
        };
        let frame_title = escape_html(placement_title(item));
        let sandbox = if item.url.is_some() {
            " sandbox=\"allow-scripts allow-same-origin\""
        } else {
            ""
        };
        use std::fmt::Write as _;
        write!(html, "<iframe src=\"{source}\" title=\"{frame_title}\" allow=\"autoplay\" loading=\"eager\"{sandbox} referrerpolicy=\"no-referrer\" style=\"position:absolute;left:{}px;top:{}px;width:{}px;height:{}px;transform-origin:0 0;transform:scale({}) rotate({}deg);opacity:{};z-index:{}\"></iframe>", item.left, item.top, item.width, item.height, item.scale, item.rotation, item.opacity, item.z_index)
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

fn placement_url(item: &Placement, base_url: &Url) -> std::result::Result<String, HttpError> {
    let Some(custom) = item.url.as_ref() else {
        let mut url = base_url.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.set_path("/");
        url.path_segments_mut()
            .map_err(|()| HttpError::new(500, "Internal Server Error", "Invalid vosu URL\n"))?
            .push(&item.counter_id)
            .push("");
        return Ok(url.into());
    };
    match custom.scheme() {
        "http" | "https" => Ok(custom.as_str().to_owned()),
        "vosulay-overlay" => {
            let token = custom
                .host_str()
                .filter(|token| is_token(token))
                .ok_or_else(|| {
                    HttpError::new(500, "Internal Server Error", "Invalid local capability\n")
                })?;
            if custom.port().is_some()
                || custom
                    .path_segments()
                    .is_none_or(|segments| segments.count() < 2)
            {
                return Err(HttpError::new(
                    500,
                    "Internal Server Error",
                    "Invalid local path\n",
                ));
            }
            let mut result = format!("/local/{token}{}", custom.path());
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

fn local_response(config: &AppConfig, path: &str) -> std::result::Result<Response, HttpError> {
    let resource = resolve_local(config, path)?;
    let file = File::open(&resource.path)
        .map_err(|_| HttpError::new(403, "Forbidden", "Local resource is not readable\n"))?;
    let mut response = Response::file(resource.mime, file, resource.length);
    response.headers.extend([
        ("Cache-Control", "no-store".to_owned()),
        ("Referrer-Policy", "no-referrer".to_owned()),
        ("X-Content-Type-Options", "nosniff".to_owned()),
        ("Cross-Origin-Resource-Policy", "cross-origin".to_owned()),
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
    let mut parts = path.strip_prefix("/local/").unwrap_or_default().split('/');
    let token = parts.next().unwrap_or_default();
    let root_encoded = parts.next().unwrap_or_default();
    if !is_token(token) || root_encoded.is_empty() {
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
    let capable = config
        .profiles
        .iter()
        .flat_map(|profile| &profile.overlays)
        .any(|item| {
            item.url.as_ref().is_some_and(|url| {
                url.scheme() == "vosulay-overlay"
                    && url.host_str() == Some(token)
                    && url.port().is_none()
                    && url.path_segments().and_then(|mut values| values.next())
                        == Some(root_encoded)
            })
        });
    if !capable {
        return Err(HttpError::new(
            403,
            "Forbidden",
            "Local capability is not configured\n",
        ));
    }
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

pub fn vosu_base_url_from_arguments(arguments: &[String]) -> Result<Url> {
    let mut configured = None;
    let mut index = 1;
    while index < arguments.len() {
        let argument = &arguments[index];
        if matches!(argument.as_str(), "--vosu-url" | "-t") {
            index += 1;
            configured = Some(
                arguments
                    .get(index)
                    .ok_or_else(|| anyhow!("{argument} requires a URL value"))?
                    .as_str(),
            );
        } else if let Some(value) = argument
            .strip_prefix("--vosu-url=")
            .or_else(|| argument.strip_prefix("-t="))
        {
            if value.is_empty() {
                bail!("{argument} requires a URL value");
            }
            configured = Some(value);
        }
        index += 1;
    }
    let source = configured.unwrap_or(DEFAULT_VOSU_URL);
    let mut url = Url::parse(source).with_context(|| format!("invalid vosu URL {source:?}"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.cannot_be_a_base()
    {
        bail!("vosu URL must be an HTTP(S) origin");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("vosu URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("vosu URL must not contain a query or fragment");
    }
    url.set_path("/");
    Ok(url)
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
}
struct Response {
    status: u16,
    reason: &'static str,
    mime: &'static str,
    headers: Vec<(&'static str, String)>,
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
            mime,
            headers: Vec::new(),
            body: Body::Bytes(value),
        }
    }
    fn file(mime: &'static str, file: File, length: u64) -> Self {
        Self {
            status: 200,
            reason: "OK",
            mime,
            headers: Vec::new(),
            body: Body::File(file, length),
        }
    }
    fn write(self, stream: &mut TcpStream, head: bool) -> io::Result<()> {
        let length = match &self.body {
            Body::Bytes(value) => value.len() as u64,
            Body::File(_, length) => *length,
        };
        write!(
            stream,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status, self.reason, self.mime, length
        )?;
        for (name, value) in self.headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        if !head {
            match self.body {
                Body::Bytes(value) => stream.write_all(&value)?,
                Body::File(mut file, length) => {
                    io::copy(&mut Read::by_ref(&mut file).take(length), stream)?;
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
        Url::parse(DEFAULT_VOSU_URL).expect("URL")
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
    fn rendering_preserves_geometry_and_counter_url() {
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
                z_index: 7,
                ..Placement::default()
            }],
        };
        let html = render_profile(&profile, &base()).expect("render");
        assert!(html.contains("http://127.0.0.1:24050/a%2Fb/"));
        assert!(html.contains("left:12.5px;top:-3px;width:640px;height:360px;transform-origin:0 0;transform:scale(1.25) rotate(-15deg);opacity:0.75;z-index:7"));
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
            resolve_local(
                &AppConfig::default(),
                &format!("/local/{token}/{root64}/index.html")
            )
            .expect_err("capability")
            .status,
            403
        );
        assert_eq!(
            resolve_local(&config, &format!("/local/{token}/{root64}/%2e%2e/x"))
                .expect_err("traversal")
                .status,
            400
        );
        assert_eq!(
            resolve_local(&config, &format!("/local/{token}/{root64}/index.html"))
                .expect("file")
                .mime,
            "text/html; charset=utf-8"
        );
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
        client
            .write_all(b"GET /overlay HTTP/1.1\r\nHost: 192.0.2.44:5050\r\n\r\n")
            .expect("write");
        client.shutdown(Shutdown::Write).expect("shutdown");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        drop(server);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("src=\"http://192.0.2.44:24050/counter/\""));
    }
    #[test]
    fn runtime_url_options_match_qt() {
        let url = vosu_base_url_from_arguments(&[
            "vosulay".to_owned(),
            "--vosu-url=https://example.test/path".to_owned(),
        ])
        .expect("URL");
        assert_eq!(url.as_str(), "https://example.test/");
    }
}
