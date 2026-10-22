fn main() {
    #[cfg(target_os = "linux")]
    {
        // Keybind-driven commands skip the retained-runtime re-exec, which hashes the binary.
        match vosulay::runtime::control_command() {
            Ok(Some(code)) => std::process::exit(code),
            Ok(None) => {}
            Err(error) => {
                eprintln!("Vosulay: {error:#}");
                std::process::exit(1);
            }
        }
        if let Err(error) = launch_cached_runtime() {
            eprintln!("Vosulay: {error:#}");
            std::process::exit(1);
        }
    }
    // Linux dispatches CEF subprocesses through this executable. The Windows
    // package instead invokes RunWinMain through CEF's sandbox bootstrap.
    match unsafe { vosulay::runtime::run(std::ptr::null_mut()) } {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Vosulay: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn launch_cached_runtime() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        env, fs, io::ErrorKind, os::unix::process::CommandExt, path::Path, process::Command,
    };
    use vosulay_cef_tools::sha256;
    if std::env::var("VOSU_TOOL_PROTOCOL").as_deref() == Ok("1")
        && std::env::args().any(|argument| argument == "--server-only")
    {
        return Ok(());
    }

    let executable = env::current_exe()?;
    let directory = executable
        .parent()
        .context("executable has no parent directory")?;
    let runtime = Path::new(env!("VOSULAY_CEF_RUNTIME"));
    if directory == runtime || directory.join("libcef.so").is_file() {
        return Ok(());
    }
    // CEF loads ICU and GPU assets beside the executable before settings can
    // redirect resources. RPATH alone cannot make a Cargo-installed binary run.
    let digest = sha256(&executable)?;
    let cached = runtime.join(format!("vosulay-{digest}"));
    if !cached.exists() {
        match fs::hard_link(&executable, &cached) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == ErrorKind::CrossesDevices => {
                let temporary = tempfile::NamedTempFile::new_in(runtime)?;
                fs::copy(&executable, temporary.path())?;
                match temporary.persist_noclobber(&cached) {
                    Ok(_) => {}
                    Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        sha256(&cached)? == digest,
        "cached executable digest mismatch: {}",
        cached.display()
    );
    Err(Command::new(cached)
        .env("VOSULAY_LAUNCHER_EXECUTABLE", &executable)
        .args(env::args_os().skip(1))
        .exec()
        .into())
}
