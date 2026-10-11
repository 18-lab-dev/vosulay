//! Builds the shipped asdf-overlay DLLs from the pinned upstream source.
//!
//! Upstream's release DLLs are not used: nothing ties those binaries to the
//! source vosulay publishes for the LGPL components linked into them. Building
//! here from the same archive that goes into the third-party source bundle, with
//! hash-pinned Frida Gum devkits, makes that correspondence part of the build.

use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use crate::{AsdfPin, acquire_file, cache_lock, extract_tar, pins, regular_file, sha256};

/// The toolchain that builds vosulay also builds asdf-overlay. Upstream's
/// `rust-toolchain.toml` names targets but no channel, and its own releases
/// used nightly; the stable route is the one verified for vosulay.
const TOOLCHAIN: &str = include_str!("../../../rust-toolchain.toml");

fn toolchain_channel() -> Result<&'static str> {
    TOOLCHAIN
        .lines()
        .find_map(|line| line.trim().strip_prefix("channel = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .context("rust-toolchain.toml has no channel")
}

fn source_root(pin: &AsdfPin) -> String {
    format!("asdf-overlay-{}", pin.source.commit)
}

/// Build both shipped asdf-overlay DLLs and return the directory holding them
/// under their packaged names.
pub fn build_asdf(cache: &Path) -> Result<PathBuf> {
    let pins = pins()?;
    let pin = &pins.asdf_overlay;
    let _lock = cache_lock(cache)?;
    let root = cache.join(format!("{}-{}", pin.version, &pin.source.commit[..12]));
    fs::create_dir_all(&root)?;
    ensure!(
        fs::symlink_metadata(&root)?.file_type().is_dir(),
        "unsafe asdf-overlay build cache directory"
    );

    let archive = root.join(&pin.source.name);
    acquire_file(
        &archive,
        &pin.source.url,
        pin.source.size,
        None,
        &pin.source.sha256,
    )?;
    // frida-build fetches `<cdn>/<version>/<devkit>`; this tree is that layout.
    let cdn = root.join("cdn");
    let devkits = cdn.join(&pin.frida_version);
    fs::create_dir_all(&devkits)?;
    for devkit in &pin.frida_devkits {
        acquire_file(
            &devkits.join(&devkit.name),
            &devkit.url,
            devkit.size,
            None,
            &devkit.sha256,
        )?;
    }

    let source = root.join(source_root(pin));
    if fs::symlink_metadata(&source).is_err() {
        let staging = tempfile::Builder::new()
            .prefix(".asdf-extract-")
            .tempdir_in(&root)?;
        extract_tar(
            GzDecoder::new(regular_file(&archive)?),
            staging.path(),
            &source_root(pin),
        )?;
        fs::rename(staging.path().join(source_root(pin)), &source)
            .context("cannot publish extracted asdf-overlay source")?;
    }

    let server = DevkitServer::start(&cdn, pin)?;
    let target_dir = root.join("target");
    let channel = toolchain_channel()?;
    for dll in &pin.dlls {
        eprintln!(
            "Building {} from asdf-overlay {} for {} with Rust {channel}",
            dll.name, pin.source.commit, dll.target
        );
        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "--locked",
                "-p",
                "asdf-overlay-dll",
                "--target",
                &dll.target,
            ])
            .current_dir(&source)
            .env("RUSTUP_TOOLCHAIN", channel)
            .env("CARGO_TARGET_DIR", &target_dir)
            .env("FRIDA_DOWNLOAD_CDN", server.url())
            .env_remove("CARGO")
            .env_remove("RUSTC")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .stdout(std::io::stderr())
            .status()
            .context("running cargo build for asdf-overlay")?;
        ensure!(
            status.success(),
            "asdf-overlay build for {} failed with {status}",
            dll.target
        );
    }
    let fetched = server.stop();
    for path in &fetched {
        eprintln!("Served pinned Frida devkit to the build: {path}");
    }

    let output = root.join("dlls");
    if output.exists() {
        fs::remove_dir_all(&output)?;
    }
    fs::create_dir(&output)?;
    for dll in &pin.dlls {
        let built = target_dir
            .join(&dll.target)
            .join("release")
            .join("asdf_overlay_dll.dll");
        let destination = output.join(&dll.name);
        fs::copy(&built, &destination)
            .with_context(|| format!("asdf-overlay DLL not found: {}", built.display()))?;
        eprintln!("{}  {}", sha256(&destination)?, dll.name);
    }
    Ok(output.canonicalize()?)
}

/// Loopback HTTP server that offers only the hash-verified devkits, so the
/// unmodified upstream build script links exactly the pinned Gum archives.
struct DevkitServer {
    url: String,
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    fetched: Arc<Mutex<Vec<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl DevkitServer {
    fn start(cdn: &Path, pin: &AsdfPin) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let routes: Vec<(String, PathBuf)> = pin
            .frida_devkits
            .iter()
            .map(|devkit| {
                (
                    format!("/{}/{}", pin.frida_version, devkit.name),
                    cdn.join(&pin.frida_version).join(&devkit.name),
                )
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let stop = stop.clone();
            let fetched = fetched.clone();
            thread::Builder::new()
                .name("frida-devkit-server".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        if let Ok(stream) = stream
                            && let Err(error) = serve(stream, &routes, &fetched)
                        {
                            eprintln!("Frida devkit server request failed: {error:#}");
                        }
                    }
                })?
        };
        Ok(Self {
            url: format!("http://{address}"),
            address,
            stop,
            fetched,
            thread: Some(thread),
        })
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn stop(mut self) -> Vec<String> {
        self.shutdown();
        std::mem::take(&mut *self.fetched.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking accept so the thread observes the stop flag.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DevkitServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &[(String, PathBuf)],
    fetched: &Mutex<Vec<String>>,
) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    let mut parts = request.split_whitespace();
    let (Some("GET"), Some(path)) = (parts.next(), parts.next()) else {
        stream.write_all(
            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    };
    let Some((_, file)) = routes.iter().find(|(route, _)| route == path) else {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        bail!("unexpected devkit request {path}");
    };
    let body = fs::read(file)?;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-xz\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Write);
    fetched
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(path.to_owned());
    Ok(())
}
