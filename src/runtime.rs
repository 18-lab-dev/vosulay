#[cfg(target_os = "linux")]
mod clipboard;
mod control;
mod desktop_entry;
mod editor;
mod host;
mod instance;
#[cfg(target_os = "linux")]
mod launchers;
mod lifecycle;
mod resources;
#[cfg(target_os = "linux")]
mod signals;
mod tosu;

pub use host::run;
pub use resources::ProductResources;

use anyhow::{Context, Result, bail, ensure};
use std::path::PathBuf;
use url::Url;

struct Options {
    runtime: PathBuf,
    #[cfg(target_os = "linux")]
    overlay: PathBuf,
    #[cfg(target_os = "linux")]
    lookup: PathBuf,
    config: PathBuf,
    /// `--vosu-url` for this run; otherwise the configured setting applies.
    vosu_url: Option<Url>,
    fps_limit: Option<u32>,
    pids: Vec<u32>,
    process_names: Vec<String>,
    control: Option<control::Command>,
    server_only: bool,
}

impl Options {
    fn parse() -> Result<Option<Self>> {
        Self::parse_arguments(std::env::args().skip(1))
    }

    fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Option<Self>> {
        let executable = std::env::current_exe()?;
        let directory = executable
            .parent()
            .context("executable has no parent directory")?;
        #[cfg(target_os = "linux")]
        let packaged = directory != std::path::Path::new(env!("VOSULAY_CEF_RUNTIME"))
            && directory.join("libcef.so").is_file();
        #[cfg(target_os = "linux")]
        let runtime = if packaged {
            directory.to_owned()
        } else {
            PathBuf::from(env!("VOSULAY_CEF_RUNTIME"))
        };
        #[cfg(windows)]
        let runtime = directory.to_owned();
        let mut options = Self {
            #[cfg(target_os = "linux")]
            overlay: if packaged {
                runtime.join("libvosulay_overlay.so")
            } else {
                PathBuf::from(env!("VOSULAY_OVERLAY_LIBRARY"))
            },
            #[cfg(target_os = "linux")]
            lookup: if packaged {
                runtime.join("libvosulay_lookup.so")
            } else {
                PathBuf::from(env!("VOSULAY_LOOKUP_LIBRARY"))
            },
            runtime,
            config: if directory.join("portable").is_file()
                || directory.join("data/config.json").is_file()
            {
                directory.join("data/config.json")
            } else {
                crate::config::config_path()?
            },
            vosu_url: None,
            fps_limit: None,
            pids: Vec::new(),
            process_names: Vec::new(),
            control: None,
            server_only: false,
        };
        let mut benchmark_uncapped = false;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--help" | "-h" => {
                    println!(
                        "vosulay {}\n\nUsage: vosulay [options]\n  --vosu-url, -t URL    vosu or tosu-compatible backend for this run (default: Settings → Advanced → vosu URL)\n  --server-only        Serve remote overlays and editing without game attachment, tray or CEF\n  --toggle-edit        Ask the running instance to toggle in-game editing\n  --quit               Ask the running instance to save and exit gracefully\n  --fps-limit, -f FPS   auto/0 or a positive cap (default: display refresh divided down to 60-120)\n  --benchmark-uncapped  Disable Chromium pacing; use minimum positive OSR interval\n  --pid PID            Additional supported game process; repeatable\n  --process-name NAME  Additional supported game executable; repeatable\n  --config PATH        Configuration file (default user configuration directory)\n  --cef-dir PATH       Runtime package directory (default beside executable or build-time cache)\n  --version            Product version\n\n--toggle-edit and --quit are mutually exclusive and fail if vosulay is not running.\n--server-only rejects --pid, --process-name, --fps-limit and --benchmark-uncapped.\nToggle editing in game with Ctrl+Shift+Space by default; configure it in Settings → Shortcuts. Escape leaves edit mode.",
                        env!("CARGO_PKG_VERSION")
                    );
                    return Ok(None);
                }
                "--version" | "-v" => {
                    println!("vosulay {}", env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                "--vosu-url" | "-t" => {
                    options.vosu_url = Some(crate::config::parse_vosu_url(
                        &arguments.next().context("vosu URL is required")?,
                    )?);
                }
                "--toggle-edit" | "--quit" => {
                    ensure!(
                        options.control.is_none(),
                        "--toggle-edit and --quit are mutually exclusive"
                    );
                    options.control = Some(if argument == "--quit" {
                        control::Command::Quit
                    } else {
                        control::Command::ToggleEdit
                    });
                }
                "--server-only" => options.server_only = true,
                "--benchmark-uncapped" => benchmark_uncapped = true,
                "--fps-limit" | "-f" => {
                    let value = arguments.next().context("FPS limit is required")?;
                    options.fps_limit = Some(parse_fps_limit(&value)?);
                }
                "--pid" => {
                    let pid = arguments
                        .next()
                        .context("PID is required")?
                        .parse::<u32>()
                        .context("invalid PID")?;
                    ensure!(pid != 0, "PID must be positive");
                    options.pids.push(pid);
                }
                "--process-name" => {
                    let name = arguments.next().context("process name is required")?;
                    ensure!(
                        !name.is_empty() && !name.contains(['/', '\\']),
                        "process name must be an executable basename"
                    );
                    options.process_names.push(name);
                }
                "--config" => {
                    options.config =
                        PathBuf::from(arguments.next().context("configuration path is required")?)
                }
                "--cef-dir" => {
                    options.runtime =
                        PathBuf::from(arguments.next().context("CEF directory is required")?);
                    #[cfg(target_os = "linux")]
                    {
                        options.overlay = options.runtime.join("libvosulay_overlay.so");
                        options.lookup = options.runtime.join("libvosulay_lookup.so");
                    }
                }
                _ => bail!("unknown option {argument}; use --help"),
            }
        }
        ensure!(
            !options.server_only
                || (options.pids.is_empty()
                    && options.process_names.is_empty()
                    && options.fps_limit.is_none()
                    && !benchmark_uncapped),
            "--server-only cannot be combined with --pid, --process-name, --fps-limit or --benchmark-uncapped"
        );
        if benchmark_uncapped {
            // CEF has no unlimited OSR sentinel: one microsecond is its smallest
            // positive interval. This benchmark-only ceiling is not a prod cap.
            options.fps_limit = Some(crate::config::MAX_FPS_LIMIT);
        }
        Ok(Some(options))
    }
}

