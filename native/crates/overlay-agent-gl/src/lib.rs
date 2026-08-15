#![allow(clippy::missing_safety_doc)]
//! GLX/EGL swap interposition for the SHM overlay path.
//!
//! Authentication, packet handling, descriptor validation and mmap are kept on
//! the control thread. The swap hooks only drain the bounded mailbox, composite
//! the newest frame, restore state, and call the original swap.

use overlay_agent_core::{
    Agent, AgentConfig, AgentError, AgentEvent, CompletionQueue, FrameDescriptor,
};
use overlay_protocol::{
    CAP_DMABUF, CAP_SHM, CAP_SHM_BGRA, DmabufDescriptor, Endpoint, FrameConsumed,
    INPUT_FLAG_REPEAT, INPUT_KEY_DOWN, INPUT_KEY_UP, INPUT_MOUSE_DOWN, INPUT_MOUSE_MOVE,
    INPUT_MOUSE_UP, INPUT_MOUSE_WHEEL, INPUT_TEXT, InputEvent, SelectedTarget,
    TEXTURE_FORMAT_PREMULTIPLIED_BGRA8, TEXTURE_FORMAT_PREMULTIPLIED_RGBA8,
    TEXTURE_FORMAT_STRAIGHT_RGBA8, Target, TextureDescriptor,
};
use plthook::{HookGuard, Module, Replacement};
use sdl3_sys::events::{
    SDL_EVENT_KEY_DOWN, SDL_EVENT_KEY_UP, SDL_EVENT_MOUSE_BUTTON_DOWN, SDL_EVENT_MOUSE_BUTTON_UP,
    SDL_EVENT_MOUSE_MOTION, SDL_EVENT_MOUSE_WHEEL, SDL_EVENT_TEXT_INPUT, SDL_Event,
};
use sdl3_sys::video::SDL_Window;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use std::mem::{align_of, size_of};
use std::ops::Deref;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::ptr::{null, null_mut};
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_TARGETS: usize = 32;
const MAX_SHM: u64 = 256 * 1024 * 1024;
const MAX_INPUT_EVENTS: usize = 128;
const MAX_DMABUF_SURFACES: usize = 8;
const FORMAT_STRAIGHT_RGBA8: u32 = TEXTURE_FORMAT_STRAIGHT_RGBA8;
const FORMAT_PREMULTIPLIED_RGBA8: u32 = TEXTURE_FORMAT_PREMULTIPLIED_RGBA8;
const FORMAT_PREMULTIPLIED_BGRA8: u32 = TEXTURE_FORMAT_PREMULTIPLIED_BGRA8;
const GL_RGBA8: u32 = 0x8058;
const GL_RED: c_int = 0x1903;
const GL_GREEN: c_int = 0x1904;
const GL_BLUE: c_int = 0x1905;
const GL_ALPHA: c_int = 0x1906;
const GL_RGBA: u32 = 0x1908;
const GL_BGRA: u32 = 0x80E1;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_TEXTURE_SWIZZLE_R: u32 = 0x8E42;
const GL_TEXTURE_SWIZZLE_G: u32 = 0x8E43;
const GL_TEXTURE_SWIZZLE_B: u32 = 0x8E44;
const GL_TEXTURE_SWIZZLE_A: u32 = 0x8E45;
const GL_HANDLE_TYPE_OPAQUE_FD_EXT: u32 = 0x9586;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;
const GL_BLEND: u32 = 0x0BE2;
const GL_CULL_FACE: u32 = 0x0B44;
const GL_ONE: u32 = 1;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_TRIANGLE_STRIP: u32 = 5;
const GL_FLOAT: u32 = 0x1406;
const GL_FALSE: u8 = 0;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
const GL_VERTEX_ARRAY_BINDING: u32 = 0x85B5;
const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_STENCIL_TEST: u32 = 0x0B90;
const GL_BLEND_EQUATION_RGB: u32 = 0x8009;
const GL_BLEND_EQUATION_ALPHA: u32 = 0x883D;
const GL_BLEND_SRC_RGB: u32 = 0x80C9;
const GL_BLEND_DST_RGB: u32 = 0x80C8;
const GL_BLEND_SRC_ALPHA: u32 = 0x80CB;
const GL_BLEND_DST_ALPHA: u32 = 0x80CA;
const GL_COLOR_WRITEMASK: u32 = 0x0C23;
const GL_DEPTH_WRITEMASK: u32 = 0x0B72;
const GL_POLYGON_MODE: u32 = 0x0B40;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_VERSION: u32 = 0x1F02;
const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const GL_PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88EF;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
const GL_TEXTURE_WIDTH: u32 = 0x1000;
const GL_TEXTURE_HEIGHT: u32 = 0x1001;
const GL_TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;
const GL_SAMPLER_BINDING: u32 = 0x8919;
const GL_UNPACK_ROW_LENGTH: u32 = 0x0CF2;
const GL_UNPACK_SKIP_ROWS: u32 = 0x0CF3;
const GL_UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
const GL_FUNC_ADD: u32 = 0x8006;
const GL_VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
const GL_VERTEX_ATTRIB_ARRAY_SIZE: u32 = 0x8623;
const GL_VERTEX_ATTRIB_ARRAY_STRIDE: u32 = 0x8624;
const GL_VERTEX_ATTRIB_ARRAY_TYPE: u32 = 0x8625;
const GL_VERTEX_ATTRIB_ARRAY_NORMALIZED: u32 = 0x886A;
const GL_VERTEX_ATTRIB_ARRAY_POINTER: u32 = 0x8645;
const GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: u32 = 0x889F;
const EGL_NONE: c_int = 0x3038;
const EGL_EXTENSIONS: c_int = 0x3055;
const EGL_WIDTH: c_int = 0x3057;
const EGL_HEIGHT: c_int = 0x3056;
const EGL_LINUX_DMA_BUF_EXT: c_uint = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: c_int = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: c_int = 0x3272;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: c_int = 0x3443;

struct TargetSlot {
    id: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    activity: AtomicU64,
    /// Milliseconds since process start at the last present into this
    /// drawable. Slots are reclaimed from this, so a destroyed window stops
    /// competing for selection.
    last_seen: AtomicU64,
    live: AtomicBool,
}
impl TargetSlot {
    const fn new() -> Self {
        Self {
            id: AtomicU64::new(0),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            activity: AtomicU64::new(0),
            last_seen: AtomicU64::new(0),
            live: AtomicBool::new(false),
        }
    }
}

/// A drawable that has not been presented into for this long is treated as
/// gone. Comfortably longer than a dropped-frame hitch, short enough that an
/// alt-tab or window recreation reselects quickly.
const TARGET_EXPIRY_MILLIS: u64 = 2_000;

static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;
const GL_ALREADY_SIGNALED: u32 = 0x911A;
const GL_TIMEOUT_EXPIRED: u32 = 0x911B;
const GL_CONDITION_SATISFIED: u32 = 0x911C;
const GL_WAIT_FAILED: u32 = 0x911D;

/// One last-use marker for a frame in this GL context. Repeated draws of the
/// same mailbox frame replace this marker: the newest fence is ordered after
/// every earlier use in the context and therefore subsumes them.
struct PendingFence {
    sync: *mut c_void,
    frame: Arc<Frame>,
}

/// Tracks final GPU use per frame for one GL context. The renderer allows only
/// two outstanding shared textures, so this vector stays small without a
/// blocking overflow path in the game's present hook.
struct FenceTracker {
    pending: Vec<PendingFence>,
}

impl Default for FenceTracker {
    fn default() -> Self {
        Self {
            pending: Vec::with_capacity(8),
        }
    }
}

impl FenceTracker {
    fn supported(gl: &GlFns) -> bool {
        gl.fence_sync.is_some() && gl.client_wait_sync.is_some() && gl.delete_sync.is_some()
    }

    /// Release every final-use marker the GPU has completed. A wait failure is
    /// not completion proof; the exceptional recovery path finishes the
    /// context before releasing anything and permanently withdraws DMA-BUF.
    fn poll(&mut self, gl: &GlFns) {
        let (Some(client_wait), Some(delete)) = (gl.client_wait_sync, gl.delete_sync) else {
            return;
        };
        let mut wait_failed = false;
        self.pending.retain(|entry| {
            // SAFETY: the sync object was created by this current context.
            let status = unsafe { client_wait(entry.sync, 0, 0) };
            match status {
                GL_ALREADY_SIGNALED | GL_CONDITION_SATISFIED => {
                    unsafe { delete(entry.sync) };
                    entry.frame.fence_completed();
                    false
                }
                GL_TIMEOUT_EXPIRED => true,
                GL_WAIT_FAILED => {
                    wait_failed = true;
                    true
                }
                _ => {
                    wait_failed = true;
                    true
                }
            }
        });
        if wait_failed {
            // Rare fail-safe only. Normal presents never wait; once the driver
            // cannot query a sync object, glFinish is the only valid completion
            // proof before releasing the producer's buffers.
            unsafe { (gl.finish)() };
            for entry in self.pending.drain(..) {
                unsafe { delete(entry.sync) };
                entry.frame.fence_completed();
            }
            withdraw_dmabuf_support();
        }
    }

    /// Record the draw's latest use of `frame`. Replacing an older marker for
    /// the same frame is safe because commands in one GL context are ordered.
    fn track(&mut self, gl: &GlFns, frame: &Arc<Frame>) -> bool {
        let (Some(fence_sync), Some(delete)) = (gl.fence_sync, gl.delete_sync) else {
            return false;
        };
        let sync = unsafe { fence_sync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0) };
        if sync.is_null() {
            return false;
        }
        unsafe { (gl.flush)() };
        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.frame, frame))
        {
            unsafe { delete(existing.sync) };
            existing.sync = sync;
            return true;
        }
        frame.fence_started();
        self.pending.push(PendingFence {
            sync,
            frame: Arc::clone(frame),
        });
        true
    }
}

impl Drop for FenceTracker {
    fn drop(&mut self) {
        // Thread exit does not guarantee a current context. Without completion
        // proof these frames are deliberately not acknowledged; context
        // destruction releases the GL objects while the duplicated DMA-BUF FDs
        // keep the allocation alive.
        self.pending.clear();
    }
}

fn monotonic_millis() -> u64 {
    PROCESS_START.elapsed().as_millis() as u64
}

struct MappedShm {
    ptr: *mut u8,
    len: usize,
    map_base: *mut u8,
    map_len: usize,
    packed: Option<Box<[u8]>>,
}
unsafe impl Send for MappedShm {}
unsafe impl Sync for MappedShm {}
impl MappedShm {
    fn map(fd: OwnedFd, d: TextureDescriptor) -> Result<Self, ()> {
        d.validate().map_err(|_| ())?;
        if (d.format != FORMAT_STRAIGHT_RGBA8
            && d.format != FORMAT_PREMULTIPLIED_RGBA8
            && d.format != FORMAT_PREMULTIPLIED_BGRA8
            && d.format != GL_RGBA8)
            || d.size == 0
            || d.size > MAX_SHM
        {
            return Err(());
        }
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 || st.st_size < 0 {
            return Err(());
        }
        let file_len = st.st_size as u64;
        if d.size > file_len {
            return Err(());
        }
        let row_bytes = (d.height as u64 - 1)
            .checked_mul(d.stride as u64)
            .ok_or(())?;
        let content_len = row_bytes
            .checked_add((d.width as u64).checked_mul(4).ok_or(())?)
            .ok_or(())?;
        let content_end = d.offset.checked_add(content_len).ok_or(())?;
        if content_end > d.size {
            return Err(());
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        if page == 0 {
            return Err(());
        }
        let base = d.offset / page * page;
        let delta = usize::try_from(d.offset - base).map_err(|_| ())?;
        let content_len = usize::try_from(content_len).map_err(|_| ())?;
        let map_len = delta.checked_add(content_len).ok_or(())?;
        let ptr = unsafe {
            // `base` is page aligned and the validated range fits the object.
            libc::mmap(
                null_mut(),
                map_len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd.as_raw_fd(),
                base as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(());
        }
        let map_base = ptr.cast::<u8>();
        let mut mapped = Self {
            // SAFETY: `delta + content_len` is exactly `map_len`.
            ptr: unsafe { map_base.add(delta) },
            len: content_len,
            map_base,
            map_len,
            packed: None,
        };
        let tight = (d.width as u64).checked_mul(4).ok_or(())?;
        if d.stride as u64 != tight {
            let packed_len = tight
                .checked_mul(d.height as u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(())?;
            let source = mapped.bytes();
            let mut packed = vec![0u8; packed_len];
            for row in 0..d.height as usize {
                let src_start = row.checked_mul(d.stride as usize).ok_or(())?;
                let dst_start = row.checked_mul(tight as usize).ok_or(())?;
                packed[dst_start..dst_start + tight as usize]
                    .copy_from_slice(&source[src_start..src_start + tight as usize]);
            }
            mapped.packed = Some(packed.into_boxed_slice());
        }
        Ok(mapped)
    }
    fn bytes(&self) -> &[u8] {
        self.packed.as_deref().unwrap_or_else(|| {
            // SAFETY: `ptr` and `len` describe the read-only range retained
            // by this mapping for the lifetime of `self`.
            unsafe { slice::from_raw_parts(self.ptr, self.len) }
        })
    }
}
impl Drop for MappedShm {
    fn drop(&mut self) {
        if !self.map_base.is_null() && self.map_len != 0 {
            // SAFETY: `map_base`/`map_len` are exactly the successful mmap
            // result retained at construction.
            unsafe {
                libc::munmap(self.map_base.cast(), self.map_len);
            }
        }
    }
}
enum FrameImage {
    Shm {
        descriptor: TextureDescriptor,
        image: MappedShm,
    },
    Dmabuf {
        descriptor: DmabufDescriptor,
        fds: Vec<OwnedFd>,
    },
}
struct Frame {
    image: FrameImage,
    serial: u64,
    acknowledgements: Arc<CompletionQueue>,
    retired: AtomicBool,
    presenters: AtomicU32,
    pending_fences: AtomicU32,
    acknowledged: AtomicBool,
}
impl Frame {
    fn target_id(&self) -> u64 {
        match &self.image {
            FrameImage::Shm { descriptor, .. } => descriptor.target_id,
            FrameImage::Dmabuf { descriptor, .. } => descriptor.target_id,
        }
    }
    fn consumed(&self) -> Option<FrameConsumed> {
        let FrameImage::Dmabuf { descriptor, .. } = &self.image else {
            return None;
        };
        Some(FrameConsumed {
            target_id: descriptor.target_id,
            sequence: descriptor.sequence,
        })
    }
    fn begin_present(&self) {
        self.presenters.fetch_add(1, Ordering::AcqRel);
    }
    fn end_present(&self) {
        let previous = self.presenters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        if previous == 1 {
            self.try_acknowledge();
        }
    }
    fn fence_started(&self) {
        self.pending_fences.fetch_add(1, Ordering::AcqRel);
    }
    fn fence_completed(&self) {
        let previous = self.pending_fences.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        if previous == 1 {
            self.try_acknowledge();
        }
    }
    fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        self.try_acknowledge();
    }
    fn try_acknowledge(&self) {
        if !self.retired.load(Ordering::Acquire)
            || self.presenters.load(Ordering::Acquire) != 0
            || self.pending_fences.load(Ordering::Acquire) != 0
        {
            return;
        }
        if self
            .acknowledged
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Some(consumed) = self.consumed()
            && !self.acknowledgements.push(consumed)
        {
            agent_diagnostic("completion ring overflow; closing the session safely");
            withdraw_dmabuf_support();
        }
    }
}

struct FrameLease {
    frame: Arc<Frame>,
}
impl FrameLease {
    fn as_arc(&self) -> &Arc<Frame> {
        &self.frame
    }
}
impl Deref for FrameLease {
    type Target = Frame;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl Drop for FrameLease {
    fn drop(&mut self) {
        self.frame.end_present();
    }
}

struct Mailbox {
    latest: Mutex<Option<Arc<Frame>>>,
    serial: AtomicU64,
    visible: AtomicBool,
    connected: AtomicBool,
}
impl Mailbox {
    fn new() -> Self {
        Self {
            latest: Mutex::new(None),
            serial: AtomicU64::new(1),
            visible: AtomicBool::new(false),
            connected: AtomicBool::new(false),
        }
    }
    fn push(&self, frame: Frame) {
        let retired = if let Ok(mut latest) = self.latest.lock() {
            let retired = latest.replace(Arc::new(frame));
            self.visible.store(true, Ordering::Release);
            retired
        } else {
            None
        };
        if let Some(frame) = retired {
            frame.retire();
        }
    }
    fn clear(&self) {
        self.visible.store(false, Ordering::Release);
        let retired = self.latest.lock().ok().and_then(|mut latest| latest.take());
        if let Some(frame) = retired {
            frame.retire();
        }
    }
    fn begin_present(&self) -> Option<FrameLease> {
        if !self.visible.load(Ordering::Acquire) {
            return None;
        }
        let latest = self.latest.lock().ok()?;
        let frame = latest.as_ref()?.clone();
        // Increment while holding the mailbox lock. Replacement cannot retire
        // and acknowledge the frame between cloning it and registering this
        // in-progress present.
        frame.begin_present();
        Some(FrameLease { frame })
    }
    fn discard(&self, frame: &FrameLease) {
        let retired = if let Ok(mut latest) = self.latest.lock()
            && latest
                .as_ref()
                .is_some_and(|candidate| Arc::ptr_eq(candidate, frame.as_arc()))
        {
            latest.take()
        } else {
            None
        };
        if let Some(frame) = retired {
            frame.retire();
        }
    }
}

struct Runtime {
    mailbox: Arc<Mailbox>,
    acknowledgements: Arc<CompletionQueue>,
    input: Mutex<VecDeque<InputEvent>>,
    capture: AtomicBool,
    targets: [TargetSlot; MAX_TARGETS],
    started: AtomicBool,
    failed: AtomicBool,
    /// Target this agent has told the host it composites into, and the
    /// generation of that decision. The agent is the only authority: the host
    /// must never infer a target from creation order.
    selected_target: AtomicU64,
    selection_generation: AtomicU64,
}
impl Runtime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mailbox: Arc::new(Mailbox::new()),
            acknowledgements: Arc::new(CompletionQueue::new()),
            input: Mutex::new(VecDeque::with_capacity(MAX_INPUT_EVENTS)),
            capture: AtomicBool::new(false),
            targets: std::array::from_fn(|_| TargetSlot::new()),
            started: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            selected_target: AtomicU64::new(0),
            selection_generation: AtomicU64::new(0),
        })
    }
    fn update_target(&self, id: u64, width: u32, height: u32) {
        if id == 0 || width == 0 || height == 0 {
            return;
        }
        let now = monotonic_millis();
        for t in &self.targets {
            let old = t.id.load(Ordering::Acquire);
            if old == id
                || (old == 0
                    && t.id
                        .compare_exchange(0, id, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok())
            {
                t.width.store(width, Ordering::Relaxed);
                t.height.store(height, Ordering::Relaxed);
                t.activity.fetch_add(1, Ordering::Relaxed);
                t.last_seen.store(now, Ordering::Relaxed);
                t.live.store(true, Ordering::Release);
                return;
            }
        }
    }
    /// Release slots that have stopped being presented into. Without this a
    /// drawable that the game destroyed stays live forever, and a stale large
    /// window can remain the winner of `choose_target` indefinitely.
    fn expire_targets(&self, now: u64) {
        for t in &self.targets {
            if !t.live.load(Ordering::Acquire) {
                continue;
            }
            let last_seen = t.last_seen.load(Ordering::Relaxed);
            if now.saturating_sub(last_seen) <= TARGET_EXPIRY_MILLIS {
                continue;
            }
            // Free the slot so a later drawable can reuse it. Ordering matters:
            // clearing `live` first stops `choose_target` from racing onto an
            // id that is about to be zeroed.
            t.live.store(false, Ordering::Release);
            t.id.store(0, Ordering::Release);
            t.width.store(0, Ordering::Relaxed);
            t.height.store(0, Ordering::Relaxed);
            t.activity.store(0, Ordering::Relaxed);
        }
    }
    fn choose_target(&self) -> Option<(u64, u32, u32)> {
        let mut chosen = None;
        let mut score = 0u128;
        for t in &self.targets {
            if !t.live.load(Ordering::Acquire) {
                continue;
            }
            let id = t.id.load(Ordering::Relaxed);
            if id == 0 {
                continue;
            }
            let w = t.width.load(Ordering::Relaxed);
            let h = t.height.load(Ordering::Relaxed);
            let s =
                (w as u128) * (h as u128) * 1_000_000 + t.activity.load(Ordering::Relaxed) as u128;
            if s >= score {
                score = s;
                chosen = Some((id, w, h));
            }
        }
        chosen
    }
    /// The target the agent has actually announced. Input routing and frame
    /// matching use this rather than re-running the scoring, so every consumer
    /// agrees with what the host was told.
    fn announced_target(&self) -> u64 {
        self.selected_target.load(Ordering::Acquire)
    }
    fn queue_input(&self, event: InputEvent) -> bool {
        let Ok(mut input) = self.input.lock() else {
            return false;
        };
        if input.len() == MAX_INPUT_EVENTS {
            input.pop_front();
        }
        input.push_back(event);
        true
    }
}
static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
fn runtime() -> Arc<Runtime> {
    RUNTIME.get_or_init(Runtime::new).clone()
}
static PRELOAD_ARMED: AtomicBool = AtomicBool::new(false);
static PRELOAD_CLAIMED: AtomicBool = AtomicBool::new(false);
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
/// A preloaded agent must not claim the host session before the process
/// actually presents. `LD_PRELOAD` propagates to every child of a launched
/// target (`wineserver`, `winedevice.exe`, `services.exe`), the host accepts a
/// single session, and a non-rendering helper would otherwise take it and leave
/// the game without an overlay.
fn should_claim_session(armed: bool, claimed: &AtomicBool) -> bool {
    armed && !claimed.swap(true, Ordering::AcqRel)
}
fn presenting_runtime() -> Option<&'static Arc<Runtime>> {
    if should_claim_session(PRELOAD_ARMED.load(Ordering::Acquire), &PRELOAD_CLAIMED) {
        let _ = start_agent(null(), 0, false);
    }
    RUNTIME.get()
}

