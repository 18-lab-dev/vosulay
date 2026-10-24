//! Linux-only OSR measurement of the real manager assets, without game attachment.
#[cfg(all(target_os = "linux", feature = "cef-host"))]
mod linux {
    use anyhow::{Context, Result, ensure};
    use serde_json::{Value, json};
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

    struct Resources(Arc<[u8]>);
    impl ResourceService for Resources {
        fn load(&self, route: ResourceRoute<'_>) -> Option<Resource> {
            let ResourceRoute::Manager {
                path: "/index.html",
            } = route
            else {
                return None;
            };
            Some(Resource {
                bytes: self.0.clone(),
                mime_type: "text/html".into(),
                status: 200,
            })
        }
    }
    struct Paints {
        active: bool,
        initial_frames: u64,
        screenshot: Option<PathBuf>,
        frames: u64,
        damage_pixels: u64,
        full_frames: u64,
        intervals: Vec<f64>,
        previous: Option<Instant>,
        transparent: bool,
    }
    struct Observer(Rc<RefCell<Paints>>);
    impl BrowserFrameBackend for Observer {
        fn resize(&mut self, _: u32, _: u32, _: f32) -> Result<()> {
            Ok(())
        }
        fn set_visible(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()> {
            let BrowserFrame::Bgra {
                pixels,
                width,
                height,
                damage,
            } = frame;
            let mut paints = self.0.borrow_mut();
            if !paints.active {
                paints.initial_frames += 1;
            } else if let Some(path) = paints.screenshot.take() {
                std::fs::write(path, pixels)?;
            }
            if !paints.active {
                return Ok(());
            }
            let now = Instant::now();
            if let Some(previous) = paints.previous.replace(now) {
                paints
                    .intervals
                    .push(now.duration_since(previous).as_secs_f64() * 1000.0);
            }
            paints.frames += 1;
            paints.transparent |= pixels[3] == 0;
            let area = u64::from(width) * u64::from(height);
            let dirty = damage.map_or(area, |damage| {
                damage
                    .as_slice()
                    .iter()
                    .map(|r| u64::from(r.width) * u64::from(r.height))
                    .sum()
            });
            paints.damage_pixels += dirty;
            paints.full_frames += u64::from(damage.is_none_or(|damage| {
                damage
                    .as_slice()
                    .iter()
                    .any(|r| r.x == 0 && r.y == 0 && r.width == width && r.height == height)
            }));
            Ok(())
        }
    }
    fn percentile(values: &mut [f64], fraction: f64) -> f64 {
        values.sort_unstable_by(f64::total_cmp);
        values
            .get(((values.len().saturating_sub(1)) as f64 * fraction) as usize)
            .copied()
            .unwrap_or(0.0)
    }
    fn cpu() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
        time.tv_sec as f64 + time.tv_nsec as f64 / 1e9
    }
    fn tree_cpu(pid: u32) -> Result<f64> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0.0),
            Err(error) => return Err(error.into()),
        };
        let mut fields = stat
            .rsplit_once(')')
            .context("invalid process stat")?
            .1
            .split_whitespace()
            .skip(11)
            .take(4);
        let ticks = fields.try_fold(0.0, |sum, field| {
            field.parse::<f64>().map(|value| sum + value)
        })?;
        let mut seconds = ticks / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        // CEF can launch subprocesses from non-main threads. Linux exposes each
        // child only in its creating task's children file, not the leader's.
        let mut children = Vec::<u32>::new();
        for task in std::fs::read_dir(format!("/proc/{pid}/task"))? {
            if let Ok(list) = std::fs::read_to_string(task?.path().join("children")) {
                for child in list.split_whitespace() {
                    children.push(child.parse()?);
                }
            }
        }
        children.sort_unstable();
        children.dedup();
        for child in children {
            seconds += tree_cpu(child)?;
        }
        Ok(seconds)
    }
    fn allocations() -> Result<[u64; 2]> {
        let Some(directory) = std::env::var_os("VOSULAY_PERF_ALLOC_DIR") else {
            return Ok([0, 0]);
        };
        let mut totals = [0, 0];
        for entry in std::fs::read_dir(directory)? {
            let bytes = std::fs::read(entry?.path())?;
            if bytes.len() == 24 {
                for (i, total) in totals.iter_mut().enumerate() {
                    *total += u64::from_ne_bytes(bytes[i * 8..i * 8 + 8].try_into()?);
                }
            }
        }
        Ok(totals)
    }
    pub fn run() -> Result<()> {
        if let Some(code) = unsafe { execute_subprocess(std::ptr::null_mut()) } {
            std::process::exit(code);
        }
        let args: Vec<_> = std::env::args().collect();
        let value = |key: &str| {
            args.windows(2)
                .find(|pair| pair[0] == key)
                .map(|pair| pair[1].as_str())
        };
        let runtime = PathBuf::from(value("--cef-dir").context("--cef-dir required")?);
        let cache = PathBuf::from(value("--cache").context("--cache required")?);
        let backend = value("--backend").context("--backend required")?;
        let seconds: f64 = value("--seconds").unwrap_or("6").parse()?;
        let editing = args.iter().any(|arg| arg == "--editing");
        let idle = args.iter().any(|arg| arg == "--idle");
        let fps = value("--fps")
            .map(str::parse::<u32>)
            .transpose()?
            .unwrap_or_else(|| vosulay::runtime::presentation_frame_rate(60.0, editing));
        let instrumentation = r#"<script>
            window.__bench={loads:0,longTasks:0,longTaskMs:0};
            new PerformanceObserver(list=>{for(const e of list.getEntries()){__bench.longTasks++;__bench.longTaskMs+=e.duration}}).observe({entryTypes:['longtask']});
            document.addEventListener('load',e=>{if(e.target.tagName==='IFRAME')__bench.loads++},true);
            document.addEventListener('DOMContentLoaded',()=>{
                const gl=document.createElement('canvas').getContext('webgl2');
                const ext=gl?.getExtension('WEBGL_debug_renderer_info');
                window.__bench_gpu=gl?{renderer:gl.getParameter(ext?ext.UNMASKED_RENDERER_WEBGL:gl.RENDERER),version:gl.getParameter(gl.VERSION)}:null;
            });
        </script>"#;
        let mut document = vosulay::ui::manager_document()
            .replace("</head>", &format!("{instrumentation}</head>"));
        if let Some(theme) = value("--theme") {
            ensure!(matches!(theme, "dark" | "light"), "invalid theme");
            document = document.replace(
                "<head>",
                &format!(
                    "<head><script>window.__VOSULAY_COLOR_SCHEME__={};</script>",
                    json!(theme)
                ),
            );
        }
        if let Some(path) = value("--css") {
            document = document.replace(
                "</body>",
                &format!("<style>{}</style></body>", std::fs::read_to_string(path)?),
            );
        }
        let resources = Arc::new(Resources(Arc::from(document.into_bytes())));
        let paints = Rc::new(RefCell::new(Paints {
            active: false,
            initial_frames: 0,
            screenshot: value("--screenshot").map(PathBuf::from),
            frames: 0,
            damage_pixels: 0,
            full_frames: 0,
            intervals: Vec::with_capacity(4096),
            previous: None,
            transparent: false,
        }));
        let runtime = unsafe {
            CefRuntime::initialize(RuntimeOptions {
                runtime: &runtime,
                sandbox_info: std::ptr::null_mut(),
                cache: &cache,
                resources,
            })
        }?;
        let browser = runtime.open(
            BrowserOptions {
                url: MANAGER_URL,
                backend_url: backend,
                broker: None,
                width: 2560,
                height: 1440,
                scale: 1.0,
                fps_limit: fps,
                shared_texture: false,
                shared_array_buffer: false,
            },
            Box::new(Observer(paints.clone())),
        )?;
        let overlays: Vec<_> = [(24.0, 24.0), (1900.0, 1160.0)].into_iter().enumerate().map(|(i,(left,top))| json!({
            "counter_id":format!("custom:{i}"),"url":format!("{backend}counter/{i}"),"name":format!("Fixture {i}"),
            "left":left,"top":top,"width":600.0,"height":200.0,"scale":1.0,"opacity":1.0,"rotation":0.0,"enabled":true,"z_index":i
        })).collect();
        let config = json!({"version":1,"active_profile":"default","profiles":[{"id":"default","name":"Default","overlays":overlays}],"settings":{}});
        let ready_deadline = Instant::now() + Duration::from_secs(20);
        let mut ready_at = None;
        let mut started = None;
        let mut cpu_start = 0.0;
        let mut tree_cpu_start = 0.0;
        let mut allocation_start = [0, 0];
        let mut pumps = 0u64;
        let mut pump_ms = Vec::with_capacity(8192);
        let mut report = None;
        let mut asked = false;
        loop {
            let before = Instant::now();
            browser.pump()?;
            if started.is_some() {
                pumps += 1;
                pump_ms.push(before.elapsed().as_secs_f64() * 1000.0);
            }
            while let Some(message) = browser.next_message() {
                let message: Value = serde_json::from_str(&message)?;
                if message["type"] == "ready" && ready_at.is_none() {
                    for envelope in [
                        json!({"type":"config","config":config,"revision":0}),
                        json!({"type":"counters","counters":[]}),
                        json!({"type":"editing","editing":editing}),
                        json!({"type":"ruleset","ruleset":"mania"}),
                    ] {
                        browser.evaluate(&format!("window.__VOSULAY_RECEIVE__({envelope});"));
                    }
                    ready_at = Some(Instant::now());
                } else if message["type"] == "benchmark" {
                    report = Some(message);
                }
            }
            if started.is_none()
                && ready_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
            {
                browser.evaluate("window.__bench={loads:0,longTasks:0,longTaskMs:0};");
                paints.borrow_mut().active = true;
                started = Some(Instant::now());
                cpu_start = cpu();
                tree_cpu_start = tree_cpu(std::process::id())?;
                allocation_start = allocations()?;
            }
            if started.is_some_and(|at| at.elapsed().as_secs_f64() >= seconds) && !asked {
                paints.borrow_mut().active = false;
                browser.evaluate("window.__VOSULAY_POST_MESSAGE__({type:'benchmark',...__bench,iframes:document.querySelectorAll('iframe').length,editing:document.body.classList.contains('editing'),fonts:document.fonts.status,gpu:window.__bench_gpu});");
                asked = true;
            }
            if report.is_some() {
                break;
            }
            ensure!(
                Instant::now() < ready_deadline,
                "manager benchmark timed out"
            );
            let next = browser
                .next_pump_deadline()
                .unwrap_or(Instant::now() + Duration::from_millis(10))
                .min(Instant::now() + Duration::from_millis(10));
            std::thread::park_timeout(next.saturating_duration_since(Instant::now()));
        }
        let elapsed = started
            .context("manager never became ready")?
            .elapsed()
            .as_secs_f64();
        let cpu_ms = (cpu() - cpu_start) * 1000.0;
        let total_cpu_ms = (tree_cpu(std::process::id())? - tree_cpu_start) * 1000.0;
        let allocation_end = allocations()?;
        let allocation_sampled = std::env::var_os("VOSULAY_PERF_ALLOC_DIR").is_some();
        let mut paints = paints.borrow_mut();
        ensure!(
            (idle || paints.frames > 0)
                && paints.initial_frames > 0
                && report.as_ref().is_some_and(|r| r["iframes"] == 2),
            "counter frames did not render: {report:?}"
        );
        ensure!(
            report.as_ref().is_some_and(|r| r["loads"] == 0),
            "steady traffic reloaded counter frames"
        );
        let partial_paints = paints.frames == 0 || paints.full_frames * 20 < paints.frames;
        ensure!(
            !args.iter().any(|arg| arg == "--check") || partial_paints,
            "steady traffic forced full-frame paints: {}/{}",
            paints.full_frames,
            paints.frames
        );
        ensure!(
            !args.iter().any(|arg| arg == "--check") || !idle || paints.frames == 0,
            "idle editor repainted {} times after warmup",
            paints.frames
        );
        let (paint_max_ms, delayed_intervals) = paints.intervals.iter().copied().fold(
            (0.0_f64, 0_usize),
            |(maximum, delayed), interval| {
                (
                    maximum.max(interval),
                    delayed + usize::from(interval > 1500.0 / fps as f64),
                )
            },
        );
        let per_paint = |value: f64| (paints.frames > 0).then(|| value / paints.frames as f64);
        println!(
            "{}",
            json!({"seconds":elapsed,"editing":editing,"idle":idle,"fps_limit":fps,"initial_frames":paints.initial_frames,"frames":paints.frames,
            "paints_per_second":paints.frames as f64/elapsed,"host_cpu_ms_per_frame":per_paint(cpu_ms),
            "total_cpu_ms_per_frame":per_paint(total_cpu_ms),"host_cpu_ms_per_second":cpu_ms/elapsed,"total_cpu_ms_per_second":total_cpu_ms/elapsed,
            "allocations_per_frame":if allocation_sampled { per_paint((allocation_end[0]-allocation_start[0]) as f64) } else { None },
            "allocated_bytes_per_frame":if allocation_sampled { per_paint((allocation_end[1]-allocation_start[1]) as f64) } else { None },
            "dirty_pixels_per_frame":per_paint(paints.damage_pixels as f64),
            "full_frame_paints":paints.full_frames,"partial_paint_guard":partial_paints,"transparent":paints.transparent,
            "paint_p50_ms":percentile(&mut paints.intervals,0.5),"paint_p99_ms":percentile(&mut paints.intervals,0.99),
            "paint_max_ms":paint_max_ms,"paint_intervals_over_1_5_period":delayed_intervals,
            "pump_calls":pumps,"pump_p99_ms":percentile(&mut pump_ms,0.99),
            "pump_max_ms":percentile(&mut pump_ms,1.0),"renderer":report})
        );
        drop(paints);
        browser.close()?;
        drop(runtime);
        Ok(())
    }
}
#[cfg(all(target_os = "linux", feature = "cef-host"))]
fn main() -> anyhow::Result<()> {
    linux::run()
}
#[cfg(not(all(target_os = "linux", feature = "cef-host")))]
fn main() {
    eprintln!("manager_bench requires Linux and cef-host");
    std::process::exit(1);
}
