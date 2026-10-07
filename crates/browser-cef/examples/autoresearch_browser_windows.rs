#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app {
    use anyhow::{Context, Result, ensure};
    use cef::{args::Args, *};
    use std::{io::Write, path::PathBuf};

    #[link(name = "libcef")]
    unsafe extern "C" {
        fn cef_version_info(entry: i32) -> i32;
    }

    wrap_app! {
        struct BrowserApp {}
        impl App {
            fn on_before_command_line_processing(
                &self,
                _process_type: Option<&CefString>,
                command_line: Option<&mut CommandLine>,
            ) {
                if let Some(command_line) = command_line {
                    for switch in [
                        "no-first-run",
                        "no-startup-window",
                        // Match the production host token; keep CEF's subprocess sandbox.
                        "do-not-de-elevate",
                        "disable-frame-rate-limit",
                        "disable-gpu-vsync",
                        "disable-stack-profiler",
                    ] {
                        command_line.append_switch(Some(&switch.into()));
                    }
                    command_line.append_switch_with_value(
                        Some(&"disable-features".into()),
                        Some(&"SpareRendererForSitePerProcess".into()),
                    );
                }
            }
        }
    }
    wrap_life_span_handler! {
        struct Lifetime {}
        impl LifeSpanHandler {
            fn on_before_close(&self, _browser: Option<&mut Browser>) {
                quit_message_loop();
            }
        }
    }
    wrap_client! {
        struct BrowserClient {}
        impl Client {
            fn life_span_handler(&self) -> Option<LifeSpanHandler> {
                Some(Lifetime::new())
            }
        }
    }

    fn argument(prefix: &str) -> Result<String> {
        std::env::args()
            .find_map(|arg| arg.strip_prefix(prefix).map(str::to_owned))
            .with_context(|| format!("missing {prefix}VALUE"))
    }

    /// # Safety
    /// The pinned CEF bootstrap owns `sandbox_info` for the process lifetime.
    pub unsafe fn run(sandbox_info: *mut u8) -> Result<i32> {
        ensure!(
            !sandbox_info.is_null(),
            "CEF bootstrap supplied no live sandbox_info"
        );
        ensure!(
            !api_hash(cef::sys::CEF_API_VERSION, 0).is_null(),
            "CEF rejected the pinned API"
        );
        let args = Args::new();
        let mut app = BrowserApp::new();
        let subprocess = execute_process(Some(args.as_main_args()), Some(&mut app), sandbox_info);
        if subprocess >= 0 {
            return Ok(subprocess);
        }

        let runtime = PathBuf::from(argument("--cef-dir=")?).canonicalize()?;
        let cache = PathBuf::from(argument("--cache=")?);
        std::fs::create_dir_all(&cache)?;
        let cache = cache.canonicalize()?;
        let url = argument("--url=")?;
        let settings = Settings {
            root_cache_path: cache.to_string_lossy().as_ref().into(),
            cache_path: cache.to_string_lossy().as_ref().into(),
            resources_dir_path: runtime.to_string_lossy().as_ref().into(),
            locales_dir_path: runtime.join("locales").to_string_lossy().as_ref().into(),
            log_file: cache.join("cef.log").to_string_lossy().as_ref().into(),
            ..Default::default()
        };
        ensure!(
            initialize(
                Some(args.as_main_args()),
                Some(&settings),
                Some(&mut app),
                sandbox_info,
            ) == 1,
            "windowed CEF initialization failed (code {})",
            get_exit_code()
        );
        let window = WindowInfo {
            window_name: "Vosulay autoresearch Alloy reference".into(),
            bounds: Rect {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            },
            style: 0x9000_0000, // Visible, borderless popup; exactly 640x480 client pixels.
            runtime_style: RuntimeStyle::ALLOY,
            ..Default::default()
        };
        let mut client = BrowserClient::new();
        let browser = browser_host_create_browser_sync(
            Some(&window),
            Some(&mut client),
            Some(&url.as_str().into()),
            Some(&BrowserSettings::default()),
            None,
            None,
        )
        .context("windowed browser creation failed")?;
        let handle = browser
            .host()
            .context("browser has no host")?
            .window_handle();
        let version = format!(
            "{}.{}.{}.{}",
            unsafe { cef_version_info(4) },
            unsafe { cef_version_info(5) },
            unsafe { cef_version_info(6) },
            unsafe { cef_version_info(7) },
        );
        std::fs::write(cache.join("chromium-version.txt"), &version)?;
        println!("BROWSER_WINDOW={handle:?}\nCHROMIUM_VERSION={version}");
        std::io::stdout().flush()?;
        run_message_loop();
        drop(browser);
        drop(client);
        drop(app);
        shutdown();
        Ok(0)
    }
}

/// # Safety
/// Called only by the matching pinned CEF bootstrap with its live sandbox context.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RunWinMain(
    _instance: *mut std::ffi::c_void,
    _command_line: *mut u16,
    _show: i32,
    sandbox_info: *mut u8,
    _version_info: *mut std::ffi::c_void,
) -> i32 {
    match unsafe { app::run(sandbox_info) } {
        Ok(code) => code,
        Err(error) => {
            eprintln!("autoresearch-browser-windows: {error:#}");
            1
        }
    }
}