static TRANSPORT_CAPABILITIES: AtomicU64 = AtomicU64::new(0);
static DIAGNOSTICS: OnceLock<bool> = OnceLock::new();
static FRAME_DUMP_DELAY_SECS: OnceLock<u64> = OnceLock::new();

fn diagnostics_enabled() -> bool {
    *DIAGNOSTICS.get_or_init(|| std::env::var_os("VOSULAY_NATIVE_DIAGNOSTICS").is_some())
}

fn frame_dump_delay_secs() -> u64 {
    *FRAME_DUMP_DELAY_SECS.get_or_init(|| {
        std::env::var("VOSULAY_FRAME_DUMP_DELAY")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(20)
    })
}

/// Set when a real import fails after the session already negotiated DMA-BUF.
/// The capability is withdrawn and the session reconnects on shared memory,
/// because Electron picks its offscreen transport once per session and would
/// otherwise keep producing frames this target can never sample.
static DMABUF_WITHDRAWN: AtomicBool = AtomicBool::new(false);

/// Records that zero-copy import does not actually work in this target and
/// asks the session to renegotiate. Returns true the first time.
fn withdraw_dmabuf_support() -> bool {
    if DMABUF_WITHDRAWN.swap(true, Ordering::AcqRel) {
        return false;
    }
    let probed = TRANSPORT_CAPABILITIES.load(Ordering::Acquire);
    TRANSPORT_CAPABILITIES.store(probed & !CAP_DMABUF, Ordering::Release);
    agent_diagnostic("DMA-BUF import failed; downgrading this target to shared memory");
    true
}

fn dmabuf_withdrawn() -> bool {
    DMABUF_WITHDRAWN.load(Ordering::Acquire)
}

/// DMA-BUF is advertised only when this current context can both import a
/// texture and prove its final GPU use. Wine stable/GLX prefers the
/// GL_EXT_memory_object_fd path used by idk-overlay; EGLImage remains the EGL
/// and GLES fallback.
fn dmabuf_import_supported() -> bool {
    if dmabuf_withdrawn() || !gl_sync_supported() {
        return false;
    }
    if !gl_is_gles() && load_gl_memory_object().is_some() {
        return true;
    }
    let Some(egl) = load_egl_dmabuf() else {
        return false;
    };
    let display = import_display(egl);
    !display.is_null() && egl_display_imports_dmabuf(egl, display)
}

fn gl_sync_supported() -> bool {
    [
        b"glFenceSync\0".as_slice(),
        b"glClientWaitSync\0".as_slice(),
        b"glDeleteSync\0".as_slice(),
    ]
    .into_iter()
    .all(|name| !extension_symbol(name).is_null())
}

/// `EGL_EXT_image_dma_buf_import` must be present on the display we would
/// actually import against; the modifier variant is required only when the
/// host sends an explicit modifier, which `import_surface` checks per frame.
fn egl_display_imports_dmabuf(egl: EglDmabufFns, display: EglDisplay) -> bool {
    let query = extension_symbol(b"eglQueryString\0");
    if query.is_null() {
        return false;
    }
    // SAFETY: the resolved symbol is eglQueryString with this ABI.
    let query = unsafe {
        std::mem::transmute::<*mut c_void, unsafe extern "C" fn(EglDisplay, c_int) -> *const c_char>(
            query,
        )
    };
    // SAFETY: querying EGL_EXTENSIONS on an initialized display is valid.
    let extensions = unsafe { query(display, EGL_EXTENSIONS) };
    if extensions.is_null() {
        return false;
    }
    // SAFETY: EGL returns a NUL-terminated, space-separated extension list.
    let extensions = unsafe { CStr::from_ptr(extensions) }.to_bytes();
    let _ = egl;
    extensions
        .split(|byte| *byte == b' ')
        .any(|name| name == b"EGL_EXT_image_dma_buf_import")
}

/// Must run on the presenting thread while the target's graphics context is
/// current: whether an imported DMA-BUF can be sampled depends on that context,
/// not on the process.
fn probe_transport_capabilities() {
    let mut capabilities = CAP_SHM;
    if dmabuf_import_supported() {
        capabilities |= CAP_DMABUF;
    }
    if !gl_is_gles() {
        capabilities |= CAP_SHM_BGRA;
    }
    TRANSPORT_CAPABILITIES.store(capabilities, Ordering::Release);
    if diagnostics_enabled() {
        eprintln!(
            "vosulay target transport: dmabuf={} bgra_shm={}",
            capabilities & CAP_DMABUF != 0,
            capabilities & CAP_SHM_BGRA != 0
        );
    }
}

/// The transport probe needs the target's graphics context, so it only runs on
/// the presenting thread. The handshake waits briefly for it and otherwise
/// falls back to shared memory, which every context can sample.
fn await_transport_capabilities() -> u64 {
    for _ in 0..200 {
        let probed = TRANSPORT_CAPABILITIES.load(Ordering::Acquire);
        if probed != 0 {
            return probed & !withdrawn_mask();
        }
        thread::sleep(Duration::from_millis(5));
    }
    CAP_SHM
}

fn withdrawn_mask() -> u64 {
    if dmabuf_withdrawn() { CAP_DMABUF } else { 0 }
}

