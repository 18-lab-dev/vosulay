//! Windowed, hardware-accelerated Chromium control using the exact pinned CEF.
//! This is a benchmark browser, not a second production runtime.

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, ensure};
    use cef::{args::Args, *};
    use std::{io::Write, path::PathBuf};

    // cef_version_info.h is not generated into the Rust binding API.
    #[link(name = "cef")]
    unsafe extern "C" {
        fn cef_version_info(entry: i32) -> i32;
    }

    wrap_app! {
        struct WindowedApp {}
        impl App {
            fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
                if let Some(command_line) = command_line {
                    for switch in ["no-first-run", "no-startup-window", "disable-frame-rate-limit", "disable-gpu-vsync", "disable-stack-profiler"] {
                        command_line.append_switch(Some(&switch.into()));
                    }
                    command_line.append_switch_with_value(Some(&"disable-features".into()), Some(&"SpareRendererForSitePerProcess".into()));
                }
            }
        }
    }
    wrap_client! {
        struct WindowedClient {}
        impl Client {}
    }
    wrap_browser_view_delegate! {
        struct ReferenceView {}
        impl ViewDelegate {}
        impl BrowserViewDelegate {
            fn browser_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
        }
    }
    wrap_window_delegate! {
        struct ReferenceWindow {
            browser_view: BrowserView,
        }
        impl ViewDelegate {
            fn preferred_size(&self, _view: Option<&mut View>) -> Size {
                Size { width: 640, height: 480 }
            }
        }
        impl PanelDelegate {}
        impl WindowDelegate {
            fn on_window_created(&self, window: Option<&mut Window>) {
                if let Some(window) = window {
                    window.set_title(Some(&"Vosulay autoresearch browser".into()));
                    window.set_to_fill_layout();
                    window.add_child_view(Some(&mut View::from(&self.browser_view)));
                    window.show();
                }
            }
            fn on_window_destroyed(&self, _window: Option<&mut Window>) { quit_message_loop(); }
            fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
                Rect { x: 0, y: 0, width: 640, height: 480 }
            }
            fn is_frameless(&self, _window: Option<&mut Window>) -> i32 { 1 }
            fn can_resize(&self, _window: Option<&mut Window>) -> i32 { 0 }
            fn window_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
        }
    }

    fn argument(name: &str) -> Result<String> {
        std::env::args()
            .find_map(|argument| argument.strip_prefix(name).map(str::to_owned))
            .with_context(|| format!("missing {name}VALUE"))
    }

    pub fn run() -> Result<i32> {
        ensure!(
            !api_hash(cef::sys::CEF_API_VERSION, 0).is_null(),
            "CEF rejected the pinned API"
        );
        let args = Args::new();
        let mut app = WindowedApp::new();
        let code = execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        if code >= 0 {
            return Ok(code);
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
                std::ptr::null_mut()
            ) == 1,
            "windowed CEF initialization failed"
        );
        // Frameless Views avoids the native host's one-pixel client inset.
        // Both the window and browser use Alloy with no browser chrome or OSR.
        let mut client = WindowedClient::new();
        let mut view_delegate = ReferenceView::new();
        let browser_view = browser_view_create(
            Some(&mut client),
            Some(&url.as_str().into()),
            Some(&BrowserSettings::default()),
            None,
            None,
            Some(&mut view_delegate),
        )
        .context("windowed browser view creation failed")?;
        let mut window_delegate = ReferenceWindow::new(browser_view.clone());
        let window = window_create_top_level(Some(&mut window_delegate))
            .context("windowed browser window creation failed")?;
        let browser = browser_view
            .browser()
            .context("windowed browser creation failed")?;
        let handle = browser
            .host()
            .context("windowed browser has no host")?
            .window_handle();
        println!("BROWSER_WINDOW={handle}");
        // Query the loaded engine, not a reduced user-agent or manifest alone.
        println!(
            "CHROMIUM_VERSION={}.{}.{}.{}",
            unsafe { cef_version_info(4) },
            unsafe { cef_version_info(5) },
            unsafe { cef_version_info(6) },
            unsafe { cef_version_info(7) }
        );
        std::io::stdout().flush()?;
        run_message_loop();
        drop(window);
        drop(window_delegate);
        drop(browser);
        drop(browser_view);
        drop(view_delegate);
        drop(client);
        drop(app);
        shutdown();
        Ok(0)
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    match linux::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("autoresearch-browser: {error:#}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("the private-display browser control currently requires Linux");
        std::process::exit(1);
    }
}
