use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::{OsStr, OsString},
    path::PathBuf,
};
use vosulay_cef_tools::{acquire, acquire_asdf, default_cache, verify_runtime};

#[cfg(target_os = "windows")]
use std::{path::Path, process::Command};

fn main() {
    if let Err(error) = run() {
        eprintln!("vosulay-cef: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        usage();
        return Ok(());
    };
    if command == OsStr::new("--help") || command == OsStr::new("-h") {
        usage();
        return Ok(());
    }
    let command = command.to_str().context("command must be valid UTF-8")?;
    if command == "package-windows" {
        const USAGE: &str =
            "usage: vosulay-cef package-windows OUTPUT_DIRECTORY [--client vosulay.dll]";
        let output = args.next().context(USAGE)?;
        let client = match (args.next(), args.next(), args.next()) {
            (None, _, _) => None,
            (Some(flag), Some(path), None) if flag == OsStr::new("--client") => {
                Some(PathBuf::from(path))
            }
            _ => bail!(USAGE),
        };
        return package_windows(PathBuf::from(output), client);
    }

    let mut options: Vec<(OsString, OsString)> = Vec::new();
    while let Some(key) = args.next() {
        if key == OsStr::new("--help") || key == OsStr::new("-h") {
            usage();
            return Ok(());
        }
        if !key.to_string_lossy().starts_with("--") {
            bail!("expected option, got {}", key.to_string_lossy());
        }
        let value = args.next().context("missing option value")?;
        let repeatable = key == OsStr::new("--exe") || key == OsStr::new("--overlay");
        if !repeatable && options.iter().any(|(existing, _)| existing == &key) {
            bail!("option supplied more than once: {}", key.to_string_lossy());
        }
        options.push((key, value));
    }
    let one = |name: &str| -> Result<&OsStr> {
        options
            .iter()
            .find(|(key, _)| key == OsStr::new(name))
            .map(|(_, value)| value.as_os_str())
            .with_context(|| format!("required option {name} missing"))
    };
    let allowed: &[&str] = match command {
        "acquire" => &["--platform", "--cache"],
        "acquire-asdf" => &["--cache"],
        "verify" => &["--platform", "--cef-path"],
        "package" => &[
            "--platform",
            "--cef-path",
            "--output",
            "--exe",
            "--overlay",
            "--client",
        ],
        _ => bail!("unknown command: {command}"),
    };
    for (key, _) in &options {
        let key = key.to_str().context("option name must be UTF-8")?;
        if !allowed.contains(&key) {
            bail!("unexpected option for {command}: {key}");
        }
    }

    match command {
        "acquire" => {
            let platform = one("--platform")?
                .to_str()
                .context("platform must be UTF-8")?;
            let cache = match option_path(&options, "--cache") {
                Some(path) => path,
                None => default_cache("cef")?,
            };
            let path = acquire(platform, &cache)?;
            println!("{}", path.canonicalize()?.display());
        }
        "acquire-asdf" => {
            let cache = match option_path(&options, "--cache") {
                Some(path) => path,
                None => default_cache("asdf")?,
            };
            let path = acquire_asdf(&cache)?;
            println!("{}", path.canonicalize()?.display());
        }
        "verify" => {
            let platform = one("--platform")?
                .to_str()
                .context("platform must be UTF-8")?;
            let path = PathBuf::from(one("--cef-path")?);
            verify_runtime(&path, platform)?;
            println!("{}", path.canonicalize()?.display());
        }
        "package" => {
            let platform = one("--platform")?
                .to_str()
                .context("platform must be UTF-8")?;
            let cef = PathBuf::from(one("--cef-path")?);
            let output = PathBuf::from(one("--output")?);
            let executables = options
                .iter()
                .filter(|(key, _)| key == OsStr::new("--exe"))
                .map(|(_, value)| PathBuf::from(value))
                .collect::<Vec<_>>();
            let overlays = options
                .iter()
                .filter(|(key, _)| key == OsStr::new("--overlay"))
                .map(|(_, value)| PathBuf::from(value))
                .collect::<Vec<_>>();
            let client = options
                .iter()
                .find(|(key, _)| key == OsStr::new("--client"))
                .map(|(_, value)| PathBuf::from(value));
            vosulay_cef_tools::package::package_runtime(
                platform,
                &cef,
                &output,
                &executables,
                &overlays,
                client.as_deref(),
            )?;
            println!("{}", output.canonicalize()?.display());
        }
        _ => unreachable!("command validated above"),
    }
    Ok(())
}

fn option_path(options: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    options
        .iter()
        .find(|(key, _)| key == OsStr::new(name))
        .map(|(_, value)| PathBuf::from(value))
}

fn usage() {
    eprintln!(
        "vosulay-cef acquire --platform PLATFORM [--cache PATH]\n\
        vosulay-cef acquire-asdf [--cache PATH]\n\
        vosulay-cef verify --platform PLATFORM --cef-path PATH\n\
        vosulay-cef package --platform PLATFORM --cef-path PATH --output PATH [--exe PATH]* [--overlay PATH]* [--client PATH]\n\
        vosulay-cef package-windows OUTPUT_DIRECTORY [--client vosulay.dll]"
    );
}

#[cfg(not(target_os = "windows"))]
fn package_windows(_output: PathBuf, _client: Option<PathBuf>) -> Result<()> {
    bail!("package-windows can only run on native Windows");
}

/// `client` packages an already built `vosulay.dll`, such as the copy
/// returned by code signing, instead of building one. Signing must happen
/// before packaging: the installer embeds the DLL and the package manifest
/// records its digest.
#[cfg(target_os = "windows")]
fn package_windows(output: PathBuf, client: Option<PathBuf>) -> Result<()> {
    use std::{fs, io::Write};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("locating repository root")?;
    let platform = "windows-x86_64";
    let cef = if let Some(value) = env::var_os("CEF_PATH") {
        let path = PathBuf::from(value);
        verify_runtime(&path, platform)
            .context("CEF_PATH is set but is not a verified pinned Windows CEF runtime")?;
        path.canonicalize()
            .context("canonicalizing verified CEF_PATH")?
    } else {
        acquire(platform, &default_cache("cef")?)?.canonicalize()?
    };
    let overlay = acquire_asdf(&default_cache("asdf")?)?;
    if let Some(env_file) = env::var_os("GITHUB_ENV") {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&env_file)
            .with_context(|| {
                format!("opening GITHUB_ENV {}", PathBuf::from(&env_file).display())
            })?;
        writeln!(file, "CEF_PATH={}", cef.display())?;
    }

    let client = if let Some(client) = client {
        if client.file_name() != Some(OsStr::new("vosulay.dll")) || !client.is_file() {
            bail!(
                "--client must be an existing file named vosulay.dll: {}",
                client.display()
            );
        }
        client
    } else {
        let target_dir = env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let target_dir = if target_dir.is_absolute() {
            target_dir
        } else {
            root.join(target_dir)
        };
        let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "build",
                "--release",
                "--lib",
                "--locked",
                "--target",
                "x86_64-pc-windows-msvc",
                "-p",
                "vosulay",
            ])
            .current_dir(root)
            .env("CEF_PATH", &cef)
            .stdout(std::io::stderr())
            .status()
            .context("running cargo build for Windows client")?;
        if !status.success() {
            bail!("Windows client cargo build failed with {status}");
        }
        let client = target_dir.join("x86_64-pc-windows-msvc/release/vosulay.dll");
        if !client.is_file() {
            bail!("Windows client DLL not found: {}", client.display());
        }
        client
    };

    let pins = vosulay_cef_tools::pins()?;
    let overlays = pins
        .asdf_overlay
        .assets
        .iter()
        .map(|asset| overlay.join(&asset.name))
        .collect::<Vec<_>>();
    let output = if output.is_absolute() {
        output
    } else {
        env::current_dir()?.join(output)
    };
    vosulay_cef_tools::package::package_runtime(
        platform,
        &cef,
        &output,
        &[],
        &overlays,
        Some(&client),
    )?;
    let archive = output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "vosulay-{}-windows-x86_64.zip",
            vosulay_cef_tools::PRODUCT_VERSION
        ));
    vosulay_cef_tools::package::write_zip(&archive, &output)?;
    println!("{}", archive.canonicalize()?.display());
    Ok(())
}