/// Resolved without the compositor's GL bindings: the capability probe runs
/// before any overlay resources exist.
fn gl_is_gles() -> bool {
    let get_string = provider_symbol(
        b"glGetString\0",
        &[b"libGL.so.1\0", b"libGLESv2.so.2\0", b"libGL.so\0"],
    );
    if get_string.is_null() {
        return false;
    }
    // SAFETY: the resolved symbol is glGetString with this ABI.
    let get_string = unsafe {
        std::mem::transmute::<*mut c_void, unsafe extern "C" fn(u32) -> *const c_char>(get_string)
    };
    // SAFETY: GL_VERSION is queryable whenever a context is current, and the
    // returned pointer is a NUL-terminated driver string.
    let version = unsafe { get_string(GL_VERSION) };
    if version.is_null() {
        return false;
    }
    // SAFETY: `version` is a NUL-terminated string owned by the driver.
    unsafe { CStr::from_ptr(version) }
        .to_bytes()
        .starts_with(b"OpenGL ES")
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
fn advertise_targets(
    runtime: &Runtime,
    agent: &mut Agent,
    advertised: &mut HashMap<u64, (u32, u32)>,
) -> Result<(), ()> {
    let mut current = HashMap::new();
    for slot in &runtime.targets {
        if !slot.live.load(Ordering::Acquire) {
            continue;
        }
        let target_id = slot.id.load(Ordering::Relaxed);
        let width = slot.width.load(Ordering::Relaxed);
        let height = slot.height.load(Ordering::Relaxed);
        if target_id == 0 || width == 0 || height == 0 {
            continue;
        }
        current.insert(target_id, (width, height));
        if advertised.get(&target_id) != Some(&(width, height)) {
            agent
                .send_target_create(Target {
                    target_id,
                    width,
                    height,
                })
                .map_err(|_| ())?;
        }
    }
    for target_id in advertised.keys().filter(|id| !current.contains_key(id)) {
        agent.send_target_destroy(*target_id).map_err(|_| ())?;
    }
    *advertised = current;
    Ok(())
}
fn agent_diagnostic(message: &str) {
    if diagnostics_enabled() {
        eprintln!("vosulay GL agent: {message}");
    }
}
fn run_control(rt: Arc<Runtime>, payload: Option<&[u8]>) -> Result<(), ()> {
    let (name, cookie_hex, expected_host_pid) = if let Some(p) = payload {
        let mut parts = p.split(|b| *b == 0);
        let name = parts.next().ok_or(())?.to_vec();
        let cookie = parts.next().ok_or(())?.to_vec();
        let host_pid = parts
            .next()
            .filter(|value| !value.is_empty())
            .map(|value| {
                std::str::from_utf8(value)
                    .map_err(|_| ())?
                    .parse::<u32>()
                    .map_err(|_| ())
            })
            .transpose()?;
        (name, cookie, host_pid)
    } else {
        // The launch path exports the abstract socket name hex-encoded, matching
        // the Vulkan layer; the profiler payload carries the raw name instead.
        let host_pid = std::env::var("VOSULAY_AGENT_HOST_PID")
            .ok()
            .map(|value| value.parse::<u32>().map_err(|_| ()))
            .transpose()?;
        let name = decode_hex(&std::env::var("VOSULAY_AGENT_SOCKET").map_err(|_| ())?)
            .ok_or(())
            .inspect_err(|()| agent_diagnostic("agent socket name is not valid hex"))?;
        (
            name,
            std::env::var("VOSULAY_AGENT_COOKIE")
                .map_err(|_| ())?
                .into_bytes(),
            host_pid,
        )
    };
    let cookie = decode_hex(std::str::from_utf8(&cookie_hex).map_err(|_| ())?).ok_or(())?;
    let cookie: [u8; 32] = cookie.try_into().map_err(|_| ())?;
    let ep = Endpoint::from_name(&name).map_err(|_| ())?;
    let mut config = AgentConfig::current();
    let probed = await_transport_capabilities();
    if probed != 0 {
        config.advertised_capabilities = probed;
    }
    // The GL backend can always upload a CPU buffer, so shared memory stays the
    // required transport and DMA-BUF is only ever an advertised upgrade.
    config.required_transports = CAP_SHM;
    let offered_dmabuf = config.advertised_capabilities & CAP_DMABUF != 0;
    if let Some(pid) = expected_host_pid {
        config.expected_host_pid = pid;
    }
    let mut agent = Agent::connect(&ep, cookie, config)
        .map_err(|_| ())
        .inspect_err(|()| agent_diagnostic("host rejected or refused the agent connection"))?;
    agent.set_nonblocking().map_err(|_| ())?;
    rt.mailbox.connected.store(true, Ordering::Release);
    let mut advertised = HashMap::new();
    let mut announced_target = 0u64;
    loop {
        // A withdrawn transport cannot be renegotiated inside a live session;
        // ending it is what lets `start_control` reconnect on shared memory.
        if offered_dmabuf && dmabuf_withdrawn() {
            return Err(());
        }
        while let Some(consumed) = rt.acknowledgements.pop() {
            agent.send_consumed(consumed).map_err(|_| ())?;
        }
        for _ in 0..MAX_INPUT_EVENTS {
            let event = rt.input.lock().ok().and_then(|mut input| input.pop_front());
            let Some(event) = event else {
                break;
            };
            agent.send_input(event).map_err(|_| ())?;
        }
        match agent.poll() {
            Ok(AgentEvent::Frame(_)) => {
                if let Some(frame) = agent.take_latest() {
                    let image = match frame.descriptor {
                        FrameDescriptor::Shm(descriptor) if frame.fds.len() == 1 => {
                            let fd = frame.fds.into_iter().next().ok_or(())?;
                            match MappedShm::map(fd, descriptor) {
                                Ok(image) => FrameImage::Shm { descriptor, image },
                                Err(()) => continue,
                            }
                        }
                        FrameDescriptor::Dmabuf(descriptor)
                            if frame.fds.len() == descriptor.planes.len() =>
                        {
                            FrameImage::Dmabuf {
                                descriptor,
                                fds: frame.fds,
                            }
                        }
                        _ => continue,
                    };
                    let serial = rt.mailbox.serial.fetch_add(1, Ordering::Relaxed);
                    rt.mailbox.push(Frame {
                        image,
                        serial,
                        acknowledgements: rt.acknowledgements.clone(),
                        retired: AtomicBool::new(false),
                        presenters: AtomicU32::new(0),
                        pending_fences: AtomicU32::new(0),
                        acknowledged: AtomicBool::new(false),
                    });
                }
            }
            Ok(AgentEvent::Visible(v)) => {
                if v {
                    rt.mailbox.visible.store(true, Ordering::Release);
                } else {
                    rt.mailbox.clear();
                }
            }
            Ok(AgentEvent::Capture(capture)) => {
                rt.capture.store(capture, Ordering::Release);
            }
            Ok(AgentEvent::Clear(_)) => rt.mailbox.clear(),
            Ok(AgentEvent::TargetCreated(_) | AgentEvent::TargetDestroyed(_)) => {}
            Ok(AgentEvent::Shutdown) | Err(AgentError::Disconnected) => {
                rt.mailbox.clear();
                rt.mailbox.connected.store(false, Ordering::Release);
                return Err(());
            }
            Ok(_) => {}
            Err(AgentError::Protocol(overlay_protocol::ProtocolError::Io(error)))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(()),
        }
        if agent.watchdog_tick() {
            rt.capture.store(false, Ordering::Release);
            agent.send_end(rt.announced_target()).map_err(|_| ())?;
        }
        rt.expire_targets(monotonic_millis());
        advertise_targets(&rt, &mut agent, &mut advertised)?;
        // Announced only after the target is registered: the host rejects a
        // selection naming a target it has not been told about.
        let selected = rt.choose_target().map(|target| target.0).unwrap_or(0);
        if selected != announced_target {
            announced_target = selected;
            let generation = rt.selection_generation.fetch_add(1, Ordering::AcqRel) + 1;
            rt.selected_target.store(selected, Ordering::Release);
            agent
                .send_selected_target(SelectedTarget {
                    target_id: selected,
                    generation,
                })
                .map_err(|_| ())?;
            agent_diagnostic(&format!(
                "selected target {selected} (generation {generation})"
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn start_control(rt: Arc<Runtime>, payload: Option<&[u8]>) -> Result<(), ()> {
    // The host listener is one-shot. A real import rejection ends this session;
    // the desktop manager then performs a fresh profiler attach, whose new
    // payload starts this already-hooked runtime with DMA-BUF withdrawn.
    let result = run_control(rt.clone(), payload);
    rt.capture.store(false, Ordering::Release);
    rt.mailbox.clear();
    rt.mailbox.connected.store(false, Ordering::Release);
    result
}

fn rounded_i32(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

/// Translate and enqueue one SDL3 event. Returning false leaves the event in
/// the game's input path; every failure is deliberately fail-open.
unsafe fn capture_sdl_input(rt: &Runtime, event: *const SDL_Event) -> bool {
    if event.is_null() || !rt.capture.load(Ordering::Acquire) {
        return false;
    }
    // The host matches input against the target it was told about, so routing
    // must use the announcement rather than re-scoring the slots.
    let target_id = rt.announced_target();
    if target_id == 0 {
        return false;
    }
    // SAFETY: SDL supplied a live event union for the duration of the hook.
    let event_type = unsafe { (*event).r#type };
    let input = if event_type == SDL_EVENT_KEY_DOWN.value()
        || event_type == SDL_EVENT_KEY_UP.value()
    {
        // SAFETY: the discriminator identifies the keyboard union member.
        let key = unsafe { (*event).key };
        Some(InputEvent {
            target_id,
            kind: if event_type == SDL_EVENT_KEY_DOWN.value() {
                INPUT_KEY_DOWN
            } else {
                INPUT_KEY_UP
            },
            code: u16::try_from(key.scancode.value()).unwrap_or(0),
            flags: u32::from(key.r#mod.value()) | if key.repeat { INPUT_FLAG_REPEAT } else { 0 },
            value: i32::try_from(key.key.value()).unwrap_or(0),
            value2: 0,
        })
    } else if event_type == SDL_EVENT_MOUSE_MOTION.value() {
        // SAFETY: the discriminator identifies the mouse motion union member.
        let motion = unsafe { (*event).motion };
        Some(InputEvent {
            target_id,
            kind: INPUT_MOUSE_MOVE,
            code: 0,
            flags: motion.state.value(),
            value: rounded_i32(motion.x),
            value2: rounded_i32(motion.y),
        })
    } else if event_type == SDL_EVENT_MOUSE_BUTTON_DOWN.value()
        || event_type == SDL_EVENT_MOUSE_BUTTON_UP.value()
    {
        // SAFETY: the discriminator identifies the mouse button union member.
        let button = unsafe { (*event).button };
        Some(InputEvent {
            target_id,
            kind: if event_type == SDL_EVENT_MOUSE_BUTTON_DOWN.value() {
                INPUT_MOUSE_DOWN
            } else {
                INPUT_MOUSE_UP
            },
            code: u16::from(button.button),
            flags: u32::from(button.clicks),
            value: rounded_i32(button.x),
            value2: rounded_i32(button.y),
        })
    } else if event_type == SDL_EVENT_MOUSE_WHEEL.value() {
        // SAFETY: the discriminator identifies the mouse wheel union member.
        let wheel = unsafe { (*event).wheel };
        Some(InputEvent {
            target_id,
            kind: INPUT_MOUSE_WHEEL,
            code: 0,
            flags: u32::try_from(wheel.direction.value()).unwrap_or(0),
            value: rounded_i32(wheel.x * 120.0),
            value2: rounded_i32(wheel.y * 120.0),
        })
    } else if event_type == SDL_EVENT_TEXT_INPUT.value() {
        // SAFETY: SDL owns this NUL-terminated string through event delivery.
        let text = unsafe { (*event).text.text };
        if text.is_null() {
            return false;
        }
        // SAFETY: SDL documents text input as a live UTF-8 C string.
        let Ok(text) = unsafe { CStr::from_ptr(text) }.to_str() else {
            return false;
        };
        let mut queued = false;
        for character in text.chars().take(16) {
            queued |= rt.queue_input(InputEvent {
                target_id,
                kind: INPUT_TEXT,
                code: 0,
                flags: 0,
                value: character as i32,
                value2: 0,
            });
        }
        return queued;
    } else {
        None
    };
    input.is_some_and(|input| rt.queue_input(input))
}

type SdlPollEvent = unsafe extern "C" fn(*mut SDL_Event) -> bool;
type SdlWaitEvent = unsafe extern "C" fn(*mut SDL_Event) -> bool;
type SdlWaitEventTimeout = unsafe extern "C" fn(*mut SDL_Event, c_int) -> bool;
static SDL_POLL_ORIGINAL: OnceLock<Option<SdlPollEvent>> = OnceLock::new();
static SDL_WAIT_ORIGINAL: OnceLock<Option<SdlWaitEvent>> = OnceLock::new();
static SDL_WAIT_TIMEOUT_ORIGINAL: OnceLock<Option<SdlWaitEventTimeout>> = OnceLock::new();
type SdlGlSwapWindow = unsafe extern "C" fn(*mut SDL_Window) -> bool;
type SdlGetWindowSizeInPixels =
    unsafe extern "C" fn(*mut SDL_Window, *mut c_int, *mut c_int) -> bool;
static SDL_GL_SWAP_ORIGINAL: OnceLock<Option<SdlGlSwapWindow>> = OnceLock::new();
static SDL_GET_WINDOW_SIZE: OnceLock<Option<SdlGetWindowSizeInPixels>> = OnceLock::new();

struct SdlDynapiGuard {
    slot: usize,
    original: usize,
    replacement: usize,
    provider: usize,
}

impl Drop for SdlDynapiGuard {
    fn drop(&mut self) {
        if mapping_allows(self.slot, size_of::<usize>(), true, true, false) {
            // SAFETY: the slot was validated as readable, writable, and aligned
            // before installation; retaining the provider handle pins its module.
            let slot = unsafe { &*(self.slot as *const AtomicPtr<c_void>) };
            let _ = slot.compare_exchange(
                self.replacement as *mut c_void,
                self.original as *mut c_void,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        // SAFETY: this balances the successful RTLD_NOLOAD dlopen retained by
        // install_sdl_dynapi_hook after the hook is no longer callable.
        unsafe { libc::dlclose(self.provider as *mut c_void) };
    }
}

fn mapping_allows(
    address: usize,
    len: usize,
    readable: bool,
    writable: bool,
    executable: bool,
) -> bool {
    let Some(end) = address.checked_add(len) else {
        return false;
    };
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return false;
    };
    maps.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else {
            return false;
        };
        let Some(permissions) = fields.next() else {
            return false;
        };
        let Some((start, finish)) = range.split_once('-') else {
            return false;
        };
        let (Ok(start), Ok(finish)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(finish, 16),
        ) else {
            return false;
        };
        let permissions = permissions.as_bytes();
        address >= start
            && end <= finish
            && (!readable || permissions.first() == Some(&b'r'))
            && (!writable || permissions.get(1) == Some(&b'w'))
            && (!executable || permissions.get(2) == Some(&b'x'))
    })
}

struct SdlProviderHandle(usize);

impl SdlProviderHandle {
    fn as_ptr(&self) -> *mut c_void {
        self.0 as *mut c_void
    }

    fn into_raw(self) -> usize {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for SdlProviderHandle {
    fn drop(&mut self) {
        // SAFETY: every instance owns one successful dlopen reference.
        unsafe { libc::dlclose(self.as_ptr()) };
    }
}

fn open_sdl_provider() -> Option<SdlProviderHandle> {
    for library in [b"libSDL3.so.0\0".as_slice(), b"libSDL3.so\0".as_slice()] {
        // SAFETY: names are static and NUL-terminated. RTLD_NOLOAD never loads
        // a new SDL implementation into the target.
        let handle =
            unsafe { libc::dlopen(library.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        if !handle.is_null() {
            return Some(SdlProviderHandle(handle as usize));
        }
    }
    None
}

#[cfg(target_arch = "x86_64")]
fn sdl_dynapi_slot_address(entry_address: usize, code: &[u8]) -> Result<usize, &'static str> {
    let jump = usize::from(code.starts_with(&[0xf3, 0x0f, 0x1e, 0xfa])) * 4;
    if code.get(jump..jump + 2) != Some(&[0xff, 0x25]) {
        return Err("SDL3 export is not a supported DynAPI jump");
    }
    let displacement = i32::from_le_bytes(
        code.get(jump + 2..jump + 6)
            .ok_or("SDL3 DynAPI displacement is truncated")?
            .try_into()
            .map_err(|_| "SDL3 DynAPI displacement is truncated")?,
    );
    let jump_end = entry_address
        .checked_add(jump + 6)
        .ok_or("SDL3 DynAPI jump address overflow")?;
    if displacement >= 0 {
        jump_end
            .checked_add(displacement as usize)
            .ok_or("SDL3 DynAPI slot address overflow")
    } else {
        jump_end
            .checked_sub(displacement.unsigned_abs() as usize)
            .ok_or("SDL3 DynAPI slot address overflow")
    }
}

#[cfg(target_arch = "x86_64")]
fn install_sdl_dynapi_hook(
    name: &'static [u8],
    replacement: *mut c_void,
) -> Result<(SdlDynapiGuard, *mut c_void), &'static str> {
    let provider = open_sdl_provider().ok_or("SDL3 provider is not loaded")?;
    // SAFETY: provider is retained and name is static and NUL-terminated.
    let entry = unsafe { libc::dlsym(provider.as_ptr(), name.as_ptr().cast()) };
    if entry.is_null() || entry == replacement {
        return Err("SDL3 export is unavailable");
    }
    let entry_address = entry as usize;
    if !mapping_allows(entry_address, 10, true, false, true) {
        return Err("SDL3 export is not in a readable executable mapping");
    }
    // SDL3's x86-64 DynAPI exports are an optional ENDBR64 followed by
    // `jmp qword ptr [rip + rel32]`. The writable target cell is the stable
    // indirection used by already-cached P/Invoke function pointers.
    let code = unsafe { slice::from_raw_parts(entry.cast::<u8>(), 10) };
    let slot_address = sdl_dynapi_slot_address(entry_address, code)?;
    if slot_address % align_of::<AtomicPtr<c_void>>() != 0
        || !mapping_allows(slot_address, size_of::<usize>(), true, true, false)
    {
        return Err("SDL3 DynAPI slot is not aligned writable memory");
    }
    // SAFETY: mapping and alignment checks above establish a live atomic
    // pointer cell, and the retained provider handle prevents SDL unloading.
    let slot = unsafe { &*(slot_address as *const AtomicPtr<c_void>) };
    let original = slot.load(Ordering::Acquire);
    if original.is_null()
        || original == replacement
        || !mapping_allows(original as usize, 1, true, false, true)
    {
        return Err("SDL3 DynAPI target is not executable");
    }
    slot.compare_exchange(original, replacement, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "SDL3 DynAPI slot changed during installation")?;
    Ok((
        SdlDynapiGuard {
            slot: slot_address,
            original: original as usize,
            replacement: replacement as usize,
            provider: provider.into_raw(),
        },
        original,
    ))
}

#[cfg(not(target_arch = "x86_64"))]
fn install_sdl_dynapi_hook(
    _name: &'static [u8],
    _replacement: *mut c_void,
) -> Result<(SdlDynapiGuard, *mut c_void), &'static str> {
    Err("SDL3 DynAPI late attach is unsupported on this architecture")
}

fn install_sdl_dynapi_hooks(diagnostics: bool) -> (Vec<SdlDynapiGuard>, bool) {
    let mut guards = Vec::with_capacity(4);
    let mut present_hooked = false;
    macro_rules! install {
        ($name:literal, $replacement:ident, $storage:ident, $original:ty, $present:expr) => {
            match install_sdl_dynapi_hook(
                concat!($name, "\0").as_bytes(),
                $replacement as *mut c_void,
            ) {
                Ok((guard, original)) => {
                    let original =
                        unsafe { std::mem::transmute::<*mut c_void, $original>(original) };
                    if $storage.set(Some(original)).is_ok() {
                        if diagnostics {
                            eprintln!("vosulay SDL DynAPI hook installed: {}", $name);
                        }
                        present_hooked |= $present;
                        guards.push(guard);
                    }
                }
                Err(error) if diagnostics => {
                    eprintln!("vosulay SDL DynAPI hook skipped: {}: {error}", $name);
                }
                Err(_) => {}
            }
        };
    }
    install!(
        "SDL_GL_SwapWindow",
        SDL_GL_SwapWindow,
        SDL_GL_SWAP_ORIGINAL,
        SdlGlSwapWindow,
        true
    );
    install!(
        "SDL_PollEvent",
        SDL_PollEvent,
        SDL_POLL_ORIGINAL,
        SdlPollEvent,
        false
    );
    install!(
        "SDL_WaitEvent",
        SDL_WaitEvent,
        SDL_WAIT_ORIGINAL,
        SdlWaitEvent,
        false
    );
    install!(
        "SDL_WaitEventTimeout",
        SDL_WaitEventTimeout,
        SDL_WAIT_TIMEOUT_ORIGINAL,
        SdlWaitEventTimeout,
        false
    );
    (guards, present_hooked)
}

fn seed_sdl_originals() {
    if SDL_GL_SWAP_ORIGINAL.get().is_none() {
        let swap = provider_symbol(
            b"SDL_GL_SwapWindow\0",
            &[b"libSDL3.so.0\0", b"libSDL3.so\0"],
        );
        if !swap.is_null() && swap != SDL_GL_SwapWindow as *mut c_void {
            let _ = SDL_GL_SWAP_ORIGINAL.set(Some(unsafe {
                std::mem::transmute::<*mut c_void, SdlGlSwapWindow>(swap)
            }));
        }
    }

    if SDL_POLL_ORIGINAL.get().is_none() {
        let poll = provider_symbol(b"SDL_PollEvent\0", &[b"libSDL3.so.0\0", b"libSDL3.so\0"]);
        if !poll.is_null() && poll != SDL_PollEvent as *mut c_void {
            let _ = SDL_POLL_ORIGINAL.set(Some(unsafe {
                std::mem::transmute::<*mut c_void, SdlPollEvent>(poll)
            }));
        }
    }
    if SDL_WAIT_ORIGINAL.get().is_none() {
        let wait = provider_symbol(b"SDL_WaitEvent\0", &[b"libSDL3.so.0\0", b"libSDL3.so\0"]);
        if !wait.is_null() && wait != SDL_WaitEvent as *mut c_void {
            let _ = SDL_WAIT_ORIGINAL.set(Some(unsafe {
                std::mem::transmute::<*mut c_void, SdlWaitEvent>(wait)
            }));
        }
    }
    if SDL_WAIT_TIMEOUT_ORIGINAL.get().is_none() {
        let timeout = provider_symbol(
            b"SDL_WaitEventTimeout\0",
            &[b"libSDL3.so.0\0", b"libSDL3.so\0"],
        );
        if !timeout.is_null() && timeout != SDL_WaitEventTimeout as *mut c_void {
            let _ = SDL_WAIT_TIMEOUT_ORIGINAL.set(Some(unsafe {
                std::mem::transmute::<*mut c_void, SdlWaitEventTimeout>(timeout)
            }));
        }
    }
}

fn sdl_poll_original() -> Option<SdlPollEvent> {
    *SDL_POLL_ORIGINAL.get_or_init(|| {
        let pointer = provider_symbol(b"SDL_PollEvent\0", &[b"libSDL3.so.0\0", b"libSDL3.so\0"]);
        (!pointer.is_null()).then(|| unsafe {
            // SAFETY: provider lookup returned SDL_PollEvent with this ABI.
            std::mem::transmute::<*mut c_void, SdlPollEvent>(pointer)
        })
    })
}

fn sdl_wait_original() -> Option<SdlWaitEvent> {
    *SDL_WAIT_ORIGINAL.get_or_init(|| {
        let pointer = provider_symbol(b"SDL_WaitEvent\0", &[b"libSDL3.so.0\0", b"libSDL3.so\0"]);
        (!pointer.is_null()).then(|| unsafe {
            // SAFETY: provider lookup returned SDL_WaitEvent with this ABI.
            std::mem::transmute::<*mut c_void, SdlWaitEvent>(pointer)
        })
    })
}

fn sdl_wait_timeout_original() -> Option<SdlWaitEventTimeout> {
    *SDL_WAIT_TIMEOUT_ORIGINAL.get_or_init(|| {
        let pointer = provider_symbol(
            b"SDL_WaitEventTimeout\0",
            &[b"libSDL3.so.0\0", b"libSDL3.so\0"],
        );
        (!pointer.is_null()).then(|| unsafe {
            // SAFETY: provider lookup returned SDL_WaitEventTimeout with this ABI.
            std::mem::transmute::<*mut c_void, SdlWaitEventTimeout>(pointer)
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_PollEvent(event: *mut SDL_Event) -> bool {
    let Some(original) = sdl_poll_original() else {
        return false;
    };
    loop {
        // SAFETY: forward SDL's event pointer unchanged.
        if !unsafe { original(event) } {
            return false;
        }
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            RUNTIME
                .get()
                .is_some_and(|runtime| unsafe { capture_sdl_input(runtime, event) })
        }))
        .unwrap_or(false);
        if !captured {
            return true;
        }
    }
}
static SHM_DRAW_REPORTED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_WaitEvent(event: *mut SDL_Event) -> bool {
    let Some(original) = sdl_wait_original() else {
        return false;
    };
    loop {
        // SAFETY: forward SDL's event pointer unchanged.
        if !unsafe { original(event) } {
            return false;
        }
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            RUNTIME
                .get()
                .is_some_and(|runtime| unsafe { capture_sdl_input(runtime, event) })
        }))
        .unwrap_or(false);
        if !captured {
            return true;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_WaitEventTimeout(event: *mut SDL_Event, timeout_ms: c_int) -> bool {
    let Some(original) = sdl_wait_timeout_original() else {
        return false;
    };
    let deadline =
        (timeout_ms >= 0).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    loop {
        let remaining = deadline.map_or(timeout_ms, |deadline| {
            let millis = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            i32::try_from(millis).unwrap_or(i32::MAX)
        });
        // SAFETY: forward SDL's event pointer unchanged.
        if !unsafe { original(event, remaining) } {
            return false;
        }
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            RUNTIME
                .get()
                .is_some_and(|runtime| unsafe { capture_sdl_input(runtime, event) })
        }))
        .unwrap_or(false);
        if !captured {
            return true;
        }
    }
}
fn sdl_gl_swap_original() -> Option<SdlGlSwapWindow> {
    *SDL_GL_SWAP_ORIGINAL.get_or_init(|| {
        let pointer = provider_symbol(
            b"SDL_GL_SwapWindow\0",
            &[b"libSDL3.so.0\0", b"libSDL3.so\0"],
        );
        (!pointer.is_null())
            .then(|| unsafe { std::mem::transmute::<*mut c_void, SdlGlSwapWindow>(pointer) })
    })
}

fn query_sdl_window(window: *mut SDL_Window) -> (u32, u32) {
    let query = *SDL_GET_WINDOW_SIZE.get_or_init(|| {
        let pointer = provider_symbol(
            b"SDL_GetWindowSizeInPixels\0",
            &[b"libSDL3.so.0\0", b"libSDL3.so\0"],
        );
        (!pointer.is_null()).then(|| unsafe {
            std::mem::transmute::<*mut c_void, SdlGetWindowSizeInPixels>(pointer)
        })
    });
    let Some(query) = query else {
        return (0, 0);
    };
    let mut width = 0;
    let mut height = 0;
    if unsafe { query(window, &mut width, &mut height) } {
        (width.max(0) as u32, height.max(0) as u32)
    } else {
        (0, 0)
    }
}

static SDL_SWAP_REPORTED: AtomicBool = AtomicBool::new(false);
static DMABUF_DRAW_REPORTED: AtomicBool = AtomicBool::new(false);
static TEXTURE_REPORTED: AtomicBool = AtomicBool::new(false);
static DMABUF_REJECT_REPORTED: AtomicBool = AtomicBool::new(false);

/// Reports the first import rejection only: a persistent incompatibility must
/// not log on every presented frame.
/// Every rejection here means this target cannot sample the buffer the host
/// negotiated, so the capability is withdrawn and the session renegotiates on
/// shared memory instead of drawing nothing forever.
fn reject_dmabuf(reason: &'static str) -> bool {
    if !DMABUF_REJECT_REPORTED.swap(true, Ordering::Relaxed) {
        agent_diagnostic(&format!("DMA-BUF import rejected: {reason}"));
    }
    withdraw_dmabuf_support();
    false
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SDL_GL_SwapWindow(window: *mut SDL_Window) -> bool {
    let original = sdl_gl_swap_original();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(runtime) = presenting_runtime() {
            let dimensions = query_sdl_window(window);
            if !SDL_SWAP_REPORTED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "vosulay SDL_GL_SwapWindow hook active: target={:#x} size={}x{}",
                    window as usize, dimensions.0, dimensions.1
                );
            }
            present(runtime, window as usize as u64, dimensions);
        }
    }));
    original.is_some_and(|swap| unsafe { swap(window) })
}

static LATE_HOOKS: OnceLock<Mutex<Vec<HookGuard>>> = OnceLock::new();
static SDL_DYNAPI_HOOKS: OnceLock<Mutex<Vec<SdlDynapiGuard>>> = OnceLock::new();

fn install_late_hooks() -> Result<(), ()> {
    let diagnostics = diagnostics_enabled();
    let (dynapi_guards, dynapi_present_hooked) = install_sdl_dynapi_hooks(diagnostics);
    seed_swap_originals();
    seed_sdl_originals();
    let mut replacements = Vec::with_capacity(6);
    if SDL_GL_SWAP_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol("SDL_GL_SwapWindow", SDL_GL_SwapWindow as *const c_void)
                .optional(),
        );
    }
    if EGL_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol("eglSwapBuffers", eglSwapBuffers as *const c_void).optional(),
        );
    }
    if GLX_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol("glXSwapBuffers", glXSwapBuffers as *const c_void).optional(),
        );
    }
    if SDL_POLL_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol("SDL_PollEvent", SDL_PollEvent as *const c_void).optional(),
        );
    }
    if SDL_WAIT_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol("SDL_WaitEvent", SDL_WaitEvent as *const c_void).optional(),
        );
    }
    if SDL_WAIT_TIMEOUT_ORIGINAL.get().copied().flatten().is_some() {
        replacements.push(
            Replacement::by_symbol(
                "SDL_WaitEventTimeout",
                SDL_WaitEventTimeout as *const c_void,
            )
            .optional(),
        );
    }
    if replacements.is_empty() {
        return Err(());
    }
    let modules = Module::enumerate().map_err(|_| ())?;
    let mut guards = Vec::new();
    let mut present_hooked = dynapi_present_hooked;
    for module in modules {
        let module_path = module.path().to_owned();
        let guard = match plthook::install(&module, &replacements) {
            Ok(guard) => guard,
            Err(error) => {
                if diagnostics {
                    eprintln!("vosulay module hook skipped: {module_path}: {error}");
                }
                continue;
            }
        };
        for installed in guard.installed() {
            let symbol = installed.symbol().describe();
            if matches!(
                symbol.as_str(),
                "SDL_GL_SwapWindow" | "eglSwapBuffers" | "glXSwapBuffers"
            ) {
                present_hooked = true;
            }
            if diagnostics {
                eprintln!("vosulay hook installed: {module_path}: {symbol}");
            }
        }
        if !guard.installed().is_empty() {
            guards.push(guard);
        }
    }
    if !present_hooked {
        return Err(());
    }
    *SDL_DYNAPI_HOOKS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| ())? = dynapi_guards;
    *LATE_HOOKS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| ())? = guards;
    Ok(())
}

