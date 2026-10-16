use super::*;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::sync::mpsc;
use std::thread;

struct Upstream {
    url: Url,
    address: SocketAddr,
    requests: mpsc::Receiver<String>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Upstream {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream bind");
        let address = listener.local_addr().expect("upstream address");
        listener.set_nonblocking(true).expect("nonblocking fixture");
        let (tx, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                stream
                    .set_read_timeout(Some(TIMEOUT))
                    .expect("fixture read timeout");
                stream
                    .set_write_timeout(Some(TIMEOUT))
                    .expect("fixture write timeout");
                let mut bytes = Vec::new();
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes).into_owned();
                let _ = tx.send(request.clone());
                let target = request.lines().next().unwrap_or_default();
                let (status, headers, body) = if target.starts_with("GET /asset.js?") {
                    (
                        "200 OK",
                        "Content-Type: application/javascript\r\nAccept-Ranges: bytes\r\n",
                        "window.assetLoaded=true;",
                    )
                } else if target.starts_with("GET /redirect") {
                    ("302 Found", "Location: ../next/page\r\n", "redirect")
                } else if target.starts_with("GET /missing") {
                    ("404 Not Found", "Content-Type: text/plain\r\n", "missing")
                } else if target.starts_with("GET /range") {
                    (
                        "206 Partial Content",
                        "Content-Range: bytes 2-4/6\r\n",
                        "cde",
                    )
                } else {
                    (
                        "200 OK",
                        "Content-Type: text/html; charset=utf-8\r\n",
                        "<!doctype html><html><head><script src=\"/stock.js\"></script></head><body>stock</body></html>",
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url: Url::parse(&format!("http://{address}/")).expect("upstream URL"),
            address,
            requests,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("upstream fixture stopped");
        }
    }
}

fn encode_root(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let (mut accumulator, mut bits) = (0_u32, 0_u32);
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            output.push(ALPHABET[((accumulator >> bits) & 63) as usize] as char);
            accumulator &= (1_u32 << bits).wrapping_sub(1);
        }
    }
    if bits > 0 {
        output.push(ALPHABET[((accumulator << (6 - bits)) & 63) as usize] as char);
    }
    output
}
fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then_some(value.trim())
        })
}
fn server(upstream: &Url, config: AppConfig) -> (RemoteServer, SocketAddr) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("proxy bind");
    let address = listener.local_addr().expect("proxy address");
    let server = RemoteServer::start_with_listener(
        listener,
        upstream.clone(),
        Arc::new(move || Ok(config.clone())),
    )
    .expect("proxy server");
    (server, address)
}

