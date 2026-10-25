#![cfg(all(target_os = "linux", feature = "cef-host"))]
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vosulay"));
    for (name, folder) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_DATA_DIRS", "system-data"),
        ("XDG_CONFIG_DIRS", "system-config"),
    ] {
        let path = root.join(folder);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command.env(
        "VOSULAY_ENDPOINT",
        format!("@vosulay-tool-test-{}", std::process::id()),
    );
    command
        .env("VOSU_TOOL_PROTOCOL", "1")
        .env("VOSU_TOOL_TOKEN", "a".repeat(64))
        .env("VOSU_URL", "http://127.0.0.1:9")
        .env("VOSU_TOOL_DATA_DIR", root.join("tool-data"))
        .env("VOSU_TOOL_CACHE_DIR", root.join("tool-cache"));
    command
}
fn wait(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("tool failed to exit within five seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn request(port: u16, route: &str, token: Option<&str>) -> String {
    request_with_headers(port, route, token, "")
}
fn request_with_headers(port: u16, route: &str, token: Option<&str>, headers: &str) -> String {
    request_with_authority(port, &format!("127.0.0.1:{port}"), route, token, headers)
}
fn request_with_authority(
    port: u16,
    authority: &str,
    route: &str,
    token: Option<&str>,
    headers: &str,
) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    write!(stream, "GET {route} HTTP/1.1\r\nHost: {authority}\r\n").unwrap();
    if let Some(token) = token {
        write!(stream, "X-Vosu-Tool-Token: {token}\r\n").unwrap();
    }
    write!(stream, "{headers}\r\n").unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).unwrap();
    result
}
#[test]
fn readiness_token_eof_exit_conflict_and_launcher_suppression() {
    let root = tempfile::tempdir().unwrap();
    use base64::Engine;
    let local = root.path().join("local");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("style.css"), "body{color:red}").unwrap();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(local.to_str().unwrap());
    let local_token = "1".repeat(32);
    let mut config = vosulay::config::AppConfig::default();
    config.profiles[0].overlays.push(serde_json::from_value(serde_json::json!({
        "counter_id":"local","url":format!("vosulay-overlay://{local_token}/{encoded}/index.html"),
        "top":0,"left":0,"width":800,"height":100,"scale":1,"z_index":0
    })).unwrap());
    vosulay::config::save_to_path(&config, &root.path().join("tool-data/config.json")).unwrap();
    let mut child = command(root.path())
        .arg("--server-only")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(output).read_line(&mut line).unwrap();
        let _ = send.send(line);
    });
    let line = receive.recv_timeout(Duration::from_secs(20)).unwrap();
    let port: u16 = line
        .strip_prefix("vosu-tool-ready ")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(request(port, "/healthz", None).starts_with("HTTP/1.1 401"));
    assert!(request(port, "/healthz", Some("wrong")).starts_with("HTTP/1.1 401"));
    assert!(request(port, "/healthz", Some(&"a".repeat(64))).starts_with("HTTP/1.1 200"));
    assert!(request(port, "/v1/manage/state", None).starts_with("HTTP/1.1 401"));
    let first_state = request(port, "/v1/manage/state", Some(&"a".repeat(64)));
    let first_state: serde_json::Value =
        serde_json::from_str(first_state.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(first_state["config"].is_object());
    assert_eq!(
        first_state["routing"]["generation"].as_str().unwrap().len(),
        32
    );
    let view = url::Url::parse(first_state["routing"]["view"].as_str().unwrap()).unwrap();
    let broker_port = view.port().unwrap();
    let opaque = "Origin: null\r\nSec-Fetch-Site: cross-site\r\n";
    let authority = &view[url::Position::BeforeHost..url::Position::AfterPort];
    let resource_path = format!("/local/{local_token}/style.css");
    let resource = request_with_authority(broker_port, authority, &resource_path, None, opaque);
    assert!(resource.starts_with("HTTP/1.1 200"));
    assert!(resource.contains("Access-Control-Allow-Origin: null\r\n"));
    assert!(resource.contains("Cross-Origin-Resource-Policy: cross-origin\r\n"));
    assert!(resource.contains("Referrer-Policy: no-referrer\r\n"));
    assert!(
        !resource
            .to_lowercase()
            .contains("access-control-allow-credentials:")
    );
    for path in [
        "/overlay",
        "/__vosulay/view-state",
        "/__vosulay/backend.js",
        "/__vosulay/manage/state",
        "/v1/manage/state",
        "/config.json",
    ] {
        for host in [authority, &format!("127.0.0.1:{broker_port}")] {
            let denied = request_with_authority(broker_port, host, path, None, opaque);
            assert!(denied.starts_with("HTTP/1.1 403"), "{path}");
            assert!(
                !denied
                    .to_lowercase()
                    .contains("access-control-allow-origin:")
            );
        }
    }
    for path in [
        &resource_path,
        "/local/22222222222222222222222222222222/style.css",
        "/counter.js",
    ] {
        for host in [
            format!("127.0.0.1:{broker_port}"),
            format!("guessed.preview.localhost:{broker_port}"),
            format!(
                "{}:{}",
                view.host_str().unwrap(),
                broker_port.saturating_add(1)
            ),
        ] {
            assert!(
                request_with_authority(broker_port, &host, path, None, opaque)
                    .starts_with("HTTP/1.1 403")
            );
        }
    }
    assert!(
        request_with_authority(
            broker_port,
            authority,
            &resource_path,
            None,
            "Origin: http://evil.example\r\nSec-Fetch-Site: cross-site\r\n"
        )
        .starts_with("HTTP/1.1 403")
    );
    let public_view = request(port, "/v1/view", Some(&"a".repeat(64)));
    let public_state = request(port, "/v1/view/state", Some(&"a".repeat(64)));
    assert!(!public_view.contains(".preview.localhost"));
    assert!(!public_state.contains(".preview.localhost"));
    let mut socket = TcpStream::connect(("127.0.0.1", broker_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    write!(socket, "GET /ws HTTP/1.1\r\nHost: {authority}\r\nOrigin: null\r\nSec-Fetch-Site: cross-site\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").unwrap();
    let mut status = String::new();
    BufReader::new(socket).read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
    let duplicate = command(root.path()).arg("--server-only").output().unwrap();
    assert_eq!(duplicate.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("vosu owns this instance"));
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        !root
            .path()
            .join("data/applications/com.vosu.vosulay.desktop")
            .exists()
    );
    drop(child.stdin.take());
    assert_eq!(wait(&mut child).code(), Some(0));
    // Legacy local Quit cannot bypass vosu's lifecycle ownership.
    let mut child = command(root.path())
        .arg("--server-only")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let replacement_port = line
        .strip_prefix("vosu-tool-ready ")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let replacement = request(replacement_port, "/v1/manage/state", Some(&"a".repeat(64)));
    let replacement: serde_json::Value =
        serde_json::from_str(replacement.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_ne!(
        first_state["routing"]["generation"],
        replacement["routing"]["generation"]
    );
    let replacement_view =
        url::Url::parse(replacement["routing"]["view"].as_str().unwrap()).unwrap();
    assert!(
        replacement_view
            .host_str()
            .unwrap()
            .ends_with(".preview.localhost")
    );
    assert!(replacement_view.host_str() != view.host_str());
    let quit = command(root.path()).arg("--quit").output().unwrap();
    assert!(!quit.status.success());
    assert!(String::from_utf8_lossy(&quit.stderr).contains("vosu owns this instance"));
    assert!(child.try_wait().unwrap().is_none());
    drop(child.stdin.take());
    assert_eq!(wait(&mut child).code(), Some(0));
}