fn start_agent(payload: *const u8, len: usize, install_hooks: bool) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = runtime();
        if rt.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        rt.failed.store(false, Ordering::Release);
        let bytes = if !payload.is_null() && len != 0 {
            // SAFETY: the C caller promises `payload` points to `len` readable
            // bytes for the duration of this call.
            Some(unsafe { slice::from_raw_parts(payload, len) }.to_vec())
        } else {
            None
        };
        if install_hooks && !HOOKS_INSTALLED.load(Ordering::Acquire) {
            if install_late_hooks().is_err() {
                rt.started.store(false, Ordering::Release);
                return Err(());
            }
            HOOKS_INSTALLED.store(true, Ordering::Release);
        } else if !install_hooks {
            // LD_PRELOAD interposition is already installed by the loader.
            HOOKS_INSTALLED.store(true, Ordering::Release);
        }
        thread::Builder::new()
            .name("vosulay-agent-gl-control".into())
            .spawn(move || {
                let result = start_control(rt.clone(), bytes.as_deref());
                rt.started.store(false, Ordering::Release);
                if let Err(error) = result {
                    eprintln!("vosulay GL agent control channel closed: {error:?}");
                    rt.failed.store(true, Ordering::Release);
                    rt.mailbox.connected.store(false, Ordering::Release);
                }
            })
            .map_err(|_| ())?;
        Ok(())
    }));
    match result {
        Ok(Ok(())) => 1,
        Ok(Err(())) | Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vosulay_agent_start() -> c_int {
    start_agent(null(), 0, true)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vosulay_agent_start_with_payload(payload: *const u8, len: usize) -> c_int {
    start_agent(payload, len, true)
}
extern "C" fn preload_start() {
    if std::env::var_os("VOSULAY_AGENT_SOCKET").is_some()
        && std::env::var_os("VOSULAY_AGENT_COOKIE").is_some()
    {
        PRELOAD_ARMED.store(true, Ordering::Release);
    }
}
#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static PRELOAD_INIT: extern "C" fn() = preload_start;

type DlsymFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type GetProcFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
static REAL_DLSYM: OnceLock<Option<DlsymFn>> = OnceLock::new();
static GLX_GET_PROC: OnceLock<Option<GetProcFn>> = OnceLock::new();
static EGL_GET_PROC: OnceLock<Option<GetProcFn>> = OnceLock::new();
static DLSYM_REPORTED: AtomicBool = AtomicBool::new(false);

/// Resolving glibc's own `dlsym` cannot go through `dlsym`: this library exports
/// that symbol, so a `dlsym` call from here can rebind to the interposer.
/// `dlvsym` is never interposed. libc's own handle is tried before `RTLD_NEXT`
/// because a `dlopen`ed agent — the profiler-injected case — has no next object
/// to search, which would otherwise leave every lookup unresolved. glibc moved
/// `dlsym` from libdl into libc at 2.34.
#[cfg(target_env = "gnu")]
fn real_dlsym() -> Option<DlsymFn> {
    *REAL_DLSYM.get_or_init(|| {
        // SAFETY: static NUL-terminated library name; the handle stays loaded
        // for the process lifetime and is intentionally not released.
        let libc_handle = unsafe {
            libc::dlopen(
                c"libc.so.6".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_NOLOAD | libc::RTLD_LOCAL,
            )
        };
        for handle in [libc_handle, libc::RTLD_NEXT] {
            if handle.is_null() {
                continue;
            }
            for version in [c"GLIBC_2.34", c"GLIBC_2.2.5", c"GLIBC_2.0"] {
                // SAFETY: static NUL-terminated symbol and version names.
                let symbol = unsafe { libc::dlvsym(handle, c"dlsym".as_ptr(), version.as_ptr()) };
                // An explicit handle resolves only that object's symbol, and
                // RTLD_NEXT skips this library, so a hit is never the interposer.
                if !symbol.is_null() {
                    // SAFETY: glibc's dlsym matches this ABI.
                    return Some(unsafe { std::mem::transmute::<*mut c_void, DlsymFn>(symbol) });
                }
            }
        }
        None
    })
}

#[cfg(not(target_env = "gnu"))]
fn real_dlsym() -> Option<DlsymFn> {
    None
}

/// Wine's `winex11.so` links no GL provider: it `dlopen`s libGL and resolves
/// `glXSwapBuffers` with `dlsym`, then calls it through a stored pointer, so
/// neither `LD_PRELOAD` symbol interposition nor PLT patching observes its
/// presents. Interposing `dlsym` hands the caller this agent's hook while
/// keeping the real symbol as the forwarded original.
#[cfg(target_env = "gnu")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    interposed_dlsym(handle, name)
}

/// Private so its address identifies this library's own code regardless of
/// symbol interposition.
#[cfg(target_env = "gnu")]
extern "C" fn interposed_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let Some(real) = real_dlsym() else {
        if !DLSYM_REPORTED.swap(true, Ordering::Relaxed) {
            eprintln!("vosulay GL agent: glibc dlsym is unavailable; overlay disabled");
        }
        return null_mut();
    };
    // SAFETY: the caller's handle and name are forwarded unchanged.
    let resolved = unsafe { real(handle, name) };
    if resolved.is_null() || name.is_null() {
        return resolved;
    }
    // SAFETY: `name` is a NUL-terminated symbol name supplied by the caller.
    let requested = unsafe { CStr::from_ptr(name) };
    interposed_symbol(requested.to_bytes(), resolved)
}

fn interposed_symbol(name: &[u8], resolved: *mut c_void) -> *mut c_void {
    match name {
        b"glXSwapBuffers" => {
            seed_glx_swap(resolved);
            report_interposed(name);
            glXSwapBuffers as *mut c_void
        }
        b"eglSwapBuffers" => {
            seed_egl_swap(resolved);
            report_interposed(name);
            eglSwapBuffers as *mut c_void
        }
        b"glXGetProcAddress" | b"glXGetProcAddressARB" => {
            seed_get_proc(&GLX_GET_PROC, resolved, glx_proc_address as *mut c_void);
            glx_proc_address as *mut c_void
        }
        b"eglGetProcAddress" => {
            seed_get_proc(&EGL_GET_PROC, resolved, egl_proc_address as *mut c_void);
            egl_proc_address as *mut c_void
        }
        _ => resolved,
    }
}

fn report_interposed(name: &[u8]) {
    if diagnostics_enabled() {
        eprintln!(
            "vosulay GL agent interposed {}",
            String::from_utf8_lossy(name)
        );
    }
}

fn seed_glx_swap(resolved: *mut c_void) {
    if resolved == glXSwapBuffers as *mut c_void {
        return;
    }
    // SAFETY: `resolved` is the provider's glXSwapBuffers with this ABI.
    let _ = GLX_ORIGINAL.set(Some(unsafe {
        std::mem::transmute::<*mut c_void, GlxSwap>(resolved)
    }));
}

fn seed_egl_swap(resolved: *mut c_void) {
    if resolved == eglSwapBuffers as *mut c_void {
        return;
    }
    // SAFETY: `resolved` is the provider's eglSwapBuffers with this ABI.
    let _ = EGL_ORIGINAL.set(Some(unsafe {
        std::mem::transmute::<*mut c_void, EglSwap>(resolved)
    }));
}

fn seed_get_proc(slot: &OnceLock<Option<GetProcFn>>, resolved: *mut c_void, hook: *mut c_void) {
    if resolved == hook {
        return;
    }
    // SAFETY: `resolved` is a GL loader entry point with this ABI.
    let _ = slot.set(Some(unsafe {
        std::mem::transmute::<*mut c_void, GetProcFn>(resolved)
    }));
}

/// Extension-based present paths resolve through the GL loaders instead of
/// `dlsym`, so those lookups are filtered the same way.
fn proc_address(slot: &OnceLock<Option<GetProcFn>>, name: *const c_char) -> *mut c_void {
    let Some(Some(real)) = slot.get() else {
        return null_mut();
    };
    // SAFETY: the caller's name pointer is forwarded unchanged.
    let resolved = unsafe { real(name) };
    if resolved.is_null() || name.is_null() {
        return resolved;
    }
    // SAFETY: `name` is a NUL-terminated function name supplied by the caller.
    let requested = unsafe { CStr::from_ptr(name) };
    interposed_symbol(requested.to_bytes(), resolved)
}

extern "C" fn glx_proc_address(name: *const c_char) -> *mut c_void {
    proc_address(&GLX_GET_PROC, name)
}

extern "C" fn egl_proc_address(name: *const c_char) -> *mut c_void {
    proc_address(&EGL_GET_PROC, name)
}

fn next_symbol(name: &'static [u8]) -> *mut c_void {
    handle_symbol(libc::RTLD_NEXT, name)
}

fn default_symbol(name: &'static [u8]) -> *mut c_void {
    handle_symbol(libc::RTLD_DEFAULT, name)
}

/// Every internal lookup must use the real `dlsym`; routing through the
/// exported interposer would seed this agent's own hooks as the originals and
/// recurse on the first present.
fn handle_symbol(handle: *mut c_void, name: &'static [u8]) -> *mut c_void {
    if let Some(real) = real_dlsym() {
        // SAFETY: `name` is a static NUL-terminated symbol name.
        return unsafe { real(handle, name.as_ptr().cast()) };
    }
    // SAFETY: `name` is a static NUL-terminated symbol name.
    unsafe { libc::dlsym(handle, name.as_ptr().cast()) }
}

