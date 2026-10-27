use super::*;

#[derive(Default)]
struct Capture {
    armed: bool,
    image: Vec<u8>,
}
struct PreviewObserver(Rc<RefCell<Capture>>);
impl BrowserFrameBackend for PreviewObserver {
    fn resize(&mut self, _: u32, _: u32, _: f32) -> Result<()> {
        Ok(())
    }
    fn set_visible(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()> {
        let mut capture = self.0.borrow_mut();
        if !capture.armed || !capture.image.is_empty() {
            return Ok(());
        }
        let (pixels, width, height) = match frame {
            BrowserFrame::Bgra {
                pixels,
                width,
                height,
                ..
            } => (pixels, width, height),
            #[cfg(windows)]
            BrowserFrame::D3d11 { .. } => anyhow::bail!("preview smoke requires CPU paint"),
        };
        if width != 1025 || height != 361 {
            return Ok(());
        }
        capture
            .image
            .reserve(width as usize * height as usize * 3 + 32);
        capture
            .image
            .extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
        for pixel in pixels.chunks_exact(4) {
            capture
                .image
                .extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(())
    }
}

/// Isolated resolver/resource proof; the caller supplies a real tool-preview URL
/// whose test counter reports its style/data through a postMessage probe.
pub(super) unsafe fn run(
    sandbox_info: *mut u8,
    runtime: &std::path::Path,
    output: &std::path::Path,
    url: &str,
) -> Result<()> {
    let source = serde_json::to_string(url)?.replace('<', "\\u003c");
    let page = format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>body{{margin:0;background:white}}iframe{{border:0;width:800px;height:100px;margin:20px}}</style><iframe id="preview" sandbox="allow-scripts" referrerpolicy="no-referrer"></iframe><script>
const frame=document.getElementById('preview');
window.addEventListener('message',event=>{{if(event.source===frame.contentWindow&&event.data?.type==='preview-security-proof')window.__VOSULAY_POST_MESSAGE__({{type:'preview-proof',proof:event.data.proof}});}});
frame.src={source};
</script>"#
    );
    let resources = Arc::new(SmokeResources {
        page: Arc::from(page.into_bytes()),
        counter: Arc::from([]),
    });
    let cef = unsafe {
        CefRuntime::initialize(RuntimeOptions {
            runtime,
            sandbox_info,
            cache: &output.join("cache"),
            resources,
            devtools_port: None,
        })
    }?;
    let capture = Rc::new(RefCell::new(Capture::default()));
    let browser = cef.open(
        BrowserOptions {
            url: MANAGER_URL,
            backend_url: url,
            broker: None,
            width: 1024,
            height: 360,
            scale: 1.0,
            fps_limit: 30,
            shared_texture: false,
            shared_array_buffer: false,
        },
        Box::new(PreviewObserver(capture.clone())),
    )?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut report = None;
    while Instant::now() < deadline {
        browser.pump()?;
        while let Some(message) = browser.next_message() {
            let message: serde_json::Value = serde_json::from_str(&message)?;
            if message["type"] == "preview-proof" {
                report = Some(message["proof"].clone());
                capture.borrow_mut().armed = true;
                browser.resize(1025, 361, 1.0)?;
            }
        }
        if report.is_some() && !capture.borrow().image.is_empty() {
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
    let report = report.context("preview resolver/style/data proof timed out")?;
    std::fs::write(
        output.join("preview-result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    std::fs::write(output.join("preview.ppm"), &capture.borrow().image)?;
    ensure!(
        report["pp"] == "123" && report["font"] == "Custom-Font" && report["fontLoaded"] == true,
        "preview counter did not load its real font and synthetic data"
    );
    ensure!(
        report["parentDenied"] == true
            && report["bridgeDenied"] == true
            && report["managerDenied"] == true,
        "opaque preview reached parent authority"
    );
    ensure!(
        !capture.borrow().image.is_empty(),
        "no preview pixels captured"
    );
    println!(
        "CEF preview resolved on loopback; font, live synthetic pp, parent isolation and pixels passed"
    );
    Ok(())
}
