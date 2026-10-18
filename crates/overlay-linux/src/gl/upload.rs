//! Overlay uploads off the game's GL command stream (EGL games).
//!
//! The CPU-to-GPU transfer of a changed overlay frame is link-bound (about
//! 2 ms for a full 2560x1440 frame on a PCIe Gen4 x4 GPU). Issued in the
//! game's command stream it delays the game's next frame by that much. Here a
//! thread with its own context, sharing objects with the game's, copies the
//! changed rows into a staging texture and fences it; the game's swap later
//! sees the fence signalled, without waiting, and copies only that rectangle
//! into the displayed texture on the GPU (`glCopyImageSubData`, microseconds).
//! On NVIDIA the shared context's transfer then overlaps the game's rendering.
//!
//! Only EGL contexts take this path: a GLX share context would need the
//! game's `Display` from another thread, which Xlib only allows after
//! `XInitThreads`. Everything else, and any failure here, keeps the in-stream
//! upload in `Renderer::compose`.
use super::{
    GL_RGBA, GL_RGBA8, GL_TEXTURE_2D, GL_UNSIGNED_BYTE, Gl, set_unpack_defaults, upload_rects,
};
use libc::{c_int, c_uint, c_void};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::{self, RecvTimeoutError},
    },
    thread::JoinHandle,
    time::Duration,
};
use vosulay_frame::DamageRects;
use vosulay_transport::ReadFrame;

type Sync = *mut c_void;
const GL_SYNC_GPU_COMMANDS_COMPLETE: c_uint = 0x9117;
const GL_SYNC_FLUSH_COMMANDS_BIT: c_uint = 0x0001;
const GL_ALREADY_SIGNALED: c_uint = 0x911a;
const GL_CONDITION_SATISFIED: c_uint = 0x911c;
const GL_TIMEOUT_EXPIRED: c_uint = 0x911b;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_NEAREST: c_int = 0x2600;
const GL_CONTEXT_PROFILE_MASK: c_uint = 0x9126;
/// Staging textures: one being filled while the game copies the other.
const STAGING: usize = 2;

const EGL_CONFIG_ID: c_int = 0x3028;
const EGL_CONTEXT_CLIENT_TYPE: c_int = 0x3097;
const EGL_OPENGL_ES_API: c_uint = 0x30a0;
const EGL_CONTEXT_MAJOR_VERSION: c_int = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: c_int = 0x30fb;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: c_int = 0x30fd;
const EGL_WIDTH: c_int = 0x3057;
const EGL_HEIGHT: c_int = 0x3056;
const EGL_NONE: c_int = 0x3038;
const EGL_EXTENSIONS: c_int = 0x3055;

type GetCurrentDisplay = unsafe extern "C" fn() -> *mut c_void;
type GetCurrentContext = unsafe extern "C" fn() -> *mut c_void;
type QueryContext = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;
type ChooseConfig =
    unsafe extern "C" fn(*mut c_void, *const c_int, *mut *mut c_void, c_int, *mut c_int) -> c_uint;
type CreateContext =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_int) -> *mut c_void;
type MakeCurrent =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> c_uint;
type DestroyContext = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type BindApi = unsafe extern "C" fn(c_uint) -> c_uint;
type QueryString = unsafe extern "C" fn(*mut c_void, c_int) -> *const libc::c_char;
type CreatePbuffer = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> *mut c_void;
type DestroySurface = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;