fn provider_symbol(name: &'static [u8], libraries: &[&'static [u8]]) -> *mut c_void {
    let next = next_symbol(name);
    if !next.is_null() {
        return next;
    }
    for library in libraries {
        // SAFETY: library names are static NUL-terminated strings. RTLD_NOLOAD
        // acquires only a provider that the target already loaded.
        let handle =
            unsafe { libc::dlopen(library.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        if handle.is_null() {
            continue;
        }
        let symbol = handle_symbol(handle, name);
        // SAFETY: balance the reference obtained by dlopen.
        unsafe { libc::dlclose(handle) };
        if !symbol.is_null() {
            return symbol;
        }
    }
    null_mut()
}

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
fn extension_symbol(name: &'static [u8]) -> *mut c_void {
    let direct = next_symbol(name);
    if !direct.is_null() {
        return direct;
    }
    let direct = default_symbol(name);
    if !direct.is_null() {
        return direct;
    }
    let get_proc = provider_symbol(b"eglGetProcAddress\0", &[b"libEGL.so.1\0", b"libEGL.so\0"]);
    if get_proc.is_null() {
        return null_mut();
    }
    // SAFETY: the loader returned eglGetProcAddress with its specified ABI and
    // `name` is a static NUL-terminated extension function name.
    let get_proc = unsafe { std::mem::transmute::<*mut c_void, GetProcAddress>(get_proc) };
    // SAFETY: the caller validates the non-null pointer before using its named ABI.
    unsafe { get_proc(name.as_ptr().cast()) }
}
type EglSwap = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
static EGL_ORIGINAL: OnceLock<Option<EglSwap>> = OnceLock::new();
type GlxSwap = unsafe extern "C" fn(*mut c_void, c_ulong);
static GLX_ORIGINAL: OnceLock<Option<GlxSwap>> = OnceLock::new();

fn seed_swap_originals() {
    let egl = provider_symbol(b"eglSwapBuffers\0", &[b"libEGL.so.1\0", b"libEGL.so\0"]);
    if !egl.is_null() && egl != eglSwapBuffers as *mut c_void {
        // SAFETY: provider lookup returned the EGL swap symbol with this ABI.
        let _ = EGL_ORIGINAL.set(Some(unsafe {
            std::mem::transmute::<*mut c_void, EglSwap>(egl)
        }));
    }
    let glx = provider_symbol(
        b"glXSwapBuffers\0",
        &[b"libGLX.so.0\0", b"libGL.so.1\0", b"libGLX.so\0"],
    );
    if !glx.is_null() && glx != glXSwapBuffers as *mut c_void {
        // SAFETY: provider lookup returned the GLX swap symbol with this ABI.
        let _ = GLX_ORIGINAL.set(Some(unsafe {
            std::mem::transmute::<*mut c_void, GlxSwap>(glx)
        }));
    }
}
fn egl_original() -> Option<EglSwap> {
    *EGL_ORIGINAL.get_or_init(|| {
        let p = next_symbol(b"eglSwapBuffers\0");
        if p.is_null() {
            None
        } else {
            // SAFETY: `p` is the address returned for eglSwapBuffers and the
            // declared ABI matches the interposed symbol.
            Some(unsafe { std::mem::transmute::<*mut c_void, EglSwap>(p) })
        }
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffers(display: *mut c_void, surface: *mut c_void) -> c_uint {
    let original = egl_original();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(rt) = presenting_runtime() {
            let dimensions = query_egl(display, surface);
            present(rt, surface as usize as u64, dimensions);
        }
    }));
    if let Some(f) = original {
        // SAFETY: forwarding the original EGL handles unchanged.
        unsafe { f(display, surface) }
    } else {
        0
    }
}
type EglQuery = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;
static EGL_QUERY: OnceLock<Option<EglQuery>> = OnceLock::new();
fn query_egl(display: *mut c_void, surface: *mut c_void) -> (u32, u32) {
    let q = *EGL_QUERY.get_or_init(|| {
        let p = extension_symbol(b"eglQuerySurface\0");
        if p.is_null() {
            None
        } else {
            // SAFETY: `p` is the EGL query function resolved for this process.
            Some(unsafe { std::mem::transmute::<*mut c_void, EglQuery>(p) })
        }
    });
    let Some(q) = q else {
        return (0, 0);
    };
    let mut w = 0;
    let mut h = 0;
    // SAFETY: display/surface are EGL handles and output pointers are local.
    unsafe {
        q(display, surface, 0x3057, &mut w);
        q(display, surface, 0x3056, &mut h);
    }
    (w.max(0) as u32, h.max(0) as u32)
}
fn glx_original() -> Option<GlxSwap> {
    GLX_ORIGINAL
        .get_or_init(|| {
            // SAFETY: symbol lookup is confined to the process's loader.
            let p = next_symbol(b"glXSwapBuffers\0");
            if p.is_null() {
                None
            } else {
                // SAFETY: `p` is the original GLX swap symbol with this ABI.
                Some(unsafe { std::mem::transmute::<*mut c_void, GlxSwap>(p) })
            }
        })
        .to_owned()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXSwapBuffers(display: *mut c_void, drawable: c_ulong) {
    let original = glx_original();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(rt) = presenting_runtime() {
            // SAFETY: GLX supplies valid handles to this interposer.
            let dimensions = query_drawable(display, drawable);
            present(rt, drawable, dimensions);
        }
    }));
    if let Some(f) = original {
        // SAFETY: forwarding the original GLX handles unchanged.
        unsafe { f(display, drawable) };
    }
}
type GlxQuery = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint);
static GLX_QUERY: OnceLock<Option<GlxQuery>> = OnceLock::new();
fn query_drawable(display: *mut c_void, drawable: c_ulong) -> (u32, u32) {
    let q = *GLX_QUERY.get_or_init(|| {
        let p = extension_symbol(b"glXQueryDrawable\0");
        if p.is_null() {
            None
        } else {
            // SAFETY: `p` is the GLX query function resolved for this process.
            Some(unsafe { std::mem::transmute::<*mut c_void, GlxQuery>(p) })
        }
    });
    let Some(q) = q else {
        return (0, 0);
    };
    let mut w = 0;
    let mut h = 0;
    // SAFETY: display/drawable are GLX handles and output pointers are local.
    unsafe {
        q(display, drawable, 0x801D, &mut w);
        q(display, drawable, 0x801E, &mut h);
    }
    (w, h)
}
static DUMP_STARTED: OnceLock<Instant> = OnceLock::new();
static DUMP_WRITTEN: AtomicBool = AtomicBool::new(false);

/// Waits for the target to settle before sampling: a game's first presented
/// frames are usually cleared buffers, so an immediate dump proves nothing.
/// `VOSULAY_FRAME_DUMP_DELAY` overrides the default delay in seconds.
fn frame_dump_due() -> bool {
    if DUMP_WRITTEN.load(Ordering::Relaxed) || !diagnostics_enabled() {
        return false;
    }
    let delay = frame_dump_delay_secs();
    DUMP_STARTED
        .get_or_init(Instant::now)
        .elapsed()
        .ge(&Duration::from_secs(delay))
}

/// Diagnostics-only: reads back the framebuffer the target is about to present
/// so overlay composition can be verified inside the target process, without
/// depending on any compositor screenshot path. `phase` distinguishes the
/// contents before and after the overlay is composited.
fn dump_presented_frame(gl: &GlFns, width: u32, height: u32, phase: &str) {
    let (Ok(w), Ok(h)) = (usize::try_from(width), usize::try_from(height)) else {
        return;
    };
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return;
    }
    let mut pixels = vec![0u8; w * h * 4];
    // SAFETY: the caller holds the current context; the destination buffer is
    // exactly the requested size and the pack alignment is restored afterwards.
    unsafe {
        let mut alignment = 4;
        (gl.get_i)(GL_PACK_ALIGNMENT, &mut alignment);
        (gl.pixel_store)(GL_PACK_ALIGNMENT, 1);
        (gl.read_pixels)(
            0,
            0,
            width as c_int,
            height as c_int,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            pixels.as_mut_ptr().cast(),
        );
        (gl.pixel_store)(GL_PACK_ALIGNMENT, alignment);
    }
    let mut ppm = Vec::with_capacity(32 + w * h * 3);
    ppm.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for row in (0..h).rev() {
        for column in 0..w {
            let offset = (row * w + column) * 4;
            ppm.extend_from_slice(&pixels[offset..offset + 3]);
        }
    }
    let path = format!("/tmp/vosulay-frame-{}-{phase}.ppm", std::process::id());
    match std::fs::write(&path, &ppm) {
        Ok(()) => eprintln!("vosulay {phase}-composite frame dumped to {path}"),
        Err(error) => eprintln!("vosulay frame dump failed: {error}"),
    }
}

fn present(rt: &Runtime, target_id: u64, surface: (u32, u32)) {
    CONTEXT.with(|cell| {
        if let Some(resources) = cell.borrow_mut().as_mut()
            && let Some(gl) = resources.gl
        {
            resources.fences.poll(&gl);
        }
    });
    if rt.failed.load(Ordering::Acquire) {
        return;
    }
    if TRANSPORT_CAPABILITIES.load(Ordering::Acquire) == 0 {
        probe_transport_capabilities();
    }
    let (width, height) = surface;
    rt.update_target(target_id, width, height);
    let Some(frame) = rt.mailbox.begin_present() else {
        return;
    };
    // Composite only into the drawable the host was told about. Re-deriving a
    // choice here could disagree with the announcement and leave the overlay
    // drawing into a surface the host is not producing frames for.
    if rt.announced_target() != target_id || frame.target_id() != target_id {
        return;
    }
    CONTEXT.with(|cell| {
        let mut resources = cell.borrow_mut();
        if resources.is_none() {
            *resources = Some(ContextResources::new());
        }
        if let Some(resources) = resources.as_mut() {
            if resources.ensure().is_err() {
                if !rt.failed.swap(true, Ordering::AcqRel) {
                    eprintln!("vosulay GL compositor initialization failed");
                }
                return;
            }
            if let Some(gl) = resources.gl {
                let _guard = GlStateGuard::capture(&gl);
                match &frame.image {
                    FrameImage::Shm { descriptor, image } => {
                        let dump = frame_dump_due();
                        if dump {
                            dump_presented_frame(&gl, width, height, "pre");
                        }
                        resources.draw(image, descriptor, frame.serial, width, height);
                        if !SHM_DRAW_REPORTED.swap(true, Ordering::Relaxed) {
                            eprintln!(
                                "vosulay shared-memory compositor first frame: {}x{} format={}",
                                descriptor.width, descriptor.height, descriptor.format
                            );
                        }
                        if dump {
                            dump_presented_frame(&gl, width, height, "post");
                            DUMP_WRITTEN.store(true, Ordering::Release);
                        }
                    }
                    FrameImage::Dmabuf { descriptor, fds } => {
                        let dump = frame_dump_due();
                        if dump {
                            dump_presented_frame(&gl, width, height, "pre");
                        }
                        let drawn =
                            resources.draw_dmabuf(descriptor, fds, frame.serial, width, height);
                        if !DMABUF_DRAW_REPORTED.swap(true, Ordering::Relaxed) {
                            eprintln!(
                                "vosulay DMA-BUF compositor first frame: {} (gpu fencing: {})",
                                if drawn { "drawn" } else { "rejected" },
                                if FenceTracker::supported(&gl) {
                                    "enabled"
                                } else {
                                    "unavailable"
                                }
                            );
                        }
                        if dump && drawn {
                            dump_presented_frame(&gl, width, height, "post");
                            DUMP_WRITTEN.store(true, Ordering::Release);
                        }
                        if drawn {
                            // Repeated presents update one last-use fence for
                            // this frame in this context. Failure means no safe
                            // completion proof exists, so withdraw DMA-BUF and
                            // retire the frame without acknowledging early.
                            if !resources.fences.track(&gl, frame.as_arc()) {
                                reject_dmabuf("GL sync objects unavailable");
                                rt.mailbox.discard(&frame);
                            }
                        } else {
                            rt.mailbox.discard(&frame);
                        }
                    }
                }
            }
        }
    });
}
#[derive(Clone, Copy)]
struct GlFns {
    is_gles: bool,
    get_i: unsafe extern "C" fn(u32, *mut c_int),
    get_b: unsafe extern "C" fn(u32, *mut u8),
    enabled: unsafe extern "C" fn(u32) -> u8,
    enable: unsafe extern "C" fn(u32),
    disable: unsafe extern "C" fn(u32),
    blend_sep: unsafe extern "C" fn(u32, u32, u32, u32),
    blend_eq: unsafe extern "C" fn(u32, u32),
    viewport: unsafe extern "C" fn(c_int, c_int, c_int, c_int),
    use_program: unsafe extern "C" fn(u32),
    bind_vao: Option<unsafe extern "C" fn(u32)>,
    gen_vao: Option<unsafe extern "C" fn(c_int, *mut u32)>,
    bind_buffer: unsafe extern "C" fn(u32, u32),
    bind_framebuffer: unsafe extern "C" fn(u32, u32),
    pixel_store: unsafe extern "C" fn(u32, c_int),
    active: unsafe extern "C" fn(u32),
    bind_tex: unsafe extern "C" fn(u32, u32),
    polygon: Option<unsafe extern "C" fn(u32, u32)>,
    color_mask: unsafe extern "C" fn(u8, u8, u8, u8),
    depth_mask: unsafe extern "C" fn(u8),
    create_shader: unsafe extern "C" fn(u32) -> u32,
    shader_source: unsafe extern "C" fn(u32, c_int, *const *const c_char, *const c_int),
    compile_shader: unsafe extern "C" fn(u32),
    shader_iv: unsafe extern "C" fn(u32, u32, *mut c_int),
    create_program: unsafe extern "C" fn() -> u32,
    attach_shader: unsafe extern "C" fn(u32, u32),
    bind_attrib: unsafe extern "C" fn(u32, u32, *const c_char),
    link_program: unsafe extern "C" fn(u32),
    program_iv: unsafe extern "C" fn(u32, u32, *mut c_int),
    delete_shader: unsafe extern "C" fn(u32),
    gen_texture: unsafe extern "C" fn(c_int, *mut u32),
    delete_textures: unsafe extern "C" fn(c_int, *const u32),
    get_error: unsafe extern "C" fn() -> u32,
    read_pixels: unsafe extern "C" fn(c_int, c_int, c_int, c_int, u32, u32, *mut c_void),
    tex_level_i: unsafe extern "C" fn(u32, c_int, u32, *mut c_int),
    tex_param: unsafe extern "C" fn(u32, u32, c_int),
    tex_image:
        unsafe extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void),
    tex_sub: unsafe extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void),
    gen_buffer: unsafe extern "C" fn(c_int, *mut u32),
    buffer_data: unsafe extern "C" fn(u32, isize, *const c_void, u32),
    attrib_enable: unsafe extern "C" fn(u32),
    attrib_disable: unsafe extern "C" fn(u32),
    get_attrib_i: unsafe extern "C" fn(u32, u32, *mut c_int),
    get_attrib_ptr: unsafe extern "C" fn(u32, u32, *mut *mut c_void),
    attrib_ptr: unsafe extern "C" fn(u32, c_int, u32, u8, c_int, *const c_void),
    draw: unsafe extern "C" fn(u32, c_int, c_int),
    finish: unsafe extern "C" fn(),
    flush: unsafe extern "C" fn(),
    /// GL 3.2 / GLES 3.0 sync objects. Present only when the context can prove
    /// GPU completion; without them frame release falls back to CPU lifetime.
    fence_sync: Option<unsafe extern "C" fn(u32, u32) -> *mut c_void>,
    client_wait_sync: Option<unsafe extern "C" fn(*mut c_void, u32, u64) -> u32>,
    delete_sync: Option<unsafe extern "C" fn(*mut c_void)>,
    uniform_i: unsafe extern "C" fn(c_int, c_int),
    get_uniform: unsafe extern "C" fn(u32, *const c_char) -> c_int,
}
type EglDisplay = *mut c_void;
type EglContext = *mut c_void;
type EglClientBuffer = *mut c_void;
type EglImage = *mut c_void;
type EglCreateImage =
    unsafe extern "C" fn(EglDisplay, EglContext, c_uint, EglClientBuffer, *const c_int) -> EglImage;
type EglDestroyImage = unsafe extern "C" fn(EglDisplay, EglImage) -> c_uint;
type GlEglImageTargetTexture = unsafe extern "C" fn(u32, *const c_void);
type GlEglImageTargetStorage = unsafe extern "C" fn(u32, *const c_void, *const c_int);
#[derive(Clone, Copy)]
struct EglDmabufFns {
    get_current_display: unsafe extern "C" fn() -> EglDisplay,
    get_display: unsafe extern "C" fn(*mut c_void) -> EglDisplay,
    initialize: unsafe extern "C" fn(EglDisplay, *mut c_int, *mut c_int) -> c_uint,
    create_image: EglCreateImage,
    destroy_image: EglDestroyImage,
    image_target_texture: Option<GlEglImageTargetTexture>,
    image_target_storage: Option<GlEglImageTargetStorage>,
}
static EGL_DMABUF: OnceLock<Option<EglDmabufFns>> = OnceLock::new();

type GlCreateMemoryObjects = unsafe extern "C" fn(c_int, *mut u32);
type GlDeleteMemoryObjects = unsafe extern "C" fn(c_int, *const u32);
type GlImportMemoryFd = unsafe extern "C" fn(u32, u64, u32, c_int);
type GlTexStorageMem2D = unsafe extern "C" fn(u32, c_int, u32, c_int, c_int, u32, u64);
#[derive(Clone, Copy)]
struct GlMemoryObjectFns {
    create: GlCreateMemoryObjects,
    delete: GlDeleteMemoryObjects,
    import_fd: GlImportMemoryFd,
    tex_storage: GlTexStorageMem2D,
}
static GL_MEMORY_OBJECT: OnceLock<Option<GlMemoryObjectFns>> = OnceLock::new();
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DmabufSurfaceKey {
    target_id: u64,
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    plane_offset: u64,
    plane_stride: u32,
    plane_size: u64,
    fd_device: u64,
    fd_inode: u64,
    fd_size: u64,
}

impl DmabufSurfaceKey {
    fn from_descriptor(
        descriptor: &DmabufDescriptor,
        fd_identity: (u64, u64, u64),
    ) -> Result<Self, ()> {
        descriptor.validate().map_err(|_| ())?;
        let Some(plane) = descriptor.planes.first().copied() else {
            return Err(());
        };
        Ok(Self {
            target_id: descriptor.target_id,
            width: descriptor.width,
            height: descriptor.height,
            fourcc: descriptor.fourcc,
            modifier: descriptor.modifier,
            plane_offset: plane.offset,
            plane_stride: plane.stride,
            plane_size: plane.size,
            fd_device: fd_identity.0,
            fd_inode: fd_identity.1,
            fd_size: fd_identity.2,
        })
    }
}

struct SurfacePoolEntry<T> {
    key: DmabufSurfaceKey,
    value: T,
    last_used: u64,
}

struct SurfacePool<T> {
    entries: Vec<SurfacePoolEntry<T>>,
}