/// Runs `--toggle-edit` / `--quit` against the running instance and returns the exit
/// code, or `None` for any other invocation. It needs no CEF runtime, so Linux runs it
/// before re-executing from the retained runtime directory (which hashes the binary).
pub fn control_command() -> Result<Option<i32>> {
    if !std::env::args().any(|argument| argument == "--toggle-edit" || argument == "--quit") {
        return Ok(None);
    }
    let Some(options) = Options::parse()? else {
        return Ok(Some(0));
    };
    let Some(command) = options.control else {
        return Ok(None);
    };
    if instance::InstanceGuard::acquire()?.is_some() {
        eprintln!("vosulay is not running");
        return Ok(Some(1));
    }
    Ok(Some(match control::send(command) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }))
}

fn parse_fps_limit(value: &str) -> Result<u32> {
    if value == "auto" || value == "0" {
        return Ok(0);
    }
    let fps = value
        .parse::<u32>()
        .context("FPS limit must be auto, 0, or a positive integer")?;
    ensure!(
        (1..=crate::config::MAX_FPS_LIMIT).contains(&fps),
        "FPS limit must be between 1 and {}",
        crate::config::MAX_FPS_LIMIT
    );
    Ok(fps)
}

/// Automatic mode renders at the display refresh divided by the smallest whole number that brings
/// it to 120 fps or less (360 -> 120, 240 -> 120, 144 -> 72), so frames stay evenly spaced on the
/// display. Rendering at a high refresh such as 360 Hz made CEF's render, readback and upload
/// enough extra GPU load that, with OBS capturing the game, the game's own frame rate dropped
/// (720 -> 400 fps on an RTX 5090); 60-180 cost nothing measurable.
fn automatic_rate(refresh_hz: f64) -> f64 {
    const MAX_AUTOMATIC_FPS: f64 = 120.0;
    refresh_hz / (refresh_hz / MAX_AUTOMATIC_FPS).ceil().max(1.0)
}

fn frame_rate(cli: Option<u32>, configured: u32, refresh_hz: f64) -> f64 {
    let limit = cli.unwrap_or(configured);
    if limit != 0 {
        f64::from(limit.min(crate::config::MAX_FPS_LIMIT))
    } else if refresh_hz.is_finite() && refresh_hz > 0.0 {
        automatic_rate(refresh_hz)
    } else {
        60.0
    }
}

