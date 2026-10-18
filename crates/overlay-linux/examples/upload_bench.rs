//! Headless benchmark of the in-game cost of overlay updates.
//!
//! A synthetic surfaceless-EGL "game" renders a pbuffer uncapped with two
//! frames in flight, while a fake vosulay host in this process attaches to the
//! preloaded compositor over the real control protocol and publishes overlay
//! frames into a real frame ring. Nothing opens a window or reads input.
//! Run it through `tools/overlay-upload-bench.py`, which builds the compositor
//! library, preloads it and interleaves scenarios; see docs/testing.md.
//!
//! One JSON line per run: game frame rate, frame time, CPU time of the hooked
//! swap, GPU time of the game's work and of everything the hook submitted, the
//! host's publication count, and a pixel read back after the last swap.
#[cfg(target_os = "linux")]
mod bench {
    use std::{
        collections::VecDeque,
        ffi::{CStr, c_char, c_void},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU32, Ordering},
        },
        time::{Duration, Instant},
    };
    use vosulay_frame::DamageRect;
    use vosulay_transport::{
        Listener, Producer,
        protocol::{FORMAT_BGRA, Kind, Packet},
    };

    type Display = *mut c_void;
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetProcAddress(name: *const c_char) -> *const c_void;
        fn eglInitialize(d: Display, major: *mut i32, minor: *mut i32) -> u32;
        fn eglBindAPI(api: u32) -> u32;
        fn eglChooseConfig(
            d: Display,
            attributes: *const i32,
            config: *mut *mut c_void,
            size: i32,
            found: *mut i32,
        ) -> u32;
        fn eglCreatePbufferSurface(d: Display, c: *mut c_void, a: *const i32) -> *mut c_void;
        fn eglCreateContext(
            d: Display,
            c: *mut c_void,
            s: *mut c_void,
            a: *const i32,
        ) -> *mut c_void;
        fn eglMakeCurrent(d: Display, w: *mut c_void, r: *mut c_void, c: *mut c_void) -> u32;
        fn eglSwapBuffers(d: Display, s: *mut c_void) -> u32;
    }
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// # Safety
    /// A context of the API that exports `name` must be current.
    unsafe fn gl<T: Copy>(name: &CStr) -> T {
        let p = unsafe { eglGetProcAddress(name.as_ptr()) };
        assert!(!p.is_null(), "missing {name:?}");
        unsafe { std::mem::transmute_copy(&p) }
    }

    const WIDTH: u32 = 2560;
    const HEIGHT: u32 = 1440;
    /// Counter rectangles: one near the top-left and one near the bottom-right
    /// corner, the bounding-box worst case for two small updates.
    const TOP_LEFT: DamageRect = DamageRect {
        x: 100,
        y: 100,
        width: 600,
        height: 200,
    };
    const BOTTOM_RIGHT: DamageRect = DamageRect {
        x: WIDTH - 700,
        y: HEIGHT - 300,
        width: 600,
        height: 200,
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Scenario {
        None,
        Static,
        Small,
        Corners,
        Full,
    }
    impl Scenario {
        fn parse(value: &str) -> Option<Self> {
            Some(match value {
                "none" => Self::None,
                "static" => Self::Static,
                "small" => Self::Small,
                "corners" => Self::Corners,
                "full" => Self::Full,
                _ => return None,
            })
        }
        fn name(self) -> &'static str {
            match self {
                Self::None => "none",
                Self::Static => "static",
                Self::Small => "small",
                Self::Corners => "corners",
                Self::Full => "full",
            }
        }
    }

    struct Options {
        scenario: Scenario,
        rate: f64,
        seconds: f64,
        draws: u32,
        es: bool,
    }
    fn options() -> Options {
        let mut options = Options {
            scenario: Scenario::Static,
            rate: 120.0,
            seconds: 2.0,
            draws: 4,
            es: false,
        };
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            let mut value = || arguments.next().unwrap_or_else(|| usage());
            match argument.as_str() {
                "--scenario" => {
                    options.scenario = Scenario::parse(&value()).unwrap_or_else(|| usage())
                }
                "--rate" => options.rate = value().parse().unwrap_or_else(|_| usage()),
                "--seconds" => options.seconds = value().parse().unwrap_or_else(|_| usage()),
                "--draws" => options.draws = value().parse().unwrap_or_else(|_| usage()),
                "--api" => options.es = value() == "es",
                _ => usage(),
            }
        }
        options
    }
    fn usage() -> ! {
        eprintln!(
            "usage: upload_bench [--scenario none|static|small|corners|full] [--rate UPDATES_PER_SECOND] \
             [--seconds S] [--draws GAME_LAYERS] [--api gl|es]\n\
             Preload libvosulay_lookup.so and libvosulay_overlay.so and set VOSULAY_ENDPOINT, \
             except for --scenario none."
        );
        std::process::exit(2)
    }

    fn paint(pixels: &mut [u8], rect: DamageRect, value: u8) {
        for y in rect.y..rect.y + rect.height {
            let start = ((y * WIDTH + rect.x) * 4) as usize;
            for pixel in pixels[start..start + rect.width as usize * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&[value, 255 - value, 128, 255]);
            }
        }
    }

    /// The host side of one attachment: Hello, Visible and the Ring, pings
    /// every 100 ms like the real host, and publications at `rate`.
    /// Lets the game stop updates before checking content: once `quiet` is
    /// set the host publishes no new changes, finishes publishing the last
    /// one, and stores its counter value (+1) in `settled`.
    struct Settle {
        quiet: AtomicBool,
        settled: AtomicU32,
    }

    fn host(scenario: Scenario, rate: f64, stop: Instant, settle: Arc<Settle>) -> u64 {
        let endpoint = std::env::var("VOSULAY_ENDPOINT").expect("VOSULAY_ENDPOINT is required");
        let listener = Listener::bind(&endpoint).expect("bind the private endpoint");
        let mut producer = Producer::new(WIDTH, HEIGHT).expect("create the frame ring");
        let mut pixels = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
        let connection = loop {
            if let Some(connection) = listener.accept().expect("accept") {
                break connection;
            }
            assert!(Instant::now() < stop, "the compositor never connected");
            std::thread::sleep(Duration::from_millis(1));
        };
        loop {
            if let Some(received) = connection.recv().expect("receive Hello")
                && received.packet.kind == Kind::Hello
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let session = |mut packet: Packet| {
            packet.session = 1;
            packet
        };
        let mut hello = session(Packet::new(Kind::Hello));
        hello.value[0] = u64::from(std::process::id());
        connection.send(&hello, &[]).expect("send Hello");
        let mut visible = session(Packet::new(Kind::Visible));
        visible.code = 1;
        connection.send(&visible, &[]).expect("send Visible");
        // A mostly transparent overlay with opaque counters, like a real profile.
        paint(&mut pixels, TOP_LEFT, 0);
        paint(&mut pixels, BOTTOM_RIGHT, 0);
        producer.publish(&pixels, None).expect("first publication");
        let handle = producer.handle();
        let mut ring = session(Packet::new(Kind::Ring));
        ring.generation = 1;
        ring.width = WIDTH;
        ring.height = HEIGHT;
        ring.stride = WIDTH * 4;
        ring.format = FORMAT_BGRA;
        ring.value[0] = handle.mapping_len() as u64;
        connection
            .send(&ring, &[handle.descriptor()])
            .expect("send Ring");
        let period = Duration::from_secs_f64(1.0 / rate.max(0.001));
        let mut next_frame = Instant::now() + period;
        let mut next_ping = Instant::now();
        let mut value = 0u8;
        let mut published = 0;
        let mut last_damage = None;
        while Instant::now() < stop {
            while let Ok(Some(_)) = connection.recv() {}
            let now = Instant::now();
            if now >= next_ping {
                let _ = connection.send(&session(Packet::new(Kind::Ping)), &[]);
                next_ping = now + Duration::from_millis(100);
            }
            if settle.quiet.load(Ordering::Acquire) {
                if settle.settled.load(Ordering::Acquire) == 0 {
                    // A publication dropped because every slot was leased is
                    // retried with its damage; unchanged pixels return false.
                    for _ in 0..50 {
                        if producer.publish(&pixels, last_damage).expect("publish") {
                            published += 1;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    settle
                        .settled
                        .store(u32::from(value) + 1, Ordering::Release);
                }
            } else if scenario != Scenario::Static && now >= next_frame {
                value = value.wrapping_add(1);
                let damage = match scenario {
                    Scenario::Full => {
                        // Every pixel changes; alpha stays put so the counters
                        // remain opaque for the composition proof.
                        for pixel in pixels.chunks_exact_mut(4) {
                            pixel[0] = value;
                        }
                        None
                    }
                    Scenario::Small => {
                        paint(&mut pixels, TOP_LEFT, value);
                        Some(TOP_LEFT)
                    }
                    _ => {
                        paint(&mut pixels, TOP_LEFT, value);
                        paint(&mut pixels, BOTTOM_RIGHT, value);
                        Some(TOP_LEFT.union(BOTTOM_RIGHT))
                    }
                };
                last_damage = damage;
                if producer.publish(&pixels, damage).expect("publish") {
                    published += 1;
                }
                next_frame += period;
                if next_frame < now {
                    next_frame = now + period;
                }
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        published
    }

    struct Timestamps {
        query_counter: unsafe extern "C" fn(u32, u32),
        available: unsafe extern "C" fn(u32, u32, *mut i32),
        result: unsafe extern "C" fn(u32, u32, *mut u64),
        free: Vec<[u32; 3]>,
        /// Query sets in flight, and whether their frame is measured.
        pending: VecDeque<([u32; 3], bool)>,
        game_ns: Vec<f64>,
        hook_ns: Vec<f64>,
    }
    const GL_TIMESTAMP: u32 = 0x8e28;
    const GL_QUERY_RESULT: u32 = 0x8866;
    const GL_QUERY_RESULT_AVAILABLE: u32 = 0x8867;
    impl Timestamps {
        /// # Safety
        /// A desktop GL 3.3 context is current.
        unsafe fn new() -> Self {
            let generate: unsafe extern "C" fn(i32, *mut u32) = unsafe { gl(c"glGenQueries") };
            let mut names = vec![0u32; 3 * 16];
            unsafe { generate(names.len() as i32, names.as_mut_ptr()) };
            Self {
                query_counter: unsafe { gl(c"glQueryCounter") },
                available: unsafe { gl(c"glGetQueryObjectiv") },
                result: unsafe { gl(c"glGetQueryObjectui64v") },
                free: names.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                pending: VecDeque::new(),
                game_ns: Vec::new(),
                hook_ns: Vec::new(),
            }
        }
        /// Collects finished frames without waiting; a frame goes untimed
        /// when every query set is still in flight.
        fn poll(&mut self) {
            while let Some((set, record)) = self.pending.front().copied() {
                let mut ready = 0;
                unsafe { (self.available)(set[2], GL_QUERY_RESULT_AVAILABLE, &mut ready) };
                if ready == 0 {
                    break;
                }
                let mut times = [0u64; 3];
                for (time, query) in times.iter_mut().zip(set) {
                    unsafe { (self.result)(query, GL_QUERY_RESULT, time) };
                }
                if record {
                    self.game_ns.push(times[1].saturating_sub(times[0]) as f64);
                    self.hook_ns.push(times[2].saturating_sub(times[1]) as f64);
                }
                self.pending.pop_front();
                self.free.push(set);
            }
        }
    }

    fn percentile(sorted: &[f64], quantile: f64) -> f64 {
        if sorted.is_empty() {
            return f64::NAN;
        }
        sorted[((sorted.len() - 1) as f64 * quantile).round() as usize]
    }
    fn summary(mut values: Vec<f64>, scale: f64) -> String {
        values.sort_by(f64::total_cmp);
        let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
        format!(
            "{{\"p50\":{:.1},\"p99\":{:.1},\"mean\":{:.1}}}",
            percentile(&values, 0.5) * scale,
            percentile(&values, 0.99) * scale,
            mean * scale
        )
    }

    /// The compositor's optional profile (built with its `profile` feature),
    /// found in the preloaded library without linking to it.
    fn compositor_profile() -> Option<String> {
        type Report = unsafe extern "C" fn(*mut u8, usize) -> usize;
        let symbol = unsafe {
            dlsym(
                std::ptr::null_mut(),
                c"vosulay_overlay_profile_json".as_ptr(),
            )
        };
        if symbol.is_null() {
            return None;
        }
        let report: Report = unsafe { std::mem::transmute(symbol) };
        let mut buffer = vec![0u8; 64 * 1024];
        let length = unsafe { report(buffer.as_mut_ptr(), buffer.len()) };
        buffer.truncate(length.min(buffer.len()));
        String::from_utf8(buffer).ok()
    }

    /// Drops the compositor profile's warm-up samples, when it has one.
    fn reset_compositor_profile() {
        let symbol = unsafe {
            dlsym(
                std::ptr::null_mut(),
                c"vosulay_overlay_profile_reset".as_ptr(),
            )
        };
        if !symbol.is_null() {
            let reset: unsafe extern "C" fn() = unsafe { std::mem::transmute(symbol) };
            unsafe { reset() };
        }
    }

    pub fn main() {
        let options = options();
        let warmup = Duration::from_secs(1);
        let stop = Instant::now()
            + warmup
            + Duration::from_secs_f64(options.seconds)
            + Duration::from_secs(1);
        let settle = Arc::new(Settle {
            quiet: AtomicBool::new(false),
            settled: AtomicU32::new(0),
        });
        let host = (options.scenario != Scenario::None).then(|| {
            let (scenario, rate, settle) = (options.scenario, options.rate, Arc::clone(&settle));
            std::thread::spawn(move || host(scenario, rate, stop, settle))
        });
        // SAFETY: EGL and GL calls below follow their C contracts on one thread
        // with the created context current.
        unsafe {
            let platform: unsafe extern "C" fn(u32, *mut c_void, *const isize) -> Display =
                gl(c"eglGetPlatformDisplayEXT");
            let display = platform(0x31dd, std::ptr::null_mut(), std::ptr::null());
            assert!(!display.is_null(), "no surfaceless EGL display");
            let (mut major, mut minor) = (0, 0);
            assert_ne!(eglInitialize(display, &mut major, &mut minor), 0);
            let (api, renderable) = if options.es {
                (0x30a0, 0x40)
            } else {
                (0x30a2, 0x08)
            };
            assert_ne!(eglBindAPI(api), 0);
            let attributes = [
                0x3033, 1, 0x3040, renderable, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
            ];
            let mut config = std::ptr::null_mut();
            let mut found = 0;
            assert_ne!(
                eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut found),
                0
            );
            assert_eq!(found, 1, "no pbuffer config");
            let surface = eglCreatePbufferSurface(
                display,
                config,
                [0x3057, WIDTH as i32, 0x3056, HEIGHT as i32, 0x3038].as_ptr(),
            );
            assert!(!surface.is_null());
            // Desktop GL 3.3 core like lazer's GL renderer, or ES 3.0.
            let context_attributes: &[i32] = if options.es {
                &[0x3098, 3, 0x3038]
            } else {
                &[0x3098, 3, 0x30fb, 3, 0x30fd, 1, 0x3038]
            };
            let context = eglCreateContext(
                display,
                config,
                std::ptr::null_mut(),
                context_attributes.as_ptr(),
            );
            assert!(!context.is_null(), "context creation failed");
            assert_ne!(eglMakeCurrent(display, surface, surface, context), 0);
            let get_string: unsafe extern "C" fn(u32) -> *const c_char = gl(c"glGetString");
            let renderer = CStr::from_ptr(get_string(0x1f01))
                .to_string_lossy()
                .into_owned();
            let clear_color: unsafe extern "C" fn(f32, f32, f32, f32) = gl(c"glClearColor");
            let clear: unsafe extern "C" fn(u32) = gl(c"glClear");
            let finish: unsafe extern "C" fn() = gl(c"glFinish");
            let fence: unsafe extern "C" fn(u32, u32) -> *mut c_void = gl(c"glFenceSync");
            let wait: unsafe extern "C" fn(*mut c_void, u32, u64) -> u32 = gl(c"glClientWaitSync");
            let delete_sync: unsafe extern "C" fn(*mut c_void) = gl(c"glDeleteSync");
            let create_shader: unsafe extern "C" fn(u32) -> u32 = gl(c"glCreateShader");
            let shader_source: unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32) =
                gl(c"glShaderSource");
            let compile: unsafe extern "C" fn(u32) = gl(c"glCompileShader");
            let create_program: unsafe extern "C" fn() -> u32 = gl(c"glCreateProgram");
            let attach: unsafe extern "C" fn(u32, u32) = gl(c"glAttachShader");
            let link: unsafe extern "C" fn(u32) = gl(c"glLinkProgram");
            let use_program: unsafe extern "C" fn(u32) = gl(c"glUseProgram");
            let generate_vertex_arrays: unsafe extern "C" fn(i32, *mut u32) =
                gl(c"glGenVertexArrays");
            let bind_vertex_array: unsafe extern "C" fn(u32) = gl(c"glBindVertexArray");
            let draw: unsafe extern "C" fn(u32, i32, i32) = gl(c"glDrawArrays");
            let enable: unsafe extern "C" fn(u32) = gl(c"glEnable");
            let blend: unsafe extern "C" fn(u32, u32) = gl(c"glBlendFunc");
            let read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void) =
                gl(c"glReadPixels");
            // Game load: blended fullscreen layers, like sprites over a background.
            let (vertex, fragment) = if options.es {
                (
                    c"#version 300 es\nvoid main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);gl_Position=vec4(p*2.0-1.0,0,1);}",
                    c"#version 300 es\nprecision mediump float;out vec4 c;void main(){c=vec4(fract(gl_FragCoord.xy/97.0),0.5,0.1);}",
                )
            } else {
                (
                    c"#version 330 core\nvoid main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);gl_Position=vec4(p*2.0-1.0,0,1);}",
                    c"#version 330 core\nout vec4 c;void main(){c=vec4(fract(gl_FragCoord.xy/97.0),0.5,0.1);}",
                )
            };
            let program = create_program();
            for (kind, text) in [(0x8b31, vertex), (0x8b30, fragment)] {
                let shader = create_shader(kind);
                let pointer = text.as_ptr();
                shader_source(shader, 1, &pointer, std::ptr::null());
                compile(shader);
                attach(program, shader);
            }
            link(program);
            let mut vertex_array = 0;
            generate_vertex_arrays(1, &mut vertex_array);
            let mut timestamps = (!options.es).then(|| Timestamps::new());
            let mut fences: VecDeque<*mut c_void> = VecDeque::new();
            let mut swap_us = Vec::new();
            let mut frame = |swap_us: &mut Vec<f64>, record: bool| {
                let set = timestamps.as_mut().and_then(|t| {
                    t.poll();
                    t.free.pop()
                });
                let counter = timestamps.as_ref().map(|t| t.query_counter);
                if let (Some(set), Some(counter)) = (set, counter) {
                    counter(set[0], GL_TIMESTAMP);
                }
                clear_color(0.08, 0.18, 0.30, 1.0);
                clear(0x4000);
                use_program(program);
                bind_vertex_array(vertex_array);
                enable(0x0be2);
                blend(0x0302, 0x0303);
                for _ in 0..options.draws {
                    draw(0x0004, 0, 3);
                }
                if let (Some(set), Some(counter)) = (set, counter) {
                    counter(set[1], GL_TIMESTAMP);
                }
                let started = Instant::now();
                assert_ne!(eglSwapBuffers(display, surface), 0);
                if record {
                    swap_us.push(started.elapsed().as_secs_f64() * 1e6);
                }
                // The hook submitted its upload and composite inside the swap;
                // a pbuffer swap presents nothing itself.
                if let (Some(set), Some(counter)) = (set, counter) {
                    counter(set[2], GL_TIMESTAMP);
                    timestamps
                        .as_mut()
                        .unwrap()
                        .pending
                        .push_back((set, record));
                }
                // Two frames in flight, like a swapchain.
                fences.push_back(fence(0x9117, 0));
                if fences.len() > 2 {
                    let oldest = fences.pop_front().unwrap();
                    wait(oldest, 1, u64::MAX);
                    delete_sync(oldest);
                }
            };
            let warm = Instant::now() + warmup;
            while Instant::now() < warm {
                frame(&mut swap_us, false);
            }
            reset_compositor_profile();
            let start = Instant::now();
            let end = start + Duration::from_secs_f64(options.seconds);
            let mut frames = 0u64;
            while Instant::now() < end {
                frame(&mut swap_us, true);
                frames += 1;
            }
            finish();
            let elapsed = start.elapsed().as_secs_f64();
            // Settle: stop updates, keep presenting until the last one has
            // had time to reach the screen, then check both counters.
            settle.quiet.store(true, Ordering::Release);
            let settle_end = Instant::now() + Duration::from_millis(300);
            while Instant::now() < settle_end
                || (host.is_some() && settle.settled.load(Ordering::Acquire) == 0)
            {
                frame(&mut swap_us, false);
                assert!(
                    Instant::now() < stop + Duration::from_secs(5),
                    "the host never settled"
                );
            }
            finish();
            if let Some(t) = timestamps.as_mut() {
                t.poll();
            }
            let value = settle.settled.load(Ordering::Acquire).saturating_sub(1) as u8;
            let read_counter = |rect: DamageRect| {
                let mut pixel = [0u8; 4];
                read(
                    (rect.x + rect.width / 2) as i32,
                    (HEIGHT - rect.y - rect.height / 2) as i32,
                    1,
                    1,
                    0x1908,
                    0x1401,
                    pixel.as_mut_ptr().cast(),
                );
                pixel
            };
            let pixel = read_counter(TOP_LEFT);
            let corner = read_counter(BOTTOM_RIGHT);
            // Expected RGBA of each counter after the last update (BGRA bytes
            // are swizzled): full changes only blue; painted counters change
            // green and blue; the bottom-right one is painted by corners only.
            let painted = [128, 255 - value, value, 255];
            let expected = match options.scenario {
                Scenario::None => None,
                Scenario::Static => Some(([128, 255, 0, 255], [128, 255, 0, 255])),
                Scenario::Small => Some((painted, [128, 255, 0, 255])),
                Scenario::Corners => Some((painted, painted)),
                Scenario::Full => Some(([128, 255, value, 255], [128, 255, value, 255])),
            };
            let content_ok = expected.is_none_or(|(top, bottom)| pixel == top && corner == bottom);
            let published = host.map(|thread| thread.join().expect("host thread"));
            let gpu = timestamps.map_or_else(
                || "null".to_owned(),
                |t| {
                    format!(
                        "{{\"game_us\":{},\"hook_us\":{}}}",
                        summary(t.game_ns, 1e-3),
                        summary(t.hook_ns, 1e-3)
                    )
                },
            );
            println!(
                "{{\"scenario\":\"{}\",\"rate\":{},\"draws\":{},\"api\":\"{}\",\"renderer\":\"{}\",\"fps\":{:.1},\"frame_ms\":{:.4},\"swap_cpu_us\":{},\"gpu\":{},\"published\":{},\"pixel\":[{},{},{},{}],\"corner\":[{},{},{},{}],\"content_ok\":{},\"profile\":{}}}",
                options.scenario.name(),
                options.rate,
                options.draws,
                if options.es { "es" } else { "gl" },
                renderer.replace('"', "'"),
                frames as f64 / elapsed,
                elapsed * 1e3 / frames as f64,
                summary(swap_us, 1.0),
                gpu,
                published.map_or_else(|| "null".to_owned(), |count| count.to_string()),
                pixel[0],
                pixel[1],
                pixel[2],
                pixel[3],
                corner[0],
                corner[1],
                corner[2],
                corner[3],
                content_ok,
                compositor_profile().unwrap_or_else(|| "null".to_owned()),
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    bench::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("upload_bench is a Linux-only EGL compositor benchmark");
    std::process::exit(2);
}