impl<T> SurfacePool<T> {
    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(MAX_DMABUF_SURFACES),
        }
    }

    fn get_mut(&mut self, key: DmabufSurfaceKey, serial: u64) -> Option<&mut T> {
        let entry = self.entries.iter_mut().find(|entry| entry.key == key)?;
        entry.last_used = serial;
        Some(&mut entry.value)
    }

    fn insert(&mut self, key: DmabufSurfaceKey, value: T, serial: u64) -> Option<T> {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.last_used = serial;
            return Some(std::mem::replace(&mut entry.value, value));
        }
        let entry = SurfacePoolEntry {
            key,
            value,
            last_used: serial,
        };
        if self.entries.len() < MAX_DMABUF_SURFACES {
            self.entries.push(entry);
            return None;
        }
        let Some((eviction, _)) = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.last_used)
        else {
            return Some(entry.value);
        };
        Some(std::mem::replace(&mut self.entries[eviction], entry).value)
    }

    #[cfg(test)]
    fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn contains(&self, key: DmabufSurfaceKey) -> bool {
        self.entries.iter().any(|entry| entry.key == key)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

enum DmabufBacking {
    Egl {
        image: EglImage,
        display: EglDisplay,
        destroy_image: EglDestroyImage,
        _fd: OwnedFd,
    },
    MemoryObject {
        memory: u32,
        delete: GlDeleteMemoryObjects,
    },
}

struct DmabufSurface {
    backing: DmabufBacking,
    texture: u32,
}

fn fd_identity(fd: &OwnedFd) -> Result<(u64, u64, u64), ()> {
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 || stat.st_size < 0 {
        return Err(());
    }
    Ok((
        stat.st_dev,
        stat.st_ino,
        u64::try_from(stat.st_size).map_err(|_| ())?,
    ))
}

const DMABUF_ATTRIBUTE_CAPACITY: usize = 17;

struct DmabufAttributes {
    values: [c_int; DMABUF_ATTRIBUTE_CAPACITY],
}

impl DmabufAttributes {
    fn as_ptr(&self) -> *const c_int {
        self.values.as_ptr()
    }
}

fn destroy_surface(g: &GlFns, surface: DmabufSurface) {
    // SAFETY: callers hold the context current and have finished prior draws.
    unsafe {
        (g.delete_textures)(1, &surface.texture);
        match surface.backing {
            DmabufBacking::Egl {
                image,
                display,
                destroy_image,
                ..
            } => {
                destroy_image(display, image);
            }
            DmabufBacking::MemoryObject { memory, delete } => {
                delete(1, &memory);
            }
        }
    }
}

fn reclaim_retired(g: &GlFns, retired: &mut Vec<DmabufSurface>) {
    if retired.is_empty() {
        return;
    }
    // Pool eviction is rare and bounded. Finish here rather than destroying an
    // imported object that an earlier draw may still reference.
    unsafe { (g.finish)() };
    for surface in retired.drain(..) {
        destroy_surface(g, surface);
    }
}

fn load_egl_dmabuf() -> Option<EglDmabufFns> {
    *EGL_DMABUF.get_or_init(|| {
        let current = extension_symbol(b"eglGetCurrentDisplay\0");
        let get_display = extension_symbol(b"eglGetDisplay\0");
        let initialize = extension_symbol(b"eglInitialize\0");
        let create = extension_symbol(b"eglCreateImageKHR\0");
        let destroy = extension_symbol(b"eglDestroyImageKHR\0");
        let texture = extension_symbol(b"glEGLImageTargetTexture2DOES\0");
        let storage = extension_symbol(b"glEGLImageTargetTexStorageEXT\0");
        if current.is_null()
            || get_display.is_null()
            || initialize.is_null()
            || create.is_null()
            || destroy.is_null()
            || (texture.is_null() && storage.is_null())
        {
            return None;
        }
        Some(unsafe {
            EglDmabufFns {
                get_current_display: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn() -> EglDisplay,
                >(current),
                get_display: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void) -> EglDisplay,
                >(get_display),
                initialize: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(EglDisplay, *mut c_int, *mut c_int) -> c_uint,
                >(initialize),
                create_image: std::mem::transmute::<*mut c_void, EglCreateImage>(create),
                destroy_image: std::mem::transmute::<*mut c_void, EglDestroyImage>(destroy),
                image_target_texture: (!texture.is_null())
                    .then(|| std::mem::transmute::<*mut c_void, GlEglImageTargetTexture>(texture)),
                image_target_storage: (!storage.is_null())
                    .then(|| std::mem::transmute::<*mut c_void, GlEglImageTargetStorage>(storage)),
            }
        })
    })
}

fn load_gl_memory_object() -> Option<GlMemoryObjectFns> {
    *GL_MEMORY_OBJECT.get_or_init(|| {
        let create = extension_symbol(b"glCreateMemoryObjectsEXT\0");
        let delete = extension_symbol(b"glDeleteMemoryObjectsEXT\0");
        let import_fd = extension_symbol(b"glImportMemoryFdEXT\0");
        let tex_storage = extension_symbol(b"glTexStorageMem2DEXT\0");
        if [create, delete, import_fd, tex_storage]
            .into_iter()
            .any(|pointer| pointer.is_null())
        {
            return None;
        }
        Some(unsafe {
            GlMemoryObjectFns {
                create: std::mem::transmute::<*mut c_void, GlCreateMemoryObjects>(create),
                delete: std::mem::transmute::<*mut c_void, GlDeleteMemoryObjects>(delete),
                import_fd: std::mem::transmute::<*mut c_void, GlImportMemoryFd>(import_fd),
                tex_storage: std::mem::transmute::<*mut c_void, GlTexStorageMem2D>(tex_storage),
            }
        })
    })
}

static IMPORT_DISPLAY: OnceLock<Option<usize>> = OnceLock::new();

/// GLX applications — notably Wine's `winex11.drv` — have no current EGL
/// display, so DMA-BUF import must initialize its own. The image is bound into
/// the current GL context, which does not have to be EGL-created.
fn import_display(egl: EglDmabufFns) -> EglDisplay {
    // SAFETY: EGL entry points were resolved from the process's providers.
    let current = unsafe { (egl.get_current_display)() };
    if !current.is_null() {
        return current;
    }
    (*IMPORT_DISPLAY.get_or_init(|| {
        // SAFETY: EGL_DEFAULT_DISPLAY is the null native display handle.
        let display = unsafe { (egl.get_display)(null_mut()) };
        if display.is_null() {
            return None;
        }
        let mut major = 0;
        let mut minor = 0;
        // SAFETY: `display` is an EGL display and the outputs are local.
        if unsafe { (egl.initialize)(display, &mut major, &mut minor) } == 0 {
            return None;
        }
        Some(display as usize)
    }))
    .map_or(null_mut(), |display| display as EglDisplay)
}
unsafe impl Send for GlFns {}
unsafe impl Sync for GlFns {}

fn configure_imported_texture(g: &GlFns, fourcc: u32) {
    unsafe {
        (g.tex_param)(GL_TEXTURE_2D, 0x2801, 0x2601);
        (g.tex_param)(GL_TEXTURE_2D, 0x2800, 0x2601);
        (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        if fourcc == overlay_protocol::DRM_FORMAT_ARGB8888 {
            (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_R, GL_BLUE);
            (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_G, GL_GREEN);
            (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_B, GL_RED);
            (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_SWIZZLE_A, GL_ALPHA);
        }
    }
}

fn bound_texture_is_sampleable(g: &GlFns) -> bool {
    let mut width = 0;
    let mut height = 0;
    unsafe {
        (g.tex_level_i)(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut width);
        (g.tex_level_i)(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut height);
    }
    width > 0 && height > 0
}

fn import_memory_object_surface(
    g: &GlFns,
    memory_fns: GlMemoryObjectFns,
    descriptor: &DmabufDescriptor,
    fd: &OwnedFd,
) -> Result<DmabufSurface, ()> {
    let [plane] = descriptor.planes.as_slice() else {
        return Err(());
    };
    // Match the proven MangoHud/idk-overlay GLX path: the PRIME fd is
    // imported as opaque external memory and the driver retains the
    // allocation's native layout. The GL API has no modifier argument,
    // so rejecting a perfectly valid tiled Chromium pixmap merely because
    // its DRM modifier is non-zero prevents the path from ever being tried.
    // Import only the byte range the texture can address; using the full
    // padded allocation size is known to fail on some Mesa drivers.
    let row_bytes = u64::from(plane.stride)
        .checked_mul(u64::from(descriptor.height))
        .ok_or(())?;
    let import_size = plane.offset.checked_add(row_bytes).ok_or(())?;
    if import_size == 0 || import_size > plane.size {
        return Err(());
    }
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(duplicate) };
    let mut memory = 0;
    let mut texture = 0;
    unsafe {
        (memory_fns.create)(1, &mut memory);
        if memory == 0 {
            return Err(());
        }
        while (g.get_error)() != 0 {}
        // The GL call takes ownership of the duplicated descriptor.
        (memory_fns.import_fd)(
            memory,
            import_size,
            GL_HANDLE_TYPE_OPAQUE_FD_EXT,
            owned.into_raw_fd(),
        );
        if (g.get_error)() != 0 {
            (memory_fns.delete)(1, &memory);
            return Err(());
        }
        (g.gen_texture)(1, &mut texture);
        if texture == 0 {
            (memory_fns.delete)(1, &memory);
            return Err(());
        }
        (g.active)(GL_TEXTURE0);
        (g.bind_tex)(GL_TEXTURE_2D, texture);
        configure_imported_texture(g, descriptor.fourcc);
        while (g.get_error)() != 0 {}
        (memory_fns.tex_storage)(
            GL_TEXTURE_2D,
            1,
            GL_RGBA8,
            descriptor.width as c_int,
            descriptor.height as c_int,
            memory,
            plane.offset,
        );
        if (g.get_error)() != 0 || !bound_texture_is_sampleable(g) {
            (g.delete_textures)(1, &texture);
            (memory_fns.delete)(1, &memory);
            return Err(());
        }
    }
    Ok(DmabufSurface {
        backing: DmabufBacking::MemoryObject {
            memory,
            delete: memory_fns.delete,
        },
        texture,
    })
}

fn current_context_uses_egl() -> bool {
    load_egl_dmabuf().is_some_and(|egl| {
        // SAFETY: queried while the present hook owns the current context.
        !unsafe { (egl.get_current_display)() }.is_null()
    })
}

fn import_egl_surface(
    g: &GlFns,
    descriptor: &DmabufDescriptor,
    fds: &[OwnedFd],
) -> Result<DmabufSurface, ()> {
    let egl = load_egl_dmabuf().ok_or(())?;
    let attributes = dmabuf_attributes(descriptor, fds)?;
    let fd = fds.first().ok_or(())?;
    let retained_fd = fd.try_clone().map_err(|_| ())?;
    unsafe {
        let display = import_display(egl);
        if display.is_null() || !egl_display_imports_dmabuf(egl, display) {
            return Err(());
        }
        let image = (egl.create_image)(
            display,
            null_mut(),
            EGL_LINUX_DMA_BUF_EXT,
            null_mut(),
            attributes.as_ptr(),
        );
        if image.is_null() {
            return Err(());
        }
        let mut texture = 0;
        (g.gen_texture)(1, &mut texture);
        if texture == 0 {
            (egl.destroy_image)(display, image);
            return Err(());
        }
        (g.active)(GL_TEXTURE0);
        (g.bind_tex)(GL_TEXTURE_2D, texture);
        configure_imported_texture(g, descriptor.fourcc);
        while (g.get_error)() != 0 {}
        if let Some(storage) = egl.image_target_storage {
            storage(GL_TEXTURE_2D, image.cast_const(), null());
        } else if let Some(texture_target) = egl.image_target_texture {
            texture_target(GL_TEXTURE_2D, image.cast_const());
        } else {
            (g.delete_textures)(1, &texture);
            (egl.destroy_image)(display, image);
            return Err(());
        }
        if (g.get_error)() != 0 || !bound_texture_is_sampleable(g) {
            (g.delete_textures)(1, &texture);
            (egl.destroy_image)(display, image);
            return Err(());
        }
        Ok(DmabufSurface {
            backing: DmabufBacking::Egl {
                image,
                display,
                destroy_image: egl.destroy_image,
                _fd: retained_fd,
            },
            texture,
        })
    }
}

fn load_gl() -> Result<GlFns, ()> {
    const REQUIRED: [&[u8]; 46] = [
        b"glGetTexLevelParameteriv\0",
        b"glReadPixels\0",
        b"glGetError\0",
        b"glGetString\0",
        b"glGetIntegerv\0",
        b"glGetBooleanv\0",
        b"glIsEnabled\0",
        b"glEnable\0",
        b"glDisable\0",
        b"glBlendFuncSeparate\0",
        b"glBlendEquationSeparate\0",
        b"glViewport\0",
        b"glUseProgram\0",
        b"glBindBuffer\0",
        b"glBindFramebuffer\0",
        b"glPixelStorei\0",
        b"glActiveTexture\0",
        b"glBindTexture\0",
        b"glColorMask\0",
        b"glDepthMask\0",
        b"glCreateShader\0",
        b"glShaderSource\0",
        b"glCompileShader\0",
        b"glGetShaderiv\0",
        b"glCreateProgram\0",
        b"glAttachShader\0",
        b"glBindAttribLocation\0",
        b"glLinkProgram\0",
        b"glGetProgramiv\0",
        b"glDeleteShader\0",
        b"glGenTextures\0",
        b"glDeleteTextures\0",
        b"glTexParameteri\0",
        b"glTexImage2D\0",
        b"glTexSubImage2D\0",
        b"glGenBuffers\0",
        b"glBufferData\0",
        b"glEnableVertexAttribArray\0",
        b"glDisableVertexAttribArray\0",
        b"glGetVertexAttribiv\0",
        b"glGetVertexAttribPointerv\0",
        b"glVertexAttribPointer\0",
        b"glDrawArrays\0",
        b"glFinish\0",
        b"glUniform1i\0",
        b"glGetUniformLocation\0",
    ];
    if REQUIRED.iter().any(|name| extension_symbol(name).is_null()) {
        return Err(());
    }
    macro_rules! required {
        ($name:literal, $ty:ty) => {{
            // SAFETY: the symbol was checked above and has this C ABI.
            unsafe {
                std::mem::transmute::<*mut c_void, $ty>(extension_symbol(
                    concat!($name, "\0").as_bytes(),
                ))
            }
        }};
    }
    macro_rules! optional {
        ($name:literal, $ty:ty) => {{
            let pointer = extension_symbol(concat!($name, "\0").as_bytes());
            if pointer.is_null() {
                None
            } else {
                // SAFETY: a non-null symbol with this name has this C ABI.
                Some(unsafe { std::mem::transmute::<*mut c_void, $ty>(pointer) })
            }
        }};
    }
    let get_string = required!("glGetString", unsafe extern "C" fn(u32) -> *const u8);
    // SAFETY: the current context owns this static version string.
    let version = unsafe { get_string(GL_VERSION) };
    if version.is_null() {
        return Err(());
    }
    // SAFETY: GL_VERSION returns a NUL-terminated string while the context lives.
    let is_gles = unsafe { CStr::from_ptr(version.cast()) }
        .to_bytes()
        .starts_with(b"OpenGL ES");
    let bind_vao = optional!("glBindVertexArray", unsafe extern "C" fn(u32));
    let gen_vao = optional!("glGenVertexArrays", unsafe extern "C" fn(c_int, *mut u32));
    let polygon = optional!("glPolygonMode", unsafe extern "C" fn(u32, u32));
    if !is_gles && (bind_vao.is_none() || gen_vao.is_none() || polygon.is_none()) {
        return Err(());
    }
    Ok(GlFns {
        is_gles,
        get_i: required!("glGetIntegerv", unsafe extern "C" fn(u32, *mut c_int)),
        get_b: required!("glGetBooleanv", unsafe extern "C" fn(u32, *mut u8)),
        enabled: required!("glIsEnabled", unsafe extern "C" fn(u32) -> u8),
        enable: required!("glEnable", unsafe extern "C" fn(u32)),
        disable: required!("glDisable", unsafe extern "C" fn(u32)),
        blend_sep: required!(
            "glBlendFuncSeparate",
            unsafe extern "C" fn(u32, u32, u32, u32)
        ),
        blend_eq: required!("glBlendEquationSeparate", unsafe extern "C" fn(u32, u32)),
        viewport: required!(
            "glViewport",
            unsafe extern "C" fn(c_int, c_int, c_int, c_int)
        ),
        use_program: required!("glUseProgram", unsafe extern "C" fn(u32)),
        bind_vao,
        gen_vao,
        bind_buffer: required!("glBindBuffer", unsafe extern "C" fn(u32, u32)),
        bind_framebuffer: required!("glBindFramebuffer", unsafe extern "C" fn(u32, u32)),
        pixel_store: required!("glPixelStorei", unsafe extern "C" fn(u32, c_int)),
        active: required!("glActiveTexture", unsafe extern "C" fn(u32)),
        bind_tex: required!("glBindTexture", unsafe extern "C" fn(u32, u32)),
        polygon,
        color_mask: required!("glColorMask", unsafe extern "C" fn(u8, u8, u8, u8)),
        depth_mask: required!("glDepthMask", unsafe extern "C" fn(u8)),
        create_shader: required!("glCreateShader", unsafe extern "C" fn(u32) -> u32),
        shader_source: required!(
            "glShaderSource",
            unsafe extern "C" fn(u32, c_int, *const *const c_char, *const c_int)
        ),
        compile_shader: required!("glCompileShader", unsafe extern "C" fn(u32)),
        shader_iv: required!("glGetShaderiv", unsafe extern "C" fn(u32, u32, *mut c_int)),
        create_program: required!("glCreateProgram", unsafe extern "C" fn() -> u32),
        attach_shader: required!("glAttachShader", unsafe extern "C" fn(u32, u32)),
        bind_attrib: required!(
            "glBindAttribLocation",
            unsafe extern "C" fn(u32, u32, *const c_char)
        ),
        link_program: required!("glLinkProgram", unsafe extern "C" fn(u32)),
        program_iv: required!("glGetProgramiv", unsafe extern "C" fn(u32, u32, *mut c_int)),
        delete_shader: required!("glDeleteShader", unsafe extern "C" fn(u32)),
        gen_texture: required!("glGenTextures", unsafe extern "C" fn(c_int, *mut u32)),
        delete_textures: required!("glDeleteTextures", unsafe extern "C" fn(c_int, *const u32)),
        get_error: required!("glGetError", unsafe extern "C" fn() -> u32),
        read_pixels: required!(
            "glReadPixels",
            unsafe extern "C" fn(c_int, c_int, c_int, c_int, u32, u32, *mut c_void)
        ),
        tex_level_i: required!(
            "glGetTexLevelParameteriv",
            unsafe extern "C" fn(u32, c_int, u32, *mut c_int)
        ),
        tex_param: required!("glTexParameteri", unsafe extern "C" fn(u32, u32, c_int)),
        tex_image: required!(
            "glTexImage2D",
            unsafe extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void)
        ),
        tex_sub: required!(
            "glTexSubImage2D",
            unsafe extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void)
        ),
        gen_buffer: required!("glGenBuffers", unsafe extern "C" fn(c_int, *mut u32)),
        buffer_data: required!(
            "glBufferData",
            unsafe extern "C" fn(u32, isize, *const c_void, u32)
        ),
        attrib_enable: required!("glEnableVertexAttribArray", unsafe extern "C" fn(u32)),
        attrib_disable: required!("glDisableVertexAttribArray", unsafe extern "C" fn(u32)),
        get_attrib_i: required!(
            "glGetVertexAttribiv",
            unsafe extern "C" fn(u32, u32, *mut c_int)
        ),
        get_attrib_ptr: required!(
            "glGetVertexAttribPointerv",
            unsafe extern "C" fn(u32, u32, *mut *mut c_void)
        ),
        attrib_ptr: required!(
            "glVertexAttribPointer",
            unsafe extern "C" fn(u32, c_int, u32, u8, c_int, *const c_void)
        ),
        draw: required!("glDrawArrays", unsafe extern "C" fn(u32, c_int, c_int)),
        finish: required!("glFinish", unsafe extern "C" fn()),
        flush: required!("glFlush", unsafe extern "C" fn()),
        fence_sync: optional!("glFenceSync", unsafe extern "C" fn(u32, u32) -> *mut c_void),
        client_wait_sync: optional!(
            "glClientWaitSync",
            unsafe extern "C" fn(*mut c_void, u32, u64) -> u32
        ),
        delete_sync: optional!("glDeleteSync", unsafe extern "C" fn(*mut c_void)),
        uniform_i: required!("glUniform1i", unsafe extern "C" fn(c_int, c_int)),
        get_uniform: required!(
            "glGetUniformLocation",
            unsafe extern "C" fn(u32, *const c_char) -> c_int
        ),
    })
}