fn request(address: SocketAddr, method: &str, target: &str, extra: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("connect");
    write!(
        stream,
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\n{extra}Connection: close\r\n\r\n"
    )
    .expect("request");
    stream.shutdown(Shutdown::Write).expect("shutdown write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    response
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").map_or("", |(_, body)| body)
}

#[test]
fn overlay_routes_configured_counter_to_local_host_without_credentials() {
    let upstream = Upstream::start();
    let mut config = AppConfig::default();
    config.profiles[0].overlays.push(Placement {
        counter_id: "counter/one".into(),
        ..Placement::default()
    });
    let (server, address) = server(&upstream.url, config);
    let response = request(address, "GET", "/overlay", "");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(body(&response).contains(&format!("src=\"http://{address}/counter%2Fone/\"")));
    drop(server);
    drop(upstream);
}

#[test]
fn proxy_injects_bootstrap_and_preserves_upstream_asset_and_redirect_semantics() {
    let upstream = Upstream::start();
    let (server, address) = server(&upstream.url, AppConfig::default());
    let html = request(address, "GET", "/page?mode=stock", "");
    assert!(html.starts_with("HTTP/1.1 200 OK\r\n"));
    let html_body = body(&html);
    let bootstrap = html_body
        .find("/__vosulay/backend.js")
        .expect("external bootstrap injected");
    let stock = html_body.find("/stock.js").expect("stock script retained");
    assert!(bootstrap < stock, "bootstrap precedes original scripts");

    let asset = request(address, "GET", "/asset.js?v=7", "Range: bytes=0-4\r\n");
    assert!(asset.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(body(&asset).contains("window.assetLoaded=true;"));
    let redirect = request(address, "GET", "/redirect", "");
    assert!(redirect.starts_with("HTTP/1.1 302 Found\r\n"));
    assert_eq!(header(&redirect, "Location"), Some("../next/page"));
    let missing = request(address, "GET", "/missing", "");
    assert!(missing.starts_with("HTTP/1.1 404 Not Found\r\n"));
    let range = request(address, "GET", "/range", "Range: bytes=2-4\r\n");
    assert!(range.starts_with("HTTP/1.1 206 Partial Content\r\n"));
    assert_eq!(header(&range, "Content-Range"), Some("bytes 2-4/6"));
    assert_eq!(body(&range), "cde");
    let seen: Vec<_> = (0..5)
        .map(|_| {
            upstream
                .requests
                .recv_timeout(TIMEOUT)
                .expect("upstream request")
        })
        .collect();
    assert!(
        seen.iter()
            .any(|request| request.starts_with("GET /page?mode=stock "))
    );
    assert!(
        seen.iter()
            .any(|request| request.starts_with("GET /asset.js?v=7 "))
    );
    assert!(
        seen.iter()
            .any(|request| header(request, "Range") == Some("bytes=0-4"))
    );
    drop(server);
    drop(upstream);
}

#[test]
fn configured_upstream_origin_is_rerouted_but_distinct_custom_origin_is_preserved() {
    let upstream = Upstream::start();
    let mut config = AppConfig::default();
    config.profiles[0].overlays = vec![
        Placement {
            url: Some(
                upstream
                    .url
                    .join("alternate-counter/")
                    .expect("configured-origin URL"),
            ),
            name: Some("counter".to_owned()),
            ..Placement::default()
        },
        Placement {
            url: Some(Url::parse("https://widgets.example/custom?theme=dark").expect("custom URL")),
            name: Some("custom".to_owned()),
            ..Placement::default()
        },
    ];
    let (server, address) = server(&upstream.url, config);
    let response = request(address, "GET", "/overlay", "");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    let html = body(&response);
    assert!(html.contains(&format!("src=\"http://{address}/alternate-counter/\"")));
    assert!(html.contains("src=\"https://widgets.example/custom?theme=dark\""));
    drop(server);
    drop(upstream);
}

#[test]
fn custom_origin_and_host_rebinding_are_denied_but_plain_authority_is_accepted() {
    let upstream = Upstream::start();
    let (server, address) = server(&upstream.url, AppConfig::default());
    let ok = request(address, "GET", "/overlay", "");
    assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"));
    let origin = request(
        address,
        "GET",
        "/overlay",
        "Origin: http://attacker.example\r\n",
    );
    assert!(origin.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    let mut stream = TcpStream::connect(address).expect("connect");
    write!(
        stream,
        "GET /overlay HTTP/1.1\r\nHost: attacker.example\r\nConnection: close\r\n\r\n"
    )
    .expect("request");
    stream.shutdown(Shutdown::Write).expect("shutdown");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    drop(server);
    drop(upstream);
}

#[test]
fn local_resources_are_confined_and_escaped_paths_are_rejected_over_http() {
    let upstream = Upstream::start();
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("ok.js"), b"window.local=true").expect("resource");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret.js"), b"secret").expect("secret");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.js"),
        root.path().join("escape.js"),
    )
    .expect("symlink");
    let root64 = encode_root(
        std::fs::canonicalize(root.path())
            .expect("canonical root")
            .as_os_str()
            .as_encoded_bytes(),
    );
    let token = "0123456789abcdef0123456789abcdef";
    let mut config = AppConfig::default();
    config.profiles[0].overlays.push(Placement {
        url: Some(
            Url::parse(&format!("vosulay-overlay://{token}/{root64}/index.html"))
                .expect("local URL"),
        ),
        ..Placement::default()
    });
    let (server, address) = server(&upstream.url, config);
    let allowed = request(
        address,
        "GET",
        &format!("/local/{token}/ok.js"),
        "Origin: null\r\nSec-Fetch-Site: cross-site\r\n",
    );
    assert!(allowed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert_eq!(body(&allowed), "window.local=true");
    let traversal = request(
        address,
        "GET",
        &format!("/local/{token}/%2e%2e/secret.js"),
        "Origin: null\r\nSec-Fetch-Site: cross-site\r\n",
    );
    assert!(
        traversal.starts_with("HTTP/1.1 400 Bad Request\r\n")
            || traversal.starts_with("HTTP/1.1 403 Forbidden\r\n")
    );
    #[cfg(unix)]
    {
        let escaped = request(
            address,
            "GET",
            &format!("/local/{token}/escape.js"),
            "Origin: null\r\nSec-Fetch-Site: cross-site\r\n",
        );
        assert!(escaped.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        assert!(!body(&escaped).contains("secret"));
    }
    drop(server);
    drop(upstream);
}

/// Opens a commands socket on the listener as a page or program would, with `origin`.
fn commands_socket(
    address: SocketAddr,
    query: &str,
    origin: Option<&str>,
) -> tokio_tungstenite::tungstenite::WebSocket<TcpStream> {
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
    let mut request = format!("ws://{address}/websocket/commands?{query}")
        .into_client_request()
        .expect("request");
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_str(origin).expect("origin"));
    }
    let stream = TcpStream::connect(address).expect("connect");
    tokio_tungstenite::tungstenite::client(request, stream)
        .expect("upgrade")
        .0
}

#[test]
fn only_sockets_presenting_the_in_game_secret_reach_vosu_with_its_token() {
    use crate::backend::tests::{OLD_TOKEN, TokenUpstream, bearer, write_token};
    let upstream = TokenUpstream::start();
    upstream.accept_only(&[OLD_TOKEN]);
    let directory = tempfile::tempdir().expect("token directory");
    let path = directory.path().join("broker.json");
    write_token(&path, &upstream.url, OLD_TOKEN);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let address = listener.local_addr().expect("address");
    let server = RemoteServer::start_with_listeners(
        vec![listener],
        upstream.url.clone(),
        Arc::new(|| Ok(AppConfig::default())),
        None,
        crate::backend::BrokerCredential::at(path, &upstream.url),
    )
    .expect("server");
    let own = format!("http://{address}");
    let secret = server.browser_secret().to_owned();

    // An in-game counter frame: the secret and this listener's origin.
    let _counter = commands_socket(
        address,
        &format!("l=counter&__vosulay_broker={secret}"),
        Some(&own),
    );
    // Another program on this machine that claims the same origin but lacks the secret.
    let _forged = commands_socket(address, "l=forged", Some(&own));
    let _guessed = commands_socket(
        address,
        &format!("l=guessed&__vosulay_broker={}", "f".repeat(64)),
        Some(&own),
    );
    // A program with no Origin at all.
    let _bare = commands_socket(address, "l=bare", None);

    // vosu never sees the secret, and only the counter's socket carries the token.
    assert_eq!(
        upstream.upgrades_to("/websocket/commands?l=counter", 1),
        [bearer(OLD_TOKEN)]
    );
    for target in ["l=forged", "l=guessed", "l=bare"] {
        assert_eq!(
            upstream.upgrades_to(&format!("/websocket/commands?{target}"), 1),
            [None],
            "{target}"
        );
    }
    drop(server);
}
