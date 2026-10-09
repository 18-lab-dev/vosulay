use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use vosulay_browser::{
    BrowserOptions, CefRuntime, RuntimeOptions, execute_subprocess,
    scheme::{MANAGER_URL, Resource, ResourceRoute, ResourceService},
};
use vosulay_frame::{BrowserFrame, BrowserFrameBackend};

struct SmokeResources {
    page: Arc<[u8]>,
    counter: Arc<[u8]>,
}
impl ResourceService for SmokeResources {
    fn load(&self, route: ResourceRoute<'_>) -> Option<Resource> {
        let ResourceRoute::Manager { path } = route else {
            return None;
        };
        let bytes = match path {
            "/index.html" => self.page.clone(),
            "/counter.html" => self.counter.clone(),
            _ => return None,
        };
        Some(Resource {
            bytes,
            mime_type: "text/html".to_owned(),
            status: 200,
        })
    }
}

#[derive(Default)]
struct Proof {
    frames: u64,
    transparent: bool,
    red: bool,
    iframe_pixels: bool,
    resized: bool,
    image: Vec<u8>,
}
struct Observer(Rc<RefCell<Proof>>);
impl BrowserFrameBackend for Observer {
    fn resize(&mut self, _: u32, _: u32, _: f32) -> Result<()> {
        Ok(())
    }
    fn set_visible(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()> {
        let (pixels, width, height) = match frame {
            BrowserFrame::Bgra {
                pixels,
                width,
                height,
                ..
            } => (pixels, width, height),
            #[cfg(windows)]
            BrowserFrame::D3d11 { .. } => anyhow::bail!("smoke expects CPU paint"),
        };
        let mut proof = self.0.borrow_mut();
        proof.frames += 1;
        let pixel = |x: u32, y: u32| -> &[u8] {
            let offset = ((y * width + x) * 4) as usize;
            &pixels[offset..offset + 4]
        };
        if width >= 256 && height >= 128 {
            proof.transparent |= pixel(0, 0)[3] == 0;
            proof.red |= pixel(32, 32) == [0, 0, 255, 255];
            proof.iframe_pixels |= pixel(128, 32) == [0, 255, 0, 255];
            if width == 384
                && height == 192
                && pixel(32, 32) == [0, 0, 255, 255]
                && pixel(128, 32) == [0, 255, 0, 255]
            {
                proof.resized = true;
                if proof.image.is_empty() {
                    proof
                        .image
                        .extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
                    for pixel in pixels.chunks_exact(4) {
                        proof
                            .image
                            .extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                    }
                }
            }
        }
        Ok(())
    }
}

/// # Safety
/// Windows callers must supply CEF's live bootstrap sandbox context; Linux
/// callers pass null. This function owns CEF initialization on the main thread.
pub unsafe fn run(sandbox_info: *mut u8) -> Result<()> {
    if let Some(code) = unsafe { execute_subprocess(sandbox_info) } {
        std::process::exit(code);
    }
    let arguments: Vec<String> = std::env::args().collect();
    let value = |key: &str| {
        arguments
            .windows(2)
            .find(|pair| pair[0] == key)
            .map(|pair| pair[1].as_str())
    };
    let runtime = PathBuf::from(value("--cef-dir").context("--cef-dir is required")?);
    let output = PathBuf::from(value("--output").context("--output is required")?);
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let resources = Arc::new(SmokeResources {
        page: Arc::from(include_bytes!("../../browser-cef/smoke/index.html").as_slice()),
        counter: Arc::from(include_bytes!("../../browser-cef/smoke/counter.html").as_slice()),
    });
    let proof = Rc::new(RefCell::new(Proof::default()));
    // The entrypoint provides the bootstrap context and owns the main thread.
    let cef = unsafe {
        CefRuntime::initialize(RuntimeOptions {
            runtime: &runtime,
            sandbox_info,
            cache: &output.join("cache"),
            resources,
        })
    }?;
    let browser = cef.open(
        BrowserOptions {
            url: MANAGER_URL,
            backend_url: "http://127.0.0.1:24050/",
            broker_url: None,
            width: 256,
            height: 128,
            scale: 1.0,
            fps_limit: 60,
            shared_texture: false,
            shared_array_buffer: false,
        },
        Box::new(Observer(proof.clone())),
    )?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut resized = false;
    let mut capabilities = None;
    let mut native_bridge = false;
    while Instant::now() < deadline {
        browser.pump()?;
        while let Some(message) = browser.next_message() {
            let message: serde_json::Value = serde_json::from_str(&message)?;
            if message["type"] == "smoke" {
                native_bridge = message["__vosulay_document_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty());
                capabilities = Some(message["capabilities"].clone());
            }
        }
        if !resized && proof.borrow().red && proof.borrow().iframe_pixels {
            browser.resize(384, 192, 1.0)?;
            resized = true;
        }
        if capabilities.is_some() && proof.borrow().resized {
            break;
        }
        let next = browser
            .next_pump_deadline()
            .unwrap_or(deadline)
            .min(deadline);
        std::thread::park_timeout(next.saturating_duration_since(Instant::now()));
    }
    browser.close()?;
    drop(cef);
    let proof = proof.borrow();
    let result = serde_json::json!({ "osr": proof.frames > 0, "frames": proof.frames, "transparent": proof.transparent, "red_marker": proof.red, "iframe_pixels": proof.iframe_pixels, "resized": proof.resized, "native_bridge": native_bridge, "capabilities": capabilities, "clean_shutdown": true });
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    if !proof.image.is_empty() {
        std::fs::write(output.join("frame.ppm"), &proof.image)?;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    ensure!(
        proof.transparent && proof.red && proof.iframe_pixels && proof.resized,
        "OSR pixel/resize proof failed"
    );
    ensure!(native_bridge, "manager native bridge did not initialize");
    let capabilities = capabilities.context("no JavaScript capability report")?;
    ensure!(
        capabilities["javascript"] == true
            && capabilities["secure"] == true
            && capabilities["iframe"] == true
            && capabilities["webgl"] == true,
        "secure scheme/JavaScript/iframe/WebGL proof failed"
    );
    if capabilities["webgpu"]["adapter"] == true {
        ensure!(
            capabilities["webgpu"]["compute"] == true,
            "WebGPU compute failed"
        );
    }
    Ok(())
}

/// Entry point loaded by CEF's precompiled Windows sandbox bootstrap.
///
/// # Safety
/// Invoked only by the matching CEF bootstrap with its live sandbox context.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RunWinMain(
    _instance: *mut std::ffi::c_void,
    _command_line: *mut u16,
    _show: i32,
    sandbox_info: *mut u8,
    _version_info: *mut std::ffi::c_void,
) -> i32 {
    match unsafe { run(sandbox_info) } {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}