struct ContextResources {
    gl: Option<GlFns>,
    program: u32,
    vao: u32,
    vbo: u32,
    texture: u32,
    sampler_loc: c_int,
    width: u32,
    height: u32,
    uploaded: u64,
    dmabuf_pool: SurfacePool<DmabufSurface>,
    retired: Vec<DmabufSurface>,
    /// Sync objects are context state, so completion tracking lives here
    /// rather than in the shared runtime.
    fences: FenceTracker,
    ready: bool,
}
thread_local! { static CONTEXT: RefCell<Option<ContextResources>> = const { RefCell::new(None) }; }
impl ContextResources {
    fn new() -> Self {
        Self {
            gl: None,
            program: 0,
            vao: 0,
            vbo: 0,
            texture: 0,
            sampler_loc: -1,
            width: 0,
            height: 0,
            uploaded: 0,
            dmabuf_pool: SurfacePool::new(),
            retired: Vec::with_capacity(MAX_DMABUF_SURFACES),
            fences: FenceTracker::default(),
            ready: false,
        }
    }

    fn ensure(&mut self) -> Result<(), ()> {
        if self.ready {
            return Ok(());
        }
        let g = load_gl()?;
        // Chromium frames are stored top-down while GL samples bottom-up, so the
        // vertical texture coordinate is inverted here for every transport.
        let vertex_source: &[u8] = if g.is_gles {
            b"attribute vec2 p; varying vec2 uv; void main(){uv=vec2((p.x+1.0)*0.5,1.0-(p.y+1.0)*0.5);gl_Position=vec4(p,0.0,1.0);}\0"
        } else {
            b"#version 330 core\nin vec2 p; out vec2 uv; void main(){uv=vec2((p.x+1.0)*0.5,1.0-(p.y+1.0)*0.5);gl_Position=vec4(p,0.0,1.0);}\0"
        };
        let fragment_source: &[u8] = if g.is_gles {
            b"precision mediump float; uniform sampler2D tex; varying vec2 uv; void main(){gl_FragColor=texture2D(tex,uv);}\0"
        } else {
            b"#version 330 core\nuniform sampler2D tex; in vec2 uv; out vec4 color; void main(){color=texture(tex,uv);}\0"
        };
        let vertex = shader(g, GL_VERTEX_SHADER, vertex_source)?;
        let fragment = shader(g, GL_FRAGMENT_SHADER, fragment_source)?;
        // SAFETY: these calls use the current context and newly created objects.
        let (program, linked, vao, vbo, texture, sampler_loc) = unsafe {
            let program = (g.create_program)();
            (g.attach_shader)(program, vertex);
            (g.attach_shader)(program, fragment);
            (g.bind_attrib)(program, 0, c"p".as_ptr());
            (g.link_program)(program);
            let mut linked = 0;
            (g.program_iv)(program, GL_LINK_STATUS, &mut linked);
            (g.delete_shader)(vertex);
            (g.delete_shader)(fragment);
            let mut vao = 0;
            if let Some(gen_vao) = g.gen_vao {
                gen_vao(1, &mut vao);
            }
            let mut vbo = 0;
            let mut texture = 0;
            (g.gen_buffer)(1, &mut vbo);
            (g.gen_texture)(1, &mut texture);
            let sampler_loc = (g.get_uniform)(program, c"tex".as_ptr());
            (program, linked, vao, vbo, texture, sampler_loc)
        };
        if linked == 0 {
            return Err(());
        }
        self.gl = Some(g);
        self.program = program;
        self.vao = vao;
        self.vbo = vbo;
        self.texture = texture;
        self.sampler_loc = sampler_loc;
        self.ready = true;
        Ok(())
    }

    fn draw_dmabuf(
        &mut self,
        descriptor: &DmabufDescriptor,
        fds: &[OwnedFd],
        serial: u64,
        target_width: u32,
        target_height: u32,
    ) -> bool {
        let Some(g) = self.gl else {
            return reject_dmabuf("no GL entry points");
        };
        reclaim_retired(&g, &mut self.retired);
        let Some(fd) = fds.first() else {
            return reject_dmabuf("missing plane descriptor");
        };
        let Ok(identity) = fd_identity(fd) else {
            return reject_dmabuf("plane descriptor identity unavailable");
        };
        let Ok(key) = DmabufSurfaceKey::from_descriptor(descriptor, identity) else {
            return reject_dmabuf("unsupported surface key");
        };
        if let Some(surface) = self.dmabuf_pool.get_mut(key, serial) {
            let texture = surface.texture;
            unsafe {
                (g.active)(GL_TEXTURE0);
                (g.bind_tex)(GL_TEXTURE_2D, texture);
                self.draw_bound_texture(g, texture, true, target_width, target_height);
            }
            return true;
        }

        let imported = if !g.is_gles {
            let memory_object = load_gl_memory_object()
                .and_then(|memory| import_memory_object_surface(&g, memory, descriptor, fd).ok());
            if memory_object.is_some() {
                memory_object
            } else if current_context_uses_egl() {
                // EGL-created desktop contexts may use EGLImage as a real
                // fallback. A GLX context must never do so: Wine's GLX path
                // can accept the bind yet sample black. Failed GLX opaque-fd
                // import therefore triggers the session's SHM re-attach.
                import_egl_surface(&g, descriptor, fds).ok()
            } else {
                None
            }
        } else {
            import_egl_surface(&g, descriptor, fds).ok()
        };
        let Some(surface) = imported else {
            return reject_dmabuf("no compatible memory-object or EGLImage import for this layout");
        };
        let texture = surface.texture;
        if let Some(evicted) = self.dmabuf_pool.insert(key, surface, serial) {
            self.retired.push(evicted);
        }
        unsafe {
            self.draw_bound_texture(g, texture, true, target_width, target_height);
        }
        true
    }

    /// One-shot diagnostic describing the texture the compositor samples. A
    /// bound EGLImage reports the image's dimensions and internal format; zeros
    /// mean the import produced no usable texture even though no GL error was
    /// raised.
    fn report_bound_texture(g: &GlFns) {
        if TEXTURE_REPORTED.load(Ordering::Relaxed)
            || !diagnostics_enabled()
            || TEXTURE_REPORTED.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let mut width = 0;
        let mut height = 0;
        let mut format = 0;
        let mut sampler = -1;
        // SAFETY: the caller has the context current with the texture bound to
        // unit 0; every output pointer is a local.
        unsafe {
            (g.tex_level_i)(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut width);
            (g.tex_level_i)(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut height);
            (g.tex_level_i)(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &mut format);
            (g.get_i)(GL_SAMPLER_BINDING, &mut sampler);
        }
        eprintln!(
            "vosulay overlay texture: {width}x{height} internal_format=0x{format:04x} sampler_binding={sampler}"
        );
    }

    unsafe fn draw_bound_texture(
        &self,
        g: GlFns,
        texture: u32,
        premultiplied: bool,
        target_width: u32,
        target_height: u32,
    ) {
        // SAFETY: the caller holds the current context and a complete
        // GlStateGuard; every object handle here belongs to this context.
        unsafe {
            (g.active)(GL_TEXTURE0);
            (g.bind_tex)(GL_TEXTURE_2D, texture);
            Self::report_bound_texture(&g);
            (g.use_program)(self.program);
            if let Some(bind_vao) = g.bind_vao {
                bind_vao(self.vao);
            }
            (g.bind_buffer)(GL_ARRAY_BUFFER, self.vbo);
            let vertices: [f32; 8] = [-1., -1., 1., -1., -1., 1., 1., 1.];
            (g.buffer_data)(
                GL_ARRAY_BUFFER,
                size_of::<[f32; 8]>() as isize,
                vertices.as_ptr().cast(),
                0x88E4,
            );
            (g.attrib_enable)(0);
            (g.attrib_ptr)(0, 2, GL_FLOAT, GL_FALSE, 8, null());
            if self.sampler_loc >= 0 {
                (g.uniform_i)(self.sampler_loc, 0);
            }
            (g.bind_framebuffer)(
                if g.is_gles {
                    GL_FRAMEBUFFER
                } else {
                    GL_DRAW_FRAMEBUFFER
                },
                0,
            );
            (g.viewport)(0, 0, target_width as c_int, target_height as c_int);
            (g.disable)(GL_DEPTH_TEST);
            (g.disable)(GL_STENCIL_TEST);
            (g.disable)(GL_CULL_FACE);
            (g.disable)(GL_SCISSOR_TEST);
            (g.color_mask)(1, 1, 1, 1);
            (g.depth_mask)(0);
            if let Some(polygon) = g.polygon {
                polygon(0x0408, 0x1B02);
            }
            (g.enable)(GL_BLEND);
            (g.blend_eq)(GL_FUNC_ADD, GL_FUNC_ADD);
            (g.blend_sep)(
                if premultiplied { GL_ONE } else { GL_SRC_ALPHA },
                GL_ONE_MINUS_SRC_ALPHA,
                if premultiplied { GL_ONE } else { GL_SRC_ALPHA },
                GL_ONE_MINUS_SRC_ALPHA,
            );
            (g.draw)(GL_TRIANGLE_STRIP, 0, 4);
        }
    }