/// The real EGL entry points (never vosulay's own hooks).
#[derive(Clone, Copy)]
struct Egl {
    current_display: GetCurrentDisplay,
    current_context: GetCurrentContext,
    query_context: QueryContext,
    choose_config: ChooseConfig,
    create_context: CreateContext,
    make_current: MakeCurrent,
    destroy_context: DestroyContext,
    bind_api: BindApi,
    query_string: QueryString,
    create_pbuffer: CreatePbuffer,
    destroy_surface: DestroySurface,
}
impl Egl {
    fn load() -> Option<Self> {
        unsafe fn real<T: Copy>(name: &[u8]) -> Option<T> {
            unsafe { crate::hooks::original(name) }
        }
        // SAFETY: each name resolves to the EGL function with this signature.
        unsafe {
            Some(Self {
                current_display: real(b"eglGetCurrentDisplay\0")?,
                current_context: real(b"eglGetCurrentContext\0")?,
                query_context: real(b"eglQueryContext\0")?,
                choose_config: real(b"eglChooseConfig\0")?,
                create_context: real(b"eglCreateContext\0")?,
                make_current: real(b"eglMakeCurrent\0")?,
                destroy_context: real(b"eglDestroyContext\0")?,
                bind_api: real(b"eglBindAPI\0")?,
                query_string: real(b"eglQueryString\0")?,
                create_pbuffer: real(b"eglCreatePbufferSurface\0")?,
                destroy_surface: real(b"eglDestroySurface\0")?,
            })
        }
    }
}

/// What the game's current EGL context needs from a context sharing with it.
struct ShareTarget {
    display: usize,
    context: usize,
    config: usize,
    api: c_uint,
    attributes: [c_int; 7],
    surfaceless: bool,
}
// SAFETY: EGL handles are process-global; they are only passed to EGL.
unsafe impl Send for ShareTarget {}