/// Editing is an interaction surface, not a high-refresh game counter. Keep the
/// resolved game limit intact so leaving edit mode restores it immediately.
pub fn presentation_frame_rate(resolved_fps: f64, editing: bool) -> u32 {
    let fps = resolved_fps.ceil() as u32;
    if editing { fps.min(30) } else { fps }
}

fn receive(browser: &vosulay_browser::OsrBrowser, envelope: &serde_json::Value) -> Result<()> {
    browser.evaluate(&format!(
        "window.__VOSULAY_RECEIVE__({});",
        serde_json::to_string(envelope)?
    ));
    Ok(())
}

/// Entry point called only by the matching, packaged CEF sandbox bootstrap.
///
/// # Safety
/// `sandbox_info` must be the live context supplied by that bootstrap.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RunWinMain(
    _instance: *mut std::ffi::c_void,
    _command_line: *mut u16,
    _show: i32,
    sandbox_info: *mut u8,
    _version_info: *mut std::ffi::c_void,
) -> i32 {
    // The pinned CEF bootstrap owns the main thread and sandbox context.
    match unsafe { run(sandbox_info) } {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            // A failed bounded shutdown must not unload Rust while a native
            // dialog/worker remains live. Process exit also releases game input.
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod fps_tests {
    use super::*;
    #[test]
    fn editor_cap_preserves_lower_limits_and_restores_game_rate() {
        assert_eq!(presentation_frame_rate(120.0, false), 120);
        assert_eq!(presentation_frame_rate(120.0, true), 30);
        assert_eq!(presentation_frame_rate(20.0, true), 20);
        assert_eq!(presentation_frame_rate(59.94, false), 60);
        assert_eq!(presentation_frame_rate(120.0, false), 120);
    }
    #[test]
    fn control_and_server_only_argument_conflicts() {
        for arguments in [
            vec!["--toggle-edit", "--quit"],
            vec!["--server-only", "--pid", "123"],
            vec!["--server-only", "--process-name", "osu!"],
            vec!["--server-only", "--fps-limit", "auto"],
            vec!["--server-only", "--benchmark-uncapped"],
        ] {
            assert!(Options::parse_arguments(arguments.into_iter().map(str::to_owned)).is_err());
        }
        let options = Options::parse_arguments(
            [
                "--server-only",
                "--vosu-url",
                "http://localhost:24050",
                "--config",
                "example.json",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap()
        .unwrap();
        assert!(options.server_only);
        assert_eq!(options.config, PathBuf::from("example.json"));
        assert!(options.vosu_url.is_some());
    }

    #[test]
    fn explicit_auto_overrides_persisted_fixed_rate_and_tracks_monitor_changes() {
        assert_eq!(frame_rate(None, 144, 59.94), 144.0);
        assert_eq!(frame_rate(Some(0), 144, 59.94), 59.94);
        assert_eq!(frame_rate(Some(0), 144, 240.0), 120.0);
        assert_eq!(frame_rate(Some(120), 144, 240.0), 120.0);
        assert_eq!(frame_rate(None, 0, 240.0), 120.0);
        assert_eq!(frame_rate(None, 0, f64::NAN), 60.0);
        // An explicit cap is never divided, however high.
        assert_eq!(frame_rate(Some(360), 0, 360.0), 360.0);
    }

    #[test]
    fn automatic_rate_divides_the_refresh_into_60_to_120_fps() {
        for (refresh, expected) in [
            (360.0, 120.0),
            (240.0, 120.0),
            (180.0, 90.0),
            (165.0, 82.5),
            (144.0, 72.0),
            (121.0, 60.5),
            (120.0, 120.0),
            (100.0, 100.0),
            (75.0, 75.0),
            (59.94, 59.94),
            (30.0, 30.0),
        ] {
            assert_eq!(frame_rate(None, 0, refresh), expected, "{refresh} Hz");
        }
        // Fractional refresh rates divide by the same whole number as their rounded value.
        assert!((frame_rate(None, 0, 359.97) - 119.99).abs() < 1e-9);
    }

    #[test]
    fn caps_with_a_zero_cef_interval_are_rejected() {
        assert_eq!(parse_fps_limit("auto").unwrap(), 0);
        assert_eq!(
            parse_fps_limit("1000000").unwrap(),
            crate::config::MAX_FPS_LIMIT
        );
        for value in ["1000001", "-1", "1.5", "", "unlimited"] {
            assert!(parse_fps_limit(value).is_err(), "{value}");
        }
    }
}