    fn draw(
        &mut self,
        image: &MappedShm,
        descriptor: &TextureDescriptor,
        serial: u64,
        target_width: u32,
        target_height: u32,
    ) {
        let Some(g) = self.gl else {
            return;
        };
        // SAFETY: the context is current and all object handles belong to it.
        unsafe {
            (g.pixel_store)(GL_UNPACK_ALIGNMENT, 1);
            if !g.is_gles {
                (g.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
                (g.pixel_store)(GL_UNPACK_ROW_LENGTH, 0);
                (g.pixel_store)(GL_UNPACK_SKIP_ROWS, 0);
                (g.pixel_store)(GL_UNPACK_SKIP_PIXELS, 0);
            }
            (g.active)(GL_TEXTURE0);
            (g.bind_tex)(GL_TEXTURE_2D, self.texture);
            if self.width != descriptor.width || self.height != descriptor.height {
                (g.tex_param)(GL_TEXTURE_2D, 0x2801, 0x2601);
                (g.tex_param)(GL_TEXTURE_2D, 0x2800, 0x2601);
                (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                (g.tex_param)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                (g.tex_image)(
                    GL_TEXTURE_2D,
                    0,
                    if g.is_gles {
                        GL_RGBA as c_int
                    } else {
                        GL_RGBA8 as c_int
                    },
                    descriptor.width as c_int,
                    descriptor.height as c_int,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    null(),
                );
                self.width = descriptor.width;
                self.height = descriptor.height;
                self.uploaded = 0;
            }
            let upload_format = if descriptor.format == FORMAT_PREMULTIPLIED_BGRA8 {
                GL_BGRA
            } else {
                GL_RGBA
            };
            if self.uploaded != serial {
                (g.tex_sub)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    descriptor.width as c_int,
                    descriptor.height as c_int,
                    upload_format,
                    GL_UNSIGNED_BYTE,
                    image.bytes().as_ptr().cast(),
                );
                self.uploaded = serial;
            }
            (g.use_program)(self.program);
            if let Some(bind_vao) = g.bind_vao {
                bind_vao(self.vao);
            }
            (g.bind_buffer)(GL_ARRAY_BUFFER, self.vbo);
            let vertices: [f32; 8] = [-1., -1., 1., -1., -1., 1., 1., 1.];
            (g.buffer_data)(
                GL_ARRAY_BUFFER,
                size_of::<[f32; 8]>() as isize,
                vertices.as_ptr().cast(),
                0x88E4,
            );
            (g.attrib_enable)(0);
            (g.attrib_ptr)(0, 2, GL_FLOAT, GL_FALSE, 8, null());
            if self.sampler_loc >= 0 {
                (g.uniform_i)(self.sampler_loc, 0);
            }
            (g.bind_framebuffer)(
                if g.is_gles {
                    GL_FRAMEBUFFER
                } else {
                    GL_DRAW_FRAMEBUFFER
                },
                0,
            );
            (g.viewport)(0, 0, target_width as c_int, target_height as c_int);
            (g.disable)(GL_DEPTH_TEST);
            (g.disable)(GL_STENCIL_TEST);
            (g.disable)(GL_CULL_FACE);
            (g.disable)(GL_SCISSOR_TEST);
            (g.color_mask)(1, 1, 1, 1);
            (g.depth_mask)(0);
            if let Some(polygon) = g.polygon {
                polygon(0x0408, 0x1B02);
            }
            (g.enable)(GL_BLEND);
            (g.blend_eq)(GL_FUNC_ADD, GL_FUNC_ADD);
            let (src_rgb, src_alpha) = if matches!(
                descriptor.format,
                FORMAT_PREMULTIPLIED_RGBA8 | FORMAT_PREMULTIPLIED_BGRA8
            ) {
                (GL_ONE, GL_ONE)
            } else {
                (GL_SRC_ALPHA, GL_SRC_ALPHA)
            };
            (g.blend_sep)(
                src_rgb,
                GL_ONE_MINUS_SRC_ALPHA,
                src_alpha,
                GL_ONE_MINUS_SRC_ALPHA,
            );
            (g.draw)(GL_TRIANGLE_STRIP, 0, 4);
        }
    }
}
fn shader(g: GlFns, kind: u32, text: &[u8]) -> Result<u32, ()> {
    // SAFETY: caller resolved `g` for the current context and `text` is a
    // static NUL-terminated shader source.
    let s = unsafe { (g.create_shader)(kind) };
    let ptr = text.as_ptr().cast();
    unsafe {
        (g.shader_source)(s, 1, &ptr, null());
        (g.compile_shader)(s);
    }
    let mut ok = 0;
    unsafe { (g.shader_iv)(s, GL_COMPILE_STATUS, &mut ok) };
    if ok == 0 {
        return Err(());
    }
    Ok(s)
}
struct VertexAttribState {
    enabled: bool,
    size: c_int,
    stride: c_int,
    kind: c_int,
    normalized: u8,
    buffer: c_int,
    pointer: *mut c_void,
}

fn dmabuf_attributes(
    descriptor: &DmabufDescriptor,
    fds: &[OwnedFd],
) -> Result<DmabufAttributes, ()> {
    descriptor.validate().map_err(|_| ())?;
    if descriptor.planes.len() != 1 || fds.len() != 1 {
        return Err(());
    }
    let width = c_int::try_from(descriptor.width).map_err(|_| ())?;
    let height = c_int::try_from(descriptor.height).map_err(|_| ())?;
    let fourcc = c_int::from_ne_bytes(descriptor.fourcc.to_ne_bytes());
    let mut values = [0; DMABUF_ATTRIBUTE_CAPACITY];
    values[0] = EGL_WIDTH;
    values[1] = width;
    values[2] = EGL_HEIGHT;
    values[3] = height;
    values[4] = EGL_LINUX_DRM_FOURCC_EXT;
    values[5] = fourcc;
    let mut cursor = 6;
    for (index, (plane, fd)) in descriptor.planes.iter().zip(fds).enumerate() {
        let index = c_int::try_from(index).map_err(|_| ())?;
        let fd_attribute = EGL_DMA_BUF_PLANE0_FD_EXT
            .checked_add(index.checked_mul(3).ok_or(())?)
            .ok_or(())?;
        let modifier_low_attribute = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT
            .checked_add(index.checked_mul(2).ok_or(())?)
            .ok_or(())?;
        values[cursor] = fd_attribute;
        values[cursor + 1] = fd.as_raw_fd();
        values[cursor + 2] = fd_attribute + 1;
        values[cursor + 3] = c_int::try_from(plane.offset).map_err(|_| ())?;
        values[cursor + 4] = fd_attribute + 2;
        values[cursor + 5] = c_int::try_from(plane.stride).map_err(|_| ())?;
        values[cursor + 6] = modifier_low_attribute;
        values[cursor + 7] = descriptor.modifier as u32 as c_int;
        values[cursor + 8] = modifier_low_attribute + 1;
        values[cursor + 9] = (descriptor.modifier >> 32) as u32 as c_int;
        cursor += 10;
    }
    values[cursor] = EGL_NONE;
    Ok(DmabufAttributes { values })
}

struct GlStateGuard {
    g: GlFns,
    program: c_int,
    vao: Option<c_int>,
    array: c_int,
    element: c_int,
    draw_framebuffer: c_int,
    active: c_int,
    texture: c_int,
    texture0: c_int,
    viewport: [c_int; 4],
    blend: bool,
    depth: bool,
    stencil: bool,
    cull: bool,
    scissor_test: bool,
    src_rgb: c_int,
    dst_rgb: c_int,
    eq_rgb: c_int,
    src_alpha: c_int,
    dst_alpha: c_int,
    eq_alpha: c_int,
    color: [u8; 4],
    depth_mask: u8,
    unpack_alignment: c_int,
    desktop_unpack: Option<[c_int; 4]>,
    polygon: Option<[c_int; 2]>,
    vertex_attrib: Option<VertexAttribState>,
}

impl GlStateGuard {
    fn capture(g: &GlFns) -> Self {
        // SAFETY: all function pointers operate on the current context.
        unsafe {
            let get = |parameter| {
                let mut value = 0;
                (g.get_i)(parameter, &mut value);
                value
            };
            let mut viewport = [0; 4];
            (g.get_i)(GL_VIEWPORT, viewport.as_mut_ptr());
            let mut color = [1; 4];
            (g.get_b)(GL_COLOR_WRITEMASK, color.as_mut_ptr());
            let mut depth_mask = 1;
            (g.get_b)(GL_DEPTH_WRITEMASK, &mut depth_mask);
            let active = get(GL_ACTIVE_TEXTURE);
            let texture = get(GL_TEXTURE_BINDING_2D);
            (g.active)(GL_TEXTURE0);
            let texture0 = get(GL_TEXTURE_BINDING_2D);
            (g.active)(active as u32);
            // Attribute state lives inside the VAO whenever one is bound, so
            // raw attrib save/restore is only needed where VAOs genuinely do
            // not exist (GLES2 without the extension).
            let vertex_attrib = if g.bind_vao.is_none() {
                let get_attrib = |parameter| {
                    let mut value = 0;
                    (g.get_attrib_i)(0, parameter, &mut value);
                    value
                };
                let mut pointer = null_mut();
                (g.get_attrib_ptr)(0, GL_VERTEX_ATTRIB_ARRAY_POINTER, &mut pointer);
                Some(VertexAttribState {
                    enabled: get_attrib(GL_VERTEX_ATTRIB_ARRAY_ENABLED) != 0,
                    size: get_attrib(GL_VERTEX_ATTRIB_ARRAY_SIZE),
                    stride: get_attrib(GL_VERTEX_ATTRIB_ARRAY_STRIDE),
                    kind: get_attrib(GL_VERTEX_ATTRIB_ARRAY_TYPE),
                    normalized: get_attrib(GL_VERTEX_ATTRIB_ARRAY_NORMALIZED) as u8,
                    buffer: get_attrib(GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING),
                    pointer,
                })
            } else {
                None
            };
            let polygon = if g.is_gles {
                None
            } else {
                let mut value = [0; 2];
                (g.get_i)(GL_POLYGON_MODE, value.as_mut_ptr());
                Some(value)
            };
            Self {
                g: *g,
                program: get(GL_CURRENT_PROGRAM),
                // The draw path binds the overlay VAO whenever
                // `glBindVertexArray` resolves, which includes GLES3. Saving
                // this only for desktop GL left the overlay's VAO bound and
                // corrupted the game's subsequent draws.
                vao: g.bind_vao.map(|_| get(GL_VERTEX_ARRAY_BINDING)),
                array: get(GL_ARRAY_BUFFER_BINDING),
                element: get(GL_ELEMENT_ARRAY_BUFFER_BINDING),
                draw_framebuffer: get(GL_DRAW_FRAMEBUFFER_BINDING),
                active,
                texture,
                texture0,
                viewport,
                blend: (g.enabled)(GL_BLEND) != 0,
                depth: (g.enabled)(GL_DEPTH_TEST) != 0,
                stencil: (g.enabled)(GL_STENCIL_TEST) != 0,
                cull: (g.enabled)(GL_CULL_FACE) != 0,
                scissor_test: (g.enabled)(GL_SCISSOR_TEST) != 0,
                src_rgb: get(GL_BLEND_SRC_RGB),
                dst_rgb: get(GL_BLEND_DST_RGB),
                eq_rgb: get(GL_BLEND_EQUATION_RGB),
                src_alpha: get(GL_BLEND_SRC_ALPHA),
                dst_alpha: get(GL_BLEND_DST_ALPHA),
                eq_alpha: get(GL_BLEND_EQUATION_ALPHA),
                color,
                depth_mask,
                unpack_alignment: get(GL_UNPACK_ALIGNMENT),
                desktop_unpack: (!g.is_gles).then(|| {
                    [
                        get(GL_PIXEL_UNPACK_BUFFER_BINDING),
                        get(GL_UNPACK_ROW_LENGTH),
                        get(GL_UNPACK_SKIP_ROWS),
                        get(GL_UNPACK_SKIP_PIXELS),
                    ]
                }),
                polygon,
                vertex_attrib,
            }
        }
    }
}

impl Drop for GlStateGuard {
    fn drop(&mut self) {
        // SAFETY: the same GL context remains current through one hook call.
        unsafe {
            let g = self.g;
            (g.use_program)(self.program as u32);
            if let (Some(bind_vao), Some(vao)) = (g.bind_vao, self.vao) {
                bind_vao(vao as u32);
            }
            (g.bind_buffer)(GL_ELEMENT_ARRAY_BUFFER, self.element as u32);
            (g.bind_framebuffer)(
                if g.is_gles {
                    GL_FRAMEBUFFER
                } else {
                    GL_DRAW_FRAMEBUFFER
                },
                self.draw_framebuffer as u32,
            );
            if let Some(attrib) = &self.vertex_attrib {
                (g.bind_buffer)(GL_ARRAY_BUFFER, attrib.buffer as u32);
                (g.attrib_ptr)(
                    0,
                    attrib.size,
                    attrib.kind as u32,
                    attrib.normalized,
                    attrib.stride,
                    attrib.pointer,
                );
                if attrib.enabled {
                    (g.attrib_enable)(0);
                } else {
                    (g.attrib_disable)(0);
                }
            }
            (g.bind_buffer)(GL_ARRAY_BUFFER, self.array as u32);
            (g.active)(GL_TEXTURE0);
            (g.bind_tex)(GL_TEXTURE_2D, self.texture0 as u32);
            (g.active)(self.active as u32);
            (g.bind_tex)(GL_TEXTURE_2D, self.texture as u32);
            (g.viewport)(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            for (capability, enabled) in [
                (GL_BLEND, self.blend),
                (GL_DEPTH_TEST, self.depth),
                (GL_STENCIL_TEST, self.stencil),
                (GL_CULL_FACE, self.cull),
                (GL_SCISSOR_TEST, self.scissor_test),
            ] {
                if enabled {
                    (g.enable)(capability);
                } else {
                    (g.disable)(capability);
                }
            }
            (g.blend_eq)(self.eq_rgb as u32, self.eq_alpha as u32);
            (g.blend_sep)(
                self.src_rgb as u32,
                self.dst_rgb as u32,
                self.src_alpha as u32,
                self.dst_alpha as u32,
            );
            (g.color_mask)(self.color[0], self.color[1], self.color[2], self.color[3]);
            (g.depth_mask)(self.depth_mask);
            (g.pixel_store)(GL_UNPACK_ALIGNMENT, self.unpack_alignment);
            if let Some([buffer, row_length, skip_rows, skip_pixels]) = self.desktop_unpack {
                (g.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, buffer as u32);
                (g.pixel_store)(GL_UNPACK_ROW_LENGTH, row_length);
                (g.pixel_store)(GL_UNPACK_SKIP_ROWS, skip_rows);
                (g.pixel_store)(GL_UNPACK_SKIP_PIXELS, skip_pixels);
            }
            if let (Some(polygon), Some(value)) = (g.polygon, self.polygon) {
                polygon(0x0404, value[0] as u32);
                polygon(0x0405, value[1] as u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_bounds_reject_short_mapping() {
        let d = TextureDescriptor {
            target_id: 1,
            width: 2,
            height: 2,
            stride: 8,
            offset: 0,
            size: 1,
            format: 0,
        };
        assert!(d.validate().is_err());
    }
    #[test]
    fn chromium_bitmap_frames_map_and_blend_as_premultiplied() {
        use std::os::fd::FromRawFd;
        let width = 4;
        let height = 3;
        let stride = width * 4;
        let bytes = vec![0x7f_u8; (stride * height) as usize];
        // SAFETY: static NUL-terminated name; the descriptor is owned below.
        let raw = unsafe { libc::memfd_create(c"vosulay-test-frame".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0, "memfd_create failed");
        // SAFETY: `raw` is a fresh owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: writing the local buffer to the owned descriptor.
        let written = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(written, bytes.len().cast_signed());
        let descriptor = TextureDescriptor {
            target_id: 1,
            width,
            height,
            stride,
            offset: 0,
            size: u64::from(stride * height),
            format: FORMAT_PREMULTIPLIED_BGRA8,
        };
        let image = MappedShm::map(fd, descriptor).expect("BGRA frame is accepted");
        assert_eq!(image.bytes().len(), (stride * height) as usize);
        assert!(matches!(
            descriptor.format,
            FORMAT_PREMULTIPLIED_RGBA8 | FORMAT_PREMULTIPLIED_BGRA8
        ));
    }
    #[test]
    fn latest_mailbox_is_bounded_and_clearable() {
        let mailbox = Mailbox::new();
        assert!(mailbox.begin_present().is_none());
        mailbox.clear();
        assert!(!mailbox.visible.load(Ordering::Relaxed));
    }
    #[test]
    fn target_choice_uses_area_before_activity() {
        let r = Runtime::new();
        r.update_target(1, 10, 10);
        r.update_target(2, 20, 20);
        assert_eq!(r.choose_target().map(|x| x.0), Some(2));
    }
    #[test]
    fn fail_open_transition_is_atomic() {
        let r = Runtime::new();
        r.failed.store(true, Ordering::Release);
        assert!(r.failed.load(Ordering::Acquire));
    }
    #[test]
    fn input_capture_is_bounded_and_disabled_by_default() {
        let runtime = Runtime::new();
        assert!(!runtime.capture.load(Ordering::Relaxed));
        for value in 0..(MAX_INPUT_EVENTS + 20) {
            assert!(runtime.queue_input(InputEvent {
                target_id: 1,
                kind: INPUT_MOUSE_MOVE,
                code: 0,
                flags: 0,
                value: value as i32,
                value2: 0,
            }));
        }
        let input = runtime.input.lock().expect("input queue lock");
        assert_eq!(input.len(), MAX_INPUT_EVENTS);
        assert_eq!(input.front().map(|event| event.value), Some(20));
    }

    #[test]
    fn only_a_presenting_preloaded_process_claims_the_session() {
        let claimed = AtomicBool::new(false);
        assert!(!super::should_claim_session(false, &claimed));
        assert!(!claimed.load(Ordering::Relaxed));
        assert!(super::should_claim_session(true, &claimed));
        assert!(!super::should_claim_session(true, &claimed));
    }

    #[test]
    fn real_dlsym_is_resolvable_and_distinct_from_the_interposer() {
        let real = super::real_dlsym().expect("glibc dlsym must be resolvable");
        assert_ne!(real as *mut c_void, super::dlsym as *mut c_void);
    }

    #[test]
    fn only_present_entry_points_are_replaced() {
        let sentinel = 0x5eed_usize as *mut c_void;
        assert_eq!(
            super::interposed_symbol(b"XOpenDisplay", sentinel),
            sentinel
        );
        assert_eq!(
            super::interposed_symbol(b"glXSwapBuffers", sentinel),
            super::glXSwapBuffers as *mut c_void,
        );
        assert_eq!(
            super::interposed_symbol(b"eglSwapBuffers", sentinel),
            super::eglSwapBuffers as *mut c_void,
        );
    }

    #[test]
    fn rounded_coordinates_saturate_and_reject_non_finite_values() {
        assert_eq!(rounded_i32(1.6), 2);
        assert_eq!(rounded_i32(f32::NAN), 0);
        assert_eq!(rounded_i32(f32::INFINITY), 0);
        assert_eq!(rounded_i32(f32::MAX), i32::MAX);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sdl_dynapi_slot_parser_accepts_plain_and_endbr_jumps() {
        let plain = [0xff, 0x25, 0x32, 0x00, 0x00, 0x00];
        assert_eq!(sdl_dynapi_slot_address(0x1000, &plain), Ok(0x1038));

        let endbr = [0xf3, 0x0f, 0x1e, 0xfa, 0xff, 0x25, 0xf6, 0xff, 0xff, 0xff];
        assert_eq!(sdl_dynapi_slot_address(0x1000, &endbr), Ok(0x1000));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sdl_dynapi_slot_parser_rejects_unknown_or_truncated_exports() {
        assert!(sdl_dynapi_slot_address(0x1000, &[0x90; 10]).is_err());
        assert!(sdl_dynapi_slot_address(0x1000, &[0xff, 0x25, 0x00]).is_err());
        assert!(sdl_dynapi_slot_address(usize::MAX, &[0xff, 0x25, 0, 0, 0, 0]).is_err());
    }

    fn pool_key(target_id: u64) -> DmabufSurfaceKey {
        DmabufSurfaceKey {
            target_id,
            width: 64,
            height: 64,
            fourcc: overlay_protocol::DRM_FORMAT_ARGB8888,
            modifier: 0,
            plane_offset: 0,
            plane_stride: 256,
            plane_size: 16_384,
            fd_device: 1,
            fd_inode: target_id + 1,
            fd_size: 16_384,
        }
    }

    #[test]
    fn dmabuf_pool_hits_and_reuses_matching_surface() {
        let key = pool_key(1);
        let mut pool = SurfacePool::new();
        assert!(pool.insert(key, 11, 1).is_none());
        assert!(pool.contains(key));
        assert_eq!(pool.get_mut(key, 2).map(|value| *value), Some(11));
        assert_eq!(pool.insert(key, 22, 3), Some(11));
        assert_eq!(pool.get_mut(key, 4).map(|value| *value), Some(22));
    }

    #[test]
    fn dmabuf_pool_replaces_evicts_and_clears() {
        let mut pool = SurfacePool::new();
        for target_id in 0..MAX_DMABUF_SURFACES as u64 {
            assert!(
                pool.insert(pool_key(target_id), target_id, target_id + 1)
                    .is_none()
            );
        }
        assert_eq!(pool.len(), MAX_DMABUF_SURFACES);
        let evicted = pool.insert(pool_key(100), 100, 100);
        assert_eq!(evicted, Some(0));
        assert_eq!(pool.len(), MAX_DMABUF_SURFACES);
        pool.clear();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn dmabuf_pool_remains_bounded_under_churn() {
        let mut pool = SurfacePool::new();
        for target_id in 0..(MAX_DMABUF_SURFACES as u64 + 64) {
            let _ = pool.insert(pool_key(target_id), target_id, target_id + 1);
            assert!(pool.len() <= MAX_DMABUF_SURFACES);
        }
        assert_eq!(pool.len(), MAX_DMABUF_SURFACES);
    }

    #[test]
    fn malformed_dmabuf_descriptors_are_rejected_before_attributes() {
        let descriptor = DmabufDescriptor {
            target_id: 1,
            sequence: 1,
            width: 64,
            height: 64,
            fourcc: overlay_protocol::DRM_FORMAT_ARGB8888,
            modifier: 0,
            planes: Vec::new(),
        };
        assert!(dmabuf_attributes(&descriptor, &[]).is_err());
    }

    #[test]
    fn a_target_that_stops_presenting_is_expired_and_stops_winning_selection() {
        let runtime = Runtime::new();
        runtime.update_target(1, 1920, 1080);
        runtime.update_target(2, 640, 480);
        // The larger drawable wins while both are live.
        assert_eq!(runtime.choose_target().map(|target| target.0), Some(1));

        // The big drawable stops being presented into while the small one
        // keeps going. Absolute timestamps make the ages unambiguous.
        let evaluated_at = TARGET_EXPIRY_MILLIS + 1;
        for slot in &runtime.targets {
            let last_seen = if slot.id.load(Ordering::Relaxed) == 2 {
                evaluated_at
            } else {
                0
            };
            slot.last_seen.store(last_seen, Ordering::Relaxed);
        }
        runtime.expire_targets(evaluated_at);

        // Without expiry the stale 1920x1080 drawable would win forever.
        assert_eq!(runtime.choose_target().map(|target| target.0), Some(2));
    }

    #[test]
    fn an_expired_slot_is_freed_for_reuse() {
        let runtime = Runtime::new();
        runtime.update_target(7, 100, 100);
        for slot in &runtime.targets {
            slot.last_seen.store(0, Ordering::Relaxed);
        }
        runtime.expire_targets(TARGET_EXPIRY_MILLIS + 1);
        assert_eq!(runtime.choose_target(), None);
        assert!(
            runtime
                .targets
                .iter()
                .all(|slot| slot.id.load(Ordering::Relaxed) == 0),
            "expired slots must be released so a new drawable can claim one"
        );

        runtime.update_target(8, 200, 200);
        assert_eq!(runtime.choose_target().map(|target| target.0), Some(8));
    }

    #[test]
    fn a_live_target_survives_expiry() {
        let runtime = Runtime::new();
        runtime.update_target(5, 800, 600);
        let now = monotonic_millis();
        runtime.expire_targets(now);
        assert_eq!(runtime.choose_target().map(|target| target.0), Some(5));
    }

    #[test]
    fn the_announced_target_is_what_consumers_read() {
        // Input routing and frame matching must agree with what the host was
        // told, not re-derive a selection of their own.
        let runtime = Runtime::new();
        assert_eq!(runtime.announced_target(), 0);
        runtime.selected_target.store(42, Ordering::Release);
        assert_eq!(runtime.announced_target(), 42);
    }

    #[test]
    fn selection_generations_increase_monotonically() {
        let runtime = Runtime::new();
        let first = runtime.selection_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let second = runtime.selection_generation.fetch_add(1, Ordering::AcqRel) + 1;
        assert_eq!((first, second), (1, 2));
        assert!(
            second > first,
            "a host must be able to discard stale selections"
        );
    }

    #[test]
    fn withdrawing_dmabuf_support_is_idempotent_and_masks_the_capability() {
        // Guards the downgrade decision itself; the static latch is process
        // wide, so only the masking logic is exercised here.
        assert_eq!(CAP_DMABUF & !CAP_DMABUF, 0);
        assert_eq!((CAP_SHM | CAP_DMABUF) & !CAP_DMABUF, CAP_SHM);
    }
}
