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
    ] {
        let path = root.join(folder);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command.env("XDG_DATA_DIRS", root.join("system-data"));
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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    write!(stream, "GET {route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n").unwrap();
    if let Some(token) = token {
        write!(stream, "X-Vosu-Tool-Token: {token}\r\n").unwrap();
    }
    write!(stream, "\r\n").unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).unwrap();
    result
}
#[test]
fn readiness_token_eof_exit_conflict_and_launcher_suppression() {
    let root = tempfile::tempdir().unwrap();
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
    assert!(request(port, "/v1/manage/state", Some(&"a".repeat(64))).contains("\"config\""));
    let duplicate = command(root.path()).arg("--server-only").output().unwrap();
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already running"));
    assert!(
        !root
            .path()
            .join("data/applications/com.vosu.vosulay.desktop")
            .exists()
    );
    drop(child.stdin.take());
    assert_eq!(wait(&mut child).code(), Some(0));
    // A user-requested quit takes the same graceful host exit path as tray Exit.
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
    let quit = command(root.path()).arg("--quit").output().unwrap();
    assert!(quit.status.success());
    assert_eq!(wait(&mut child).code(), Some(0));
}
