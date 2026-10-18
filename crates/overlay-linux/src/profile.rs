//! Opt-in timing of the GL upload and composite (`profile` feature, benchmark
//! builds only). CPU time of each GL call on the update path, and GPU time of
//! the upload and the composite from timestamp queries that are read back
//! only once available, so profiling never waits on the GPU itself.
//! `tools/overlay-upload-bench.py --profile` reads the report through
//! `vosulay_overlay_profile_json`.
use parking_lot::Mutex;
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    /// Saving the game's GL state (every composite).
    Capture,
    /// Orphaning the unpack buffer's storage (`glBufferData(NULL)`).
    Orphan,
    /// `glMapBufferRange` of the staging buffer.
    Map,
    /// Copying damaged rows from the frame ring into the mapping.
    Copy,
    /// `glUnmapBuffer`.
    Unmap,
    /// `glTexSubImage2D` calls.
    TexSubImage,
    /// Composite draw submission.
    Draw,
    /// Restoring the game's GL state.
    Restore,
    /// Everything in `compose`, uploads included.
    Compose,
    /// GPU: upload commands (timestamp before the first and after the last).
    GpuUpload,
    /// GPU: the composite draw.
    GpuComposite,
}
const STAGES: usize = 11;
const NAMES: [&str; STAGES] = [
    "cpu_capture",
    "cpu_orphan",
    "cpu_map",
    "cpu_copy",
    "cpu_unmap",
    "cpu_tex_sub_image",
    "cpu_draw",
    "cpu_restore",
    "cpu_compose",
    "gpu_upload",
    "gpu_composite",
];
/// Samples kept per stage; later samples are dropped, not averaged in.
const CAPACITY: usize = 1 << 16;

struct Report {
    samples: [Vec<u32>; STAGES],
    uploads: u64,
    upload_bytes: u64,
}
static REPORT: Mutex<Option<Report>> = Mutex::new(None);

fn with_report(f: impl FnOnce(&mut Report)) {
    let mut report = REPORT.lock();
    let report = report.get_or_insert_with(|| Report {
        samples: std::array::from_fn(|_| Vec::with_capacity(CAPACITY)),
        uploads: 0,
        upload_bytes: 0,
    });
    f(report);
}

pub(crate) fn record(stage: Stage, nanoseconds: u64) {
    with_report(|report| {
        let samples = &mut report.samples[stage as usize];
        if samples.len() < CAPACITY {
            samples.push(nanoseconds.min(u64::from(u32::MAX)) as u32);
        }
    });
}

pub(crate) fn upload(bytes: usize) {
    with_report(|report| {
        report.uploads += 1;
        report.upload_bytes += bytes as u64;
    });
}

/// Runs `f`, recording its CPU time under `stage`.
pub(crate) fn time<T>(stage: Stage, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = f();
    record(stage, start.elapsed().as_nanos() as u64);
    value
}

/// Clears all samples, e.g. after a benchmark's warm-up.
#[unsafe(no_mangle)]
pub extern "C" fn vosulay_overlay_profile_reset() {
    *REPORT.lock() = None;
}

/// Writes the report as JSON (µs per stage: count, p50, p99, mean, sum) into
/// `buffer` and returns its full length, which may exceed `capacity`.
///
/// # Safety
/// `buffer` is writable for `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vosulay_overlay_profile_json(buffer: *mut u8, capacity: usize) -> usize {
    let mut json = String::from("{");
    with_report(|report| {
        for (name, samples) in NAMES.iter().zip(&mut report.samples) {
            samples.sort_unstable();
            let at = |q: f64| {
                samples
                    .get(((samples.len().saturating_sub(1)) as f64 * q).round() as usize)
                    .map_or(0.0, |&value| f64::from(value) / 1e3)
            };
            let sum: f64 = samples.iter().map(|&value| f64::from(value) / 1e3).sum();
            json += &format!(
                "\"{name}\":{{\"count\":{},\"p50\":{:.1},\"p99\":{:.1},\"mean\":{:.1},\"sum\":{:.0}}},",
                samples.len(),
                at(0.5),
                at(0.99),
                sum / samples.len().max(1) as f64,
                sum
            );
        }
        json += &format!(
            "\"uploads\":{},\"upload_bytes\":{}}}",
            report.uploads, report.upload_bytes
        );
    });
    let length = json.len();
    // SAFETY: the caller guarantees `capacity` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(json.as_ptr(), buffer, length.min(capacity));
    }
    length
}