/// A changed region waiting in a staging texture for the game's copy.
struct Done {
    staging: usize,
    ready: usize,
    rects: DamageRects,
    sequence: u64,
    full: bool,
}
struct Offer {
    frame: ReadFrame,
    full: bool,
}
#[derive(Default)]
struct State {
    offer: Option<Offer>,
    done: VecDeque<Done>,
    /// Staging textures the game has finished copying from, each with the
    /// fence of that copy.
    free: Vec<(usize, usize)>,
    stop: bool,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// A copy the game issued into the displayed texture.
pub(super) struct Applied {
    pub(super) sequence: u64,
    pub(super) full: bool,
}

pub(super) struct Uploader {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Uploader {
    /// Starts an uploader for the game's current EGL context, or None when it
    /// cannot share one (not EGL, no `glCopyImageSubData`, no usable config
    /// or surface, or `VOSULAY_OVERLAY_ASYNC_UPLOAD=0`).
    ///
    /// # Safety
    /// The game's context is current on this thread, `gl` was loaded for it,
    /// and no unpack buffer is bound.
    pub(super) unsafe fn start(
        gl: &Gl,
        is_es: bool,
        version: (u8, u8),
        width: u32,
        height: u32,
    ) -> Option<Self> {
        if std::env::var_os("VOSULAY_OVERLAY_ASYNC_UPLOAD").is_some_and(|value| value == "0") {
            return None;
        }
        gl.copy_image?;
        let egl = Egl::load()?;
        // SAFETY: EGL queries of the calling thread's current context.
        let target = unsafe { share_target(&egl, gl, is_es, version) }?;
        let mut staging = [0; STAGING];
        // SAFETY: the game's context is current; the textures belong to its
        // share group, so the uploader's context can write them.
        unsafe {
            (gl.gen_textures)(STAGING as c_int, staging.as_mut_ptr());
            if staging.contains(&0) {
                (gl.delete_textures)(STAGING as c_int, staging.as_ptr());
                return None;
            }
            for texture in staging {
                (gl.bind_texture)(GL_TEXTURE_2D, texture);
                (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
                (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
                set_unpack_defaults(gl);
                (gl.tex_image)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA8,
                    width as c_int,
                    height as c_int,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    std::ptr::null(),
                );
            }
            // The uploader's context must see complete storage.
            (gl.flush)();
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                free: staging
                    .iter()
                    .map(|&texture| (texture as usize, 0))
                    .collect(),
                ..State::default()
            }),
            wake: Condvar::new(),
        });
        let (started, result) = mpsc::sync_channel(1);
        let thread = {
            let shared = Arc::clone(&shared);
            let gl = *gl;
            std::thread::Builder::new()
                .name("vosulay-upload".into())
                .spawn(move || run(egl, gl, target, width, shared, started))
                .ok()?
        };
        match result.recv_timeout(Duration::from_secs(2)) {
            Ok(true) => Some(Self {
                shared,
                thread: Some(thread),
            }),
            Ok(false) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                // SAFETY: the game's context is current and owns these names.
                unsafe { (gl.delete_textures)(STAGING as c_int, staging.as_ptr()) };
                None
            }
            Err(RecvTimeoutError::Timeout) => {
                // A context that cannot become current in two seconds is not
                // worth blocking a game swap for; the thread exits on stop.
                shared.state.lock().stop = true;
                shared.wake.notify_all();
                drop(thread);
                None
            }
        }
    }

    /// Hands the newest frame to the uploader, replacing one not yet started.
    pub(super) fn offer(&self, frame: ReadFrame, full: bool) {
        let mut state = self.shared.state.lock();
        // A replaced full request stays full: the display still needs it.
        let full = full || state.offer.as_ref().is_some_and(|offer| offer.full);
        state.offer = Some(Offer { frame, full });
        drop(state);
        self.shared.wake.notify_all();
    }

    /// Whether a full upload is offered or in flight.
    pub(super) fn full_pending(&self) -> bool {
        let state = self.shared.state.lock();
        state.offer.as_ref().is_some_and(|offer| offer.full)
            || state.done.iter().any(|done| done.full)
    }

    /// Copies every finished staging rectangle, in order, into `texture`
    /// without waiting; stops at the first transfer still running.
    ///
    /// # Safety
    /// The game's context is current with `texture` its displayed texture.
    pub(super) unsafe fn apply(&self, gl: &Gl, texture: c_uint) -> Option<Applied> {
        let copy = gl.copy_image?;
        let mut applied = None;
        let mut state = self.shared.state.lock();
        while let Some(done) = state.done.front() {
            // SAFETY: `ready` is a live fence of the share group.
            let status = unsafe { (gl.client_wait_sync)(done.ready as Sync, 0, 0) };
            if status != GL_ALREADY_SIGNALED && status != GL_CONDITION_SATISFIED {
                break;
            }
            let done = state.done.pop_front().expect("front exists");
            // SAFETY: both textures are RGBA8 of the frame's size and the
            // rectangles lie within them.
            unsafe {
                (gl.delete_sync)(done.ready as Sync);
                for rect in done.rects.as_slice() {
                    copy(
                        done.staging as c_uint,
                        GL_TEXTURE_2D,
                        0,
                        rect.x as c_int,
                        rect.y as c_int,
                        0,
                        texture,
                        GL_TEXTURE_2D,
                        0,
                        rect.x as c_int,
                        rect.y as c_int,
                        0,
                        rect.width as c_int,
                        rect.height as c_int,
                        1,
                    );
                }
                let copied = (gl.fence_sync)(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
                (gl.flush)();
                state.free.push((done.staging, copied as usize));
            }
            applied = Some(Applied {
                sequence: done.sequence,
                full: done.full || applied.as_ref().is_some_and(|a: &Applied| a.full),
            });
        }
        drop(state);
        if applied.is_some() {
            self.shared.wake.notify_all();
        }
        applied
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.shared.state.lock().stop = true;
        self.shared.wake.notify_all();
        // The thread deletes the staging textures and its context.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reads the game's current EGL context's API, config and version.
///
/// # Safety
/// `gl` was loaded for the calling thread's current context.
unsafe fn share_target(egl: &Egl, gl: &Gl, is_es: bool, version: (u8, u8)) -> Option<ShareTarget> {
    // SAFETY: plain EGL queries of current handles.
    unsafe {
        let context = (egl.current_context)();
        let display = (egl.current_display)();
        if context.is_null() || display.is_null() {
            return None;
        }
        let mut client_type = 0;
        if (egl.query_context)(display, context, EGL_CONTEXT_CLIENT_TYPE, &mut client_type) == 0 {
            return None;
        }
        if (client_type as c_uint == EGL_OPENGL_ES_API) != is_es {
            return None;
        }
        let mut config_id = 0;
        let mut config = std::ptr::null_mut();
        if (egl.query_context)(display, context, EGL_CONFIG_ID, &mut config_id) != 0
            && config_id != 0
        {
            let mut found = 0;
            let attributes = [EGL_CONFIG_ID, config_id, EGL_NONE];
            if (egl.choose_config)(display, attributes.as_ptr(), &mut config, 1, &mut found) == 0
                || found != 1
            {
                return None;
            }
        }
        let extensions = (egl.query_string)(display, EGL_EXTENSIONS);
        let surfaceless = !extensions.is_null()
            && std::ffi::CStr::from_ptr(extensions)
                .to_bytes()
                .split(|&byte| byte == b' ')
                .any(|name| name == b"EGL_KHR_surfaceless_context");
        if !surfaceless && config.is_null() {
            return None;
        }
        let mut profile = 0;
        if !is_es {
            (gl.get_integer)(GL_CONTEXT_PROFILE_MASK, &mut profile);
        }
        let mut attributes = [
            EGL_CONTEXT_MAJOR_VERSION,
            c_int::from(version.0),
            EGL_CONTEXT_MINOR_VERSION,
            c_int::from(version.1),
            EGL_NONE,
            EGL_NONE,
            EGL_NONE,
        ];
        if !is_es && profile != 0 {
            attributes[4] = EGL_CONTEXT_OPENGL_PROFILE_MASK;
            attributes[5] = profile;
        }
        Some(ShareTarget {
            display: display as usize,
            context: context as usize,
            config: config as usize,
            api: client_type as c_uint,
            attributes,
            surfaceless,
        })
    }
}

/// The uploader thread: creates its shared context, then turns offered
/// frames into fenced staging uploads until stopped.
fn run(
    egl: Egl,
    gl: Gl,
    target: ShareTarget,
    width: u32,
    shared: Arc<Shared>,
    started: mpsc::SyncSender<bool>,
) {
    let display = target.display as *mut c_void;
    // SAFETY: EGL calls on this thread with handles of the game's display.
    let (context, surface) = unsafe {
        if (egl.bind_api)(target.api) == 0 {
            let _ = started.send(false);
            return;
        }
        let context = (egl.create_context)(
            display,
            target.config as *mut c_void,
            target.context as *mut c_void,
            target.attributes.as_ptr(),
        );
        if context.is_null() {
            let _ = started.send(false);
            return;
        }
        let surface = if target.surfaceless {
            std::ptr::null_mut()
        } else {
            (egl.create_pbuffer)(
                display,
                target.config as *mut c_void,
                [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE].as_ptr(),
            )
        };
        if (!target.surfaceless && surface.is_null())
            || (egl.make_current)(display, surface, surface, context) == 0
        {
            if !surface.is_null() {
                (egl.destroy_surface)(display, surface);
            }
            (egl.destroy_context)(display, context);
            let _ = started.send(false);
            return;
        }
        (context, surface)
    };
    let mut buffer = 0;
    // SAFETY: this thread's context is current.
    unsafe { (gl.gen_buffers)(1, &mut buffer) };
    let _ = started.send(true);
    drop(started);
    let mut uploaded = 0u64;
    loop {
        let (offer, staging) = {
            let mut state = shared.state.lock();
            loop {
                if state.stop {
                    break;
                }
                if state.offer.is_some() && !state.free.is_empty() {
                    break;
                }
                shared.wake.wait(&mut state);
            }
            if state.stop {
                break;
            }
            (
                state.offer.take().expect("checked"),
                state.free.pop().expect("checked"),
            )
        };
        let (texture, copied) = staging;
        // SAFETY: this thread's context is current; fences and textures
        // belong to the shared group.
        unsafe {
            if copied != 0 {
                // The game's copy out of this staging texture must finish
                // before it is overwritten. This blocks only this thread.
                loop {
                    let status = (gl.client_wait_sync)(
                        copied as Sync,
                        GL_SYNC_FLUSH_COMMANDS_BIT,
                        1_000_000,
                    );
                    if status != GL_TIMEOUT_EXPIRED || shared.state.lock().stop {
                        break;
                    }
                }
                (gl.delete_sync)(copied as Sync);
            }
            let frame = offer.frame;
            let full = offer.full || uploaded == 0;
            let rects = (!full)
                .then(|| frame.damage_since(uploaded))
                .flatten()
                .unwrap_or_else(|| DamageRects::full(frame.width(), frame.height()));
            (gl.bind_texture)(GL_TEXTURE_2D, texture as c_uint);
            set_unpack_defaults(&gl);
            upload_rects(&gl, buffer, frame.pixels(), width, rects.as_slice());
            set_unpack_defaults(&gl);
            #[cfg(feature = "profile")]
            crate::profile::upload(rects.pixels() as usize * 4);
            let sequence = frame.sequence();
            // The rows are in GL's hands; release the ring slot.
            drop(frame);
            let ready = (gl.fence_sync)(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
            (gl.flush)();
            if ready.is_null() {
                // Without a fence the game could never apply it; give up on
                // this path rather than stall the overlay.
                shared.state.lock().stop = true;
                break;
            }
            uploaded = sequence;
            shared.state.lock().done.push_back(Done {
                staging: texture,
                ready: ready as usize,
                rects,
                sequence,
                full,
            });
        }
    }
    // SAFETY: this thread's context is current; release everything it made
    // or still holds, then the context itself.
    unsafe {
        let mut state = shared.state.lock();
        let mut textures = Vec::with_capacity(STAGING);
        for done in state.done.drain(..) {
            (gl.delete_sync)(done.ready as Sync);
            textures.push(done.staging as c_uint);
        }
        for (texture, copied) in state.free.drain(..) {
            if copied != 0 {
                (gl.client_wait_sync)(copied as Sync, GL_SYNC_FLUSH_COMMANDS_BIT, 100_000_000);
                (gl.delete_sync)(copied as Sync);
            }
            textures.push(texture as c_uint);
        }
        state.offer = None;
        drop(state);
        (gl.delete_buffers)(1, &buffer);
        (gl.delete_textures)(textures.len() as c_int, textures.as_ptr());
        (gl.finish)();
        (egl.make_current)(
            display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if !surface.is_null() {
            (egl.destroy_surface)(display, surface);
        }
        (egl.destroy_context)(display, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vosulay_transport::{Consumer, Producer};

    fn frames() -> (Producer, Consumer) {
        let mut producer = Producer::new(4, 4).unwrap();
        producer.publish(&[0; 64], None).unwrap();
        let handle = producer.handle();
        let memory = handle.descriptor().try_clone_to_owned().unwrap();
        (producer, Consumer::from_handle(memory, 4, 4).unwrap())
    }

    fn idle() -> Uploader {
        Uploader {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
            }),
            thread: None,
        }
    }

    #[test]
    fn a_newer_frame_never_downgrades_a_pending_full_upload() {
        let (mut producer, mut consumer) = frames();
        let uploader = idle();
        uploader.offer(consumer.latest().unwrap(), true);
        producer.publish(&[1; 64], None).unwrap();
        // The display texture is still undefined: the newer frame must also
        // be uploaded whole, not just the region that changed since.
        uploader.offer(consumer.latest().unwrap(), false);
        assert!(uploader.full_pending());
        let state = uploader.shared.state.lock();
        let offer = state.offer.as_ref().unwrap();
        assert!(offer.full);
        assert_eq!(offer.frame.sequence(), 2);
    }

    #[test]
    fn a_replaced_offer_releases_its_ring_slot() {
        let (mut producer, mut consumer) = frames();
        let uploader = idle();
        uploader.offer(consumer.latest().unwrap(), false);
        for value in 1..8 {
            producer.publish(&[value; 64], None).unwrap();
            uploader.offer(consumer.latest().unwrap(), false);
        }
        // Each replaced offer dropped its lease, so the producer still finds
        // slots and the consumer sees the newest frame.
        assert!(producer.publish(&[9; 64], None).unwrap());
        assert!(!uploader.full_pending());
        assert_eq!(consumer.latest().unwrap().sequence(), 9);
    }
}