type GenQueries = unsafe extern "C" fn(libc::c_int, *mut libc::c_uint);
type QueryCounter = unsafe extern "C" fn(libc::c_uint, libc::c_uint);
type GetQueryObjectiv = unsafe extern "C" fn(libc::c_uint, libc::c_uint, *mut libc::c_int);
type GetQueryObjectui64v = unsafe extern "C" fn(libc::c_uint, libc::c_uint, *mut u64);
const GL_TIMESTAMP: libc::c_uint = 0x8e28;
const GL_QUERY_RESULT: libc::c_uint = 0x8866;
const GL_QUERY_RESULT_AVAILABLE: libc::c_uint = 0x8867;

/// Timestamp queries around one compose: before the upload, after it, after
/// the draw. Sets are recycled once their results have been read.
pub(crate) struct GpuTimer {
    counter: QueryCounter,
    available: GetQueryObjectiv,
    result: GetQueryObjectui64v,
    free: Vec<([libc::c_uint; 3], bool)>,
    pending: std::collections::VecDeque<([libc::c_uint; 3], bool)>,
    current: Option<([libc::c_uint; 3], bool)>,
}
impl GpuTimer {
    /// None without timer queries (GL < 3.3 or ES without the extension).
    ///
    /// # Safety
    /// The context `load` resolves symbols for is current.
    pub(crate) unsafe fn new(mut load: impl FnMut(&[u8]) -> *const libc::c_void) -> Option<Self> {
        let mut symbol = |names: &[&[u8]]| {
            names
                .iter()
                .map(|name| load(name))
                .find(|address| !address.is_null())
        };
        let generate = symbol(&[b"glGenQueries\0", b"glGenQueriesEXT\0"])?;
        let counter = symbol(&[b"glQueryCounter\0", b"glQueryCounterEXT\0"])?;
        let available = symbol(&[b"glGetQueryObjectiv\0", b"glGetQueryObjectivEXT\0"])?;
        let result = symbol(&[b"glGetQueryObjectui64v\0", b"glGetQueryObjectui64vEXT\0"])?;
        // SAFETY: GL resolved these names with these C signatures.
        unsafe {
            let generate = std::mem::transmute::<*const libc::c_void, GenQueries>(generate);
            let mut names = [0; 3 * 32];
            generate(names.len() as libc::c_int, names.as_mut_ptr());
            Some(Self {
                counter: std::mem::transmute::<*const libc::c_void, QueryCounter>(counter),
                available: std::mem::transmute::<*const libc::c_void, GetQueryObjectiv>(available),
                result: std::mem::transmute::<*const libc::c_void, GetQueryObjectui64v>(result),
                free: names
                    .chunks_exact(3)
                    .map(|set| ([set[0], set[1], set[2]], false))
                    .collect(),
                pending: Default::default(),
                current: None,
            })
        }
    }
    /// Reads finished sets without waiting, then starts a new one.
    ///
    /// # Safety
    /// The timer's context is current.
    pub(crate) unsafe fn begin(&mut self) {
        while let Some((set, uploaded)) = self.pending.front().copied() {
            let mut ready = 0;
            // SAFETY: valid query names of the current context.
            unsafe { (self.available)(set[2], GL_QUERY_RESULT_AVAILABLE, &mut ready) };
            if ready == 0 {
                break;
            }
            let mut times = [0u64; 3];
            for (time, query) in times.iter_mut().zip(set) {
                // SAFETY: the last query of the set is available, so all are.
                unsafe { (self.result)(query, GL_QUERY_RESULT, time) };
            }
            if uploaded {
                record(Stage::GpuUpload, times[1].saturating_sub(times[0]));
                record(Stage::GpuComposite, times[2].saturating_sub(times[1]));
            } else {
                record(Stage::GpuComposite, times[2].saturating_sub(times[0]));
            }
            self.pending.pop_front();
            self.free.push((set, false));
        }
        self.current = self.free.pop();
        if let Some((set, _)) = self.current {
            // SAFETY: valid query name.
            unsafe { (self.counter)(set[0], GL_TIMESTAMP) };
        }
    }
    /// # Safety
    /// The timer's context is current.
    pub(crate) unsafe fn uploaded(&mut self) {
        if let Some((set, uploaded)) = self.current.as_mut() {
            *uploaded = true;
            // SAFETY: valid query name.
            unsafe { (self.counter)(set[1], GL_TIMESTAMP) };
        }
    }
    /// # Safety
    /// The timer's context is current.
    pub(crate) unsafe fn end(&mut self) {
        if let Some((set, uploaded)) = self.current.take() {
            // SAFETY: valid query names; without an upload the composite
            // starts where the set started.
            unsafe {
                if !uploaded {
                    (self.counter)(set[1], GL_TIMESTAMP);
                }
                (self.counter)(set[2], GL_TIMESTAMP);
            }
            self.pending.push_back((set, uploaded));
        }
    }
}
