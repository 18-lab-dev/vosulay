//! Native GL presentation interposition. No work is performed by constructors.
use crate::{client::Client, gl::Renderer};
use libc::{c_char, c_int, c_uint, c_ulong, c_void};
use parking_lot::Mutex;
use std::{
    cell::Cell,
    ffi::CStr,
    mem,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
thread_local! {
    static ENTERED: Cell<bool> = const { Cell::new(false) };
    static SDL_PRESENT: Cell<Option<SdlPresent>> = const { Cell::new(None) };
}
#[derive(Clone, Copy)]
struct SdlPresent {
    window: usize,
    width: u32,
    height: u32,
    focused: bool,
    drawn: bool,
}
struct RestoreSdlPresent(Option<SdlPresent>);
impl Drop for RestoreSdlPresent {
    fn drop(&mut self) {
        SDL_PRESENT.with(|present| present.set(self.0));
    }
}
pub(crate) fn with_sdl_present<T>(
    window: usize,
    width: u32,
    height: u32,
    focused: bool,
    present: impl FnOnce() -> T,
) -> T {
    let previous = SDL_PRESENT.with(|state| {
        let previous = state.get();
        state.set(Some(SdlPresent {
            window,
            width,
            height,
            focused,
            drawn: false,
        }));
        previous
    });
    let _restore = RestoreSdlPresent(previous);
    present()
}
// SDL itself swaps through GLX/EGL. Keep its surface metadata pending until the
// native swap interposer, which runs after an upstream capture layer has copied
// the pristine backbuffer.
fn draw_sdl_present() -> bool {
    SDL_PRESENT.with(|state| {
        let Some(mut present) = state.get() else {
            return false;
        };
        if !present.drawn {
            present.drawn = true;
            state.set(Some(present));
            process(
                Backend::Sdl,
                0,
                present.window,
                Some((present.width, present.height, present.focused)),
            );
        }
        true
    })
}
pub(crate) struct Guard;
impl Guard {
    pub(crate) fn enter() -> Option<Self> {
        ENTERED.with(|v| if v.replace(true) { None } else { Some(Self) })
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        ENTERED.with(|v| v.set(false));
    }
}
unsafe fn lookup_symbol(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    {
        unsafe { crate::lookup::resolve(handle, name) }
    }
    #[cfg(not(all(target_arch = "x86_64", target_env = "gnu")))]
    {
        unsafe { libc::dlsym(handle, name) }
    }
}
pub(crate) unsafe fn real_symbol(name: &[u8]) -> *const c_void {
    if name.last() != Some(&0) {
        return std::ptr::null();
    }
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    if name.starts_with(b"SDL_") {
        let address = unsafe { crate::lookup::sdl_symbol(name.as_ptr().cast()) };
        if !address.is_null() {
            return address.cast_const();
        }
    }
    let next = unsafe { lookup_symbol(libc::RTLD_NEXT, name.as_ptr().cast()) };
    if !next.is_null() {
        return next.cast_const();
    }
    // Default scope can re-enter an upstream capture hook when the native
    // provider is RTLD_LOCAL. Resolve loaded providers by handle instead.
    const LIBRARIES: [&[u8]; 8] = [
        b"libGL.so.1\0",
        b"libGLX.so.0\0",
        b"libEGL.so.1\0",
        b"libSDL2-2.0.so.0\0",
        b"libSDL3.so.0\0",
        b"libX11.so.6\0",
        b"libOpenGL.so.0\0",
        b"libGLESv2.so.2\0",
    ];
    for library in LIBRARIES {
        let handle = unsafe {
            libc::dlopen(
                library.as_ptr().cast(),
                libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
            )
        };
        if handle.is_null() {
            continue;
        }
        let address = unsafe { lookup_symbol(handle, name.as_ptr().cast()) };
        unsafe {
            libc::dlclose(handle);
        }
        if !address.is_null() {
            return address.cast_const();
        }
    }
    std::ptr::null()
}
pub(crate) unsafe fn original<T: Copy>(name: &[u8]) -> Option<T> {
    let p = unsafe { real_symbol(name) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { mem::transmute_copy(&p) })
    }
}
macro_rules! api {
    ($static:ident,$kind:ty,$name:literal) => {
        static $static: LazyLock<Option<$kind>> =
            LazyLock::new(|| unsafe { original(concat!($name, "\0").as_bytes()) });
    };
}
type GlxSwap = unsafe extern "C" fn(*mut c_void, c_ulong);
type EglSwap = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type Damage = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int, c_int) -> c_uint;
type Proc = unsafe extern "C" fn(*const c_char) -> *const c_void;
type Context = unsafe extern "C" fn() -> *mut c_void;
api!(GX_SWAP, GlxSwap, "glXSwapBuffers");
api!(EG_SWAP, EglSwap, "eglSwapBuffers");
static EG_KHR: LazyLock<Option<Damage>> =
    LazyLock::new(|| unsafe { egl_original(b"eglSwapBuffersWithDamageKHR\0") });
static EG_EXT: LazyLock<Option<Damage>> =
    LazyLock::new(|| unsafe { egl_original(b"eglSwapBuffersWithDamageEXT\0") });
unsafe fn egl_original<T: Copy>(name: &[u8]) -> Option<T> {
    if let Some(f) = unsafe { original(name) } {
        return Some(f);
    }
    let p = EG_PROC.map(|f| unsafe { f(name.as_ptr().cast()) })?;
    if p.is_null() || hooked(name.strip_suffix(b"\0").unwrap_or(name)) == Some(p) {
        None
    } else {
        Some(unsafe { mem::transmute_copy(&p) })
    }
}
api!(GX_PROC, Proc, "glXGetProcAddressARB");
api!(GX_PROC_CORE, Proc, "glXGetProcAddress");
api!(EG_PROC, Proc, "eglGetProcAddress");
api!(GX_CONTEXT, Context, "glXGetCurrentContext");
api!(EG_CONTEXT, Context, "eglGetCurrentContext");
api!(
    GX_DRAWABLE,
    unsafe extern "C" fn() -> c_ulong,
    "glXGetCurrentDrawable"
);
api!(
    EG_DRAWABLE,
    unsafe extern "C" fn(c_int) -> *mut c_void,
    "eglGetCurrentSurface"
);
api!(
    GX_QUERY,
    unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint),
    "glXQueryDrawable"
);
api!(
    EG_QUERY,
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint,
    "eglQuerySurface"
);
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Glx,
    Egl,
    Sdl,
}
struct Surface {
    backend: Backend,
    display: usize,
    drawable: usize,
    native: u64,
    client: Mutex<Client>,
    rendering: Mutex<RenderState>,
    active: AtomicBool,
    composed: AtomicBool,
}
struct RenderState {
    renderers: Vec<Renderer>,
    generation: u64,
}
// Registry/control locks never span graphics calls. SDL input may arrive on a
// different thread while a present is uploading or composing the previous frame.
static SURFACES: LazyLock<Mutex<Vec<Arc<Surface>>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(16)));
unsafe fn context(backend: Backend) -> usize {
    unsafe {
        match backend {
            Backend::Glx => GX_CONTEXT.map(|f| f() as usize).unwrap_or(0),
            Backend::Egl => EG_CONTEXT.map(|f| f() as usize).unwrap_or(0),
            Backend::Sdl => crate::sdl::current_context(),
        }
    }
}
unsafe fn load(backend: Backend, name: &[u8]) -> *const c_void {
    let p = unsafe { real_symbol(name) };
    if !p.is_null() {
        return p;
    }
    unsafe {
        match backend {
            Backend::Glx => GX_PROC
                .or(*GX_PROC_CORE)
                .map(|f| f(name.as_ptr().cast()))
                .unwrap_or(std::ptr::null()),
            Backend::Egl => EG_PROC
                .map(|f| f(name.as_ptr().cast()))
                .unwrap_or(std::ptr::null()),
            Backend::Sdl => crate::sdl::gl_proc(name),
        }
    }
}
fn draw(surface: &Surface, context: usize, w: u32, h: u32, focused: bool) {
    let refresh = if surface.backend == Backend::Sdl {
        crate::sdl::refresh_hz(surface.drawable)
    } else {
        crate::sdl::native_refresh_hz(surface.native)
    };
    let mut rendering = surface.rendering.lock();
    let mut client = surface.client.lock();
    if !surface.active.load(Ordering::Acquire) {
        return;
    }
    client.present(w, h, refresh, focused);
    let generation = client.frame_generation();
    if rendering.generation != generation {
        rendering.generation = generation;
        surface.composed.store(false, Ordering::Release);
        for renderer in &mut rendering.renderers {
            renderer.invalidate();
        }
    }
    if !client.visible() {
        surface.composed.store(false, Ordering::Release);
        for renderer in &mut rendering.renderers {
            renderer.invalidate();
        }
        return;
    }
    let existing = rendering
        .renderers
        .iter()
        .position(|renderer| renderer.matches(context));
    let frame = if existing.is_some_and(|index| rendering.renderers[index].has_frame()) {
        client.latest()
    } else {
        client.replay_latest()
    };
    // ReadFrame owns its mapping/slot lease; control can now receive a resize,
    // disconnect, or editor input without waiting for the game graphics driver.
    drop(client);
    if context == 0 || w == 0 || h == 0 {
        surface.composed.store(false, Ordering::Release);
        return;
    }
    let index = if let Some(index) = existing {
        index
    } else {
        if RETIRED
            .try_lock()
            .is_none_or(|retired| retired.len() >= 256)
            || rendering.renderers.len() == 16
        {
            surface.composed.store(false, Ordering::Release);
            return;
        }
        let Some(renderer) =
            (unsafe { Renderer::new(context, w, h, |n| load(surface.backend, n)) })
        else {
            surface.composed.store(false, Ordering::Release);
            return;
        };
        rendering.renderers.push(renderer);
        rendering.renderers.len() - 1
    };
    if let Some(frame) = frame.as_ref() {
        for (other, renderer) in rendering.renderers.iter_mut().enumerate() {
            if other != index && renderer.sequence() != frame.sequence() {
                renderer.invalidate();
            }
        }
    }
    let renderer = &mut rendering.renderers[index];
    let result = if let Some(frame) = frame {
        let extent = (frame.width(), frame.height());
        if extent.0 == 0
            || extent.1 == 0
            || extent.0 > i32::MAX as u32
            || extent.1 > i32::MAX as u32
        {
            renderer.invalidate();
            surface.composed.store(false, Ordering::Release);
            return;
        }
        unsafe {
            if !renderer.resize_texture_if_needed(extent.0, extent.1) {
                renderer.invalidate();
                surface.composed.store(false, Ordering::Release);
                return;
            }
            renderer.present_sized(Some(frame), w, h)
        }
    } else {
        unsafe { renderer.present_sized(None, w, h) }
    };
    if result.is_none() {
        renderer.invalidate();
    }
    surface.composed.store(
        result.is_some() && renderer.has_frame() && surface.active.load(Ordering::Acquire),
        Ordering::Release,
    );
}
fn process(backend: Backend, display: usize, drawable: usize, known: Option<(u32, u32, bool)>) {
    let (w, h, focused) = if let Some(v) = known {
        v
    } else {
        unsafe {
            match backend {
                Backend::Glx => {
                    let Some(query) = *GX_QUERY else { return };
                    let (mut w, mut h) = (0, 0);
                    query(display as _, drawable as _, 0x801d, &mut w);
                    query(display as _, drawable as _, 0x801e, &mut h);
                    (w, h, native_focused(backend, display, drawable))
                }
                Backend::Egl => {
                    let Some(query) = *EG_QUERY else { return };
                    let (mut w, mut h) = (0, 0);
                    if query(display as _, drawable as _, 0x3057, &mut w) == 0
                        || query(display as _, drawable as _, 0x3056, &mut h) == 0
                    {
                        return;
                    }
                    (
                        w.max(0) as u32,
                        h.max(0) as u32,
                        native_focused(backend, display, drawable),
                    )
                }
                Backend::Sdl => return,
            }
        }
    };
    if w == 0 || h == 0 || w > i32::MAX as u32 || h > i32::MAX as u32 {
        return;
    }
    let ctx = unsafe { context(backend) };
    let current_drawable = unsafe {
        match backend {
            Backend::Glx => GX_DRAWABLE.map(|f| f() as usize).unwrap_or(0),
            Backend::Egl => EG_DRAWABLE.map(|f| f(0x3059) as usize).unwrap_or(0),
            Backend::Sdl => crate::sdl::current_window(),
        }
    };
    if ctx == 0 || current_drawable != drawable {
        return;
    }
    reap_retired(backend, ctx);
    let native = if backend == Backend::Sdl {
        crate::sdl::native_window(drawable).unwrap_or(drawable as u64)
    } else {
        native_identity(backend, display, drawable).0
    };
    let surface = {
        let mut surfaces = SURFACES.lock();
        if let Some(surface) = surfaces
            .iter()
            .find(|s| s.backend == backend && s.display == display && s.drawable == drawable)
        {
            Arc::clone(surface)
        } else {
            if surfaces.len() == 16 {
                return;
            }
            let surface = Arc::new(Surface {
                backend,
                display,
                drawable,
                native,
                client: Mutex::new(Client::new(native)),
                rendering: Mutex::new(RenderState {
                    renderers: Vec::with_capacity(16),
                    generation: 0,
                }),
                active: AtomicBool::new(true),
                composed: AtomicBool::new(false),
            });
            surfaces.push(Arc::clone(&surface));
            surface
        }
    };
    draw(&surface, ctx, w, h, focused);
}
fn window_surface(window: usize) -> Option<Arc<Surface>> {
    let native = crate::sdl::native_window(window);
    SURFACES
        .lock()
        .iter()
        .find(|s| (s.backend == Backend::Sdl && s.drawable == window) || native == Some(s.native))
        .cloned()
}
pub(crate) fn route(window: usize, input: vosulay_frame::BrowserInput) -> Option<bool> {
    let surface = window_surface(window)?;
    let focused = crate::sdl::focused(window);
    let mut client = surface.client.lock();
    if !surface.active.load(Ordering::Acquire) {
        return Some(false);
    }
    client.service();
    client.focus(focused);
    Some(client.route_input(input) && surface.composed.load(Ordering::Acquire))
}
pub(crate) fn capturing(window: usize) -> bool {
    let Some(surface) = window_surface(window) else {
        return false;
    };
    let focused = crate::sdl::focused(window);
    let mut client = surface.client.lock();
    if !surface.active.load(Ordering::Acquire) {
        return false;
    }
    client.service();
    client.focus(focused);
    client.capturing() && surface.composed.load(Ordering::Acquire)
}
pub(crate) fn focus(window: usize, focused: bool) {
    if let Some(surface) = window_surface(window) {
        let mut client = surface.client.lock();
        if surface.active.load(Ordering::Acquire) {
            client.focus(focused);
        }
    }
}
/// The X11 surface this process's X input belongs to. Wine reads input on its
/// top-level window while rendering into an unrelated offscreen client window,
/// so events are matched to the process's surface rather than by window id.
fn x11_surface() -> Option<Arc<Surface>> {
    let candidates: Vec<_> = SURFACES
        .lock()
        .iter()
        .filter(|s| s.backend != Backend::Sdl && s.active.load(Ordering::Acquire))
        .cloned()
        .collect();
    // Client locks are never taken while the registry lock is held.
    candidates
        .iter()
        .find(|s| s.client.lock().visible())
        .or_else(|| candidates.first())
        .cloned()
}
pub(crate) fn route_x11(input: vosulay_frame::BrowserInput) -> bool {
    let Some(surface) = x11_surface() else {
        return false;
    };
    let focused = crate::xfocus::focused(surface.native);
    let mut client = surface.client.lock();
    client.service();
    client.focus(focused);
    client.route_input(input) && surface.composed.load(Ordering::Acquire)
}
pub(crate) fn capturing_x11() -> bool {
    let Some(surface) = x11_surface() else {
        return false;
    };
    let mut client = surface.client.lock();
    client.service();
    client.capturing() && surface.composed.load(Ordering::Acquire)
}
fn take_surfaces(matches: impl Fn(&Surface) -> bool) -> [Option<Arc<Surface>>; 16] {
    let mut removed = std::array::from_fn(|_| None);
    let mut surfaces = SURFACES.lock();
    let (mut index, mut count) = (0, 0);
    while index < surfaces.len() {
        if matches(&surfaces[index]) {
            let surface = surfaces.swap_remove(index);
            surface.active.store(false, Ordering::Release);
            surface.composed.store(false, Ordering::Release);
            removed[count] = Some(surface);
            count += 1;
        } else {
            index += 1;
        }
    }
    removed
}
fn retire_surface(surface: &Surface) {
    surface.client.lock().disconnect();
    let current = unsafe { context(surface.backend) };
    let mut rendering = surface.rendering.lock();
    for mut renderer in rendering.renderers.drain(..) {
        if renderer.matches(current) {
            unsafe {
                renderer.destroy_if_current(current);
            }
        } else {
            RETIRED
                .lock()
                .push((surface.backend, surface.display, renderer));
        }
    }
}
pub(crate) fn remove(backend: Backend, display: usize, drawable: usize) {
    if let Some(mut map) = NATIVES.try_lock() {
        for entry in map.iter_mut() {
            if entry.is_some_and(|v| {
                v.backend == backend && v.display == display && v.drawable == drawable
            }) {
                *entry = None;
            }
        }
    }
    for surface in
        take_surfaces(|s| s.backend == backend && s.display == display && s.drawable == drawable)
            .into_iter()
            .flatten()
    {
        crate::sdl::release_cursor_for_surface(surface.native);
        retire_surface(&surface);
    }
}
pub(crate) fn destroy_context(backend: Backend, ctx: usize) {
    let surfaces: [Option<Arc<Surface>>; 16] = {
        let surfaces = SURFACES.lock();
        std::array::from_fn(|index| surfaces.get(index).cloned())
    };
    let current = unsafe { context(backend) };
    for surface in surfaces
        .into_iter()
        .flatten()
        .filter(|s| backend == Backend::Sdl || s.backend == backend || s.backend == Backend::Sdl)
    {
        let mut rendering = surface.rendering.lock();
        if let Some(index) = rendering.renderers.iter().position(|r| r.matches(ctx)) {
            let mut renderer = rendering.renderers.swap_remove(index);
            surface.composed.store(false, Ordering::Release);
            unsafe {
                renderer.destroy_if_current(current);
            }
            drop(rendering);
            crate::sdl::release_cursor_for_surface(surface.native);
        }
    }
    let mut retired = RETIRED.lock();
    let mut index = 0;
    while index < retired.len() {
        if (backend == Backend::Sdl
            || retired[index].0 == backend
            || retired[index].0 == Backend::Sdl)
            && retired[index].2.matches(ctx)
        {
            let (_, _, mut renderer) = retired.swap_remove(index);
            unsafe {
                renderer.destroy_if_current(current);
            }
        } else {
            index += 1;
        }
    }
}
thread_local! {
    /// Set while OBS's swap wrapper runs on this thread: it captures the clean
    /// frame, then chains to our exported hook, which composites and swaps.
    static IN_OBS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// OBS game capture's wrapper for `name`, when it precedes our hook. Games
/// such as Wine resolve swaps through `dlsym`, which our lookup helper routes
/// to us; without this, OBS would never see the frame.
unsafe fn obs_wrapper<T: Copy>(name: &CStr, ours: *const c_void) -> Option<T> {
    let pointer = unsafe { lookup_symbol(libc::RTLD_DEFAULT, name.as_ptr()) };
    if pointer.is_null() || pointer.cast_const() == ours {
        return None;
    }
    let mut owner: libc::Dl_info = unsafe { mem::zeroed() };
    if unsafe { libc::dladdr(pointer, &mut owner) } == 0 || owner.dli_fname.is_null() {
        return None;
    }
    let path = unsafe { CStr::from_ptr(owner.dli_fname) }.to_bytes();
    let file = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
    file.starts_with(b"libobs_glcapture.so")
        .then(|| unsafe { mem::transmute_copy(&pointer) })
}
/// Runs `capture` (OBS) once per swap unless this call is OBS chaining back.
fn through_obs<T>(obs: Option<T>, capture: impl FnOnce(T)) -> bool {
    let Some(obs) = obs else {
        return false;
    };
    if IN_OBS.get() {
        return false;
    }
    IN_OBS.set(true);
    capture(obs);
    IN_OBS.set(false);
    true
}
static OBS_GX_SWAP: LazyLock<Option<GlxSwap>> =
    LazyLock::new(|| unsafe { obs_wrapper(c"glXSwapBuffers", glXSwapBuffers as *const c_void) });
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXSwapBuffers(d: *mut c_void, w: c_ulong) {
    if through_obs(*OBS_GX_SWAP, |obs| unsafe { obs(d, w) }) {
        return;
    }
    if let Some(f) = *GX_SWAP {
        if let Some(_guard) = Guard::enter() {
            if !draw_sdl_present() {
                process(Backend::Glx, d as usize, w as usize, None);
            }
            unsafe { f(d, w) }
        } else {
            unsafe { f(d, w) }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffers(d: *mut c_void, s: *mut c_void) -> c_uint {
    let mut result = 0;
    if through_obs(*OBS_EG_SWAP, |obs| result = unsafe { obs(d, s) }) {
        return result;
    }
    let Some(f) = *EG_SWAP else { return 0 };
    if let Some(_guard) = Guard::enter() {
        if !draw_sdl_present() {
            process(Backend::Egl, d as usize, s as usize, None);
        }
        unsafe { f(d, s) }
    } else {
        unsafe { f(d, s) }
    }
}
static OBS_EG_SWAP: LazyLock<Option<EglSwap>> =
    LazyLock::new(|| unsafe { obs_wrapper(c"eglSwapBuffers", eglSwapBuffers as *const c_void) });
unsafe fn damage(
    f: Option<Damage>,
    d: *mut c_void,
    s: *mut c_void,
    r: *const c_int,
    n: c_int,
) -> c_uint {
    let Some(f) = f else { return 0 };
    // OBS 1.5.6 hooks eglSwapBuffers, not the damage variants. Its standard
    // wrapper performs the same full-buffer swap, then chains through our
    // native hook, where composition follows capture.
    let mut result = 0;
    if through_obs(*OBS_EG_SWAP, |obs| result = unsafe { obs(d, s) }) {
        return result;
    }
    if let Some(_guard) = Guard::enter() {
        if !draw_sdl_present() {
            process(Backend::Egl, d as usize, s as usize, None); /* overlay affects whole buffer */
        }
        unsafe { f(d, s, std::ptr::null(), 0) }
    } else {
        unsafe { f(d, s, r, n) }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageKHR(
    d: *mut c_void,
    s: *mut c_void,
    r: *const c_int,
    n: c_int,
) -> c_uint {
    unsafe { damage(*EG_KHR, d, s, r, n) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageEXT(
    d: *mut c_void,
    s: *mut c_void,
    r: *const c_int,
    n: c_int,
) -> c_uint {
    unsafe { damage(*EG_EXT, d, s, r, n) }
}
pub(crate) fn hooked(name: &[u8]) -> Option<*const c_void> {
    Some(match name {
        b"glXSwapBuffers" => glXSwapBuffers as _,
        b"eglSwapBuffers" => eglSwapBuffers as _,
        b"eglSwapBuffersWithDamageKHR" => eglSwapBuffersWithDamageKHR as _,
        b"eglSwapBuffersWithDamageEXT" => eglSwapBuffersWithDamageEXT as _,
        b"glXGetProcAddressARB" => glXGetProcAddressARB as _,
        b"glXGetProcAddress" => glXGetProcAddress as _,
        b"eglGetProcAddress" => eglGetProcAddress as _,
        b"glXDestroyContext" => glXDestroyContext as _,
        b"eglDestroyContext" => eglDestroyContext as _,
        b"glXDestroyWindow" => glXDestroyWindow as _,
        b"eglDestroySurface" => eglDestroySurface as _,
        b"glXCreateWindow" => glXCreateWindow as _,
        b"eglGetDisplay" => eglGetDisplay as _,
        b"eglGetPlatformDisplay" => eglGetPlatformDisplay as _,
        b"eglGetPlatformDisplayEXT" => eglGetPlatformDisplayEXT as _,
        b"eglCreateWindowSurface" => eglCreateWindowSurface as _,
        b"eglCreatePlatformWindowSurface" => eglCreatePlatformWindowSurface as _,
        b"eglCreatePlatformWindowSurfaceEXT" => eglCreatePlatformWindowSurfaceEXT as _,
        b"eglTerminate" => eglTerminate as _,
        b"XDestroyWindow" => XDestroyWindow as _,
        _ => return crate::x11input::hooked(name).or_else(|| crate::sdl::hooked(name)),
    })
}
unsafe fn get_proc(name: *const c_char, real: Option<Proc>) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    let provider = real.map(|f| unsafe { f(name) }).unwrap_or(std::ptr::null());
    if provider.is_null() {
        return provider;
    }
    let name_bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    hooked(name_bytes).unwrap_or(provider)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddressARB(n: *const c_char) -> *const c_void {
    unsafe { get_proc(n, *GX_PROC) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddress(n: *const c_char) -> *const c_void {
    unsafe { get_proc(n, *GX_PROC_CORE) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetProcAddress(n: *const c_char) -> *const c_void {
    unsafe { get_proc(n, *EG_PROC) }
}
api!(
    GX_DESTROY_CONTEXT,
    unsafe extern "C" fn(*mut c_void, *mut c_void),
    "glXDestroyContext"
);
api!(
    EG_DESTROY_CONTEXT,
    unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint,
    "eglDestroyContext"
);
api!(
    GX_DESTROY_WINDOW,
    unsafe extern "C" fn(*mut c_void, c_ulong),
    "glXDestroyWindow"
);
api!(EG_DESTROY_SURFACE, EglSwap, "eglDestroySurface");
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXDestroyContext(d: *mut c_void, c: *mut c_void) {
    destroy_context(Backend::Glx, c as usize);
    if let Some(f) = *GX_DESTROY_CONTEXT {
        unsafe { f(d, c) }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglDestroyContext(d: *mut c_void, c: *mut c_void) -> c_uint {
    destroy_context(Backend::Egl, c as usize);
    EG_DESTROY_CONTEXT.map(|f| unsafe { f(d, c) }).unwrap_or(0)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXDestroyWindow(d: *mut c_void, w: c_ulong) {
    remove(Backend::Glx, d as usize, w as usize);
    if let Some(f) = *GX_DESTROY_WINDOW {
        unsafe { f(d, w) }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglDestroySurface(d: *mut c_void, s: *mut c_void) -> c_uint {
    remove(Backend::Egl, d as usize, s as usize);
    EG_DESTROY_SURFACE.map(|f| unsafe { f(d, s) }).unwrap_or(0)
}
static INSTALLED: AtomicBool = AtomicBool::new(false);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vosulay_attach_v1(endpoint: *const c_char) -> c_int {
    if endpoint.is_null() {
        return 0;
    }
    let Ok(endpoint) = (unsafe { CStr::from_ptr(endpoint) }).to_str() else {
        return 0;
    };
    if crate::client::set_endpoint(endpoint).is_err() {
        return 0;
    }
    if INSTALLED.load(Ordering::Acquire) {
        return 1;
    }
    let names: [&[u8]; 43] = [
        b"glXSwapBuffers",
        b"eglSwapBuffers",
        b"eglSwapBuffersWithDamageKHR",
        b"eglSwapBuffersWithDamageEXT",
        b"glXGetProcAddressARB",
        b"glXGetProcAddress",
        b"eglGetProcAddress",
        b"glXDestroyContext",
        b"eglDestroyContext",
        b"glXDestroyWindow",
        b"eglDestroySurface",
        b"SDL_GL_SwapWindow",
        b"SDL_SetEventFilter",
        b"SDL_GetEventFilter",
        b"SDL_PeepEvents",
        b"SDL_DestroyWindow",
        b"SDL_GetKeyboardState",
        b"SDL_GetMouseState",
        b"SDL_GetRelativeMouseState",
        b"SDL_GetGlobalMouseState",
        b"SDL_ShowCursor",
        b"SDL_HideCursor",
        b"SDL_CursorVisible",
        b"SDL_SetRelativeMouseMode",
        b"SDL_GetRelativeMouseMode",
        b"SDL_SetWindowRelativeMouseMode",
        b"SDL_GetWindowRelativeMouseMode",
        b"SDL_GL_GetProcAddress",
        b"SDL_GL_DestroyContext",
        b"SDL_GL_DeleteContext",
        b"SDL_CreateWindow",
        b"glXCreateWindow",
        b"eglGetDisplay",
        b"eglGetPlatformDisplay",
        b"eglGetPlatformDisplayEXT",
        b"eglCreateWindowSurface",
        b"eglCreatePlatformWindowSurface",
        b"eglCreatePlatformWindowSurfaceEXT",
        b"eglTerminate",
        b"XDestroyWindow",
        b"XCheckIfEvent",
        b"XNextEvent",
        b"XDefineCursor",
    ];
    let targets: Vec<_> = names
        .iter()
        .filter_map(|name| {
            hooked(name).map(|replacement| crate::late::Target {
                name,
                replacement,
                is_swap: matches!(
                    *name,
                    b"glXSwapBuffers"
                        | b"eglSwapBuffers"
                        | b"eglSwapBuffersWithDamageKHR"
                        | b"eglSwapBuffersWithDamageEXT"
                        | b"SDL_GL_SwapWindow"
                ),
            })
        })
        .collect();
    if unsafe { crate::late::install(&targets) } {
        INSTALLED.store(true, Ordering::Release);
        1
    } else {
        0
    }
}

#[derive(Clone, Copy)]
struct Native {
    backend: Backend,
    display: usize,
    drawable: usize,
    window: u64,
    xdisplay: usize,
}
static NATIVES: LazyLock<Mutex<[Option<Native>; 64]>> = LazyLock::new(|| Mutex::new([None; 64]));
#[derive(Clone, Copy)]
struct EglDisplay {
    display: usize,
    native: usize,
    platform: u32,
}
static DISPLAYS: LazyLock<Mutex<[Option<EglDisplay>; 16]>> =
    LazyLock::new(|| Mutex::new([None; 16]));
fn register_native(value: Native) {
    if let Some(mut map) = NATIVES.try_lock()
        && let Some(slot) = map.iter_mut().find(|v| {
            v.is_none()
                || v.is_some_and(|v| {
                    v.backend == value.backend
                        && v.display == value.display
                        && v.drawable == value.drawable
                })
        })
    {
        *slot = Some(value);
    }
}
fn register_display(display: usize, native: usize, platform: u32) {
    if display == 0 {
        return;
    }
    if let Some(mut map) = DISPLAYS.try_lock()
        && let Some(slot) = map
            .iter_mut()
            .find(|v| v.is_none() || v.is_some_and(|v| v.display == display))
    {
        *slot = Some(EglDisplay {
            display,
            native,
            platform,
        });
    }
}
fn native_identity(backend: Backend, display: usize, drawable: usize) -> (u64, usize) {
    NATIVES
        .try_lock()
        .and_then(|map| {
            map.iter()
                .flatten()
                .find(|v| v.backend == backend && v.display == display && v.drawable == drawable)
                .map(|v| (v.window, v.xdisplay))
        })
        .unwrap_or((
            drawable as u64,
            if backend == Backend::Glx { display } else { 0 },
        ))
}
fn native_focused(backend: Backend, display: usize, drawable: usize) -> bool {
    let (window, xdisplay) = native_identity(backend, display, drawable);
    if let Some((focused, _)) = crate::sdl::surface_status(window) {
        return focused;
    }
    // Xlib-backed surfaces only; non-SDL Wayland exposes no focus query.
    xdisplay != 0 && crate::xfocus::focused(window)
}
fn register_egl(display: usize, drawable: usize, window: usize) {
    if drawable == 0 {
        return;
    }
    let mapping = DISPLAYS
        .try_lock()
        .and_then(|map| map.iter().flatten().find(|v| v.display == display).copied());
    // Non-SDL Wayland focus has no public EGL query. Do not invent it.
    let xdisplay = mapping
        .filter(|v| v.platform == 0x31d5)
        .map(|v| v.native)
        .unwrap_or(0);
    if xdisplay != 0 {
        register_native(Native {
            backend: Backend::Egl,
            display,
            drawable,
            window: window as u64,
            xdisplay,
        });
    }
}
api!(
    GX_CREATE,
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_ulong, *const c_int) -> c_ulong,
    "glXCreateWindow"
);
api!(
    EG_DISPLAY,
    unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    "eglGetDisplay"
);
type PlatformDisplay = unsafe extern "C" fn(c_uint, *mut c_void, *const isize) -> *mut c_void;
type PlatformDisplayExt = unsafe extern "C" fn(c_uint, *mut c_void, *const c_int) -> *mut c_void;
static EG_PLATFORM: LazyLock<Option<PlatformDisplay>> =
    LazyLock::new(|| unsafe { egl_original(b"eglGetPlatformDisplay\0") });
static EG_PLATFORM_EXT: LazyLock<Option<PlatformDisplayExt>> =
    LazyLock::new(|| unsafe { egl_original(b"eglGetPlatformDisplayEXT\0") });
type WindowSurface =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_int) -> *mut c_void;
type PlatformSurface =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const isize) -> *mut c_void;
api!(EG_WINDOW, WindowSurface, "eglCreateWindowSurface");
static EG_PLATFORM_WINDOW: LazyLock<Option<PlatformSurface>> =
    LazyLock::new(|| unsafe { egl_original(b"eglCreatePlatformWindowSurface\0") });
static EG_PLATFORM_WINDOW_EXT: LazyLock<Option<WindowSurface>> =
    LazyLock::new(|| unsafe { egl_original(b"eglCreatePlatformWindowSurfaceEXT\0") });
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXCreateWindow(
    d: *mut c_void,
    c: *mut c_void,
    w: c_ulong,
    a: *const c_int,
) -> c_ulong {
    let v = GX_CREATE.map(|f| unsafe { f(d, c, w, a) }).unwrap_or(0);
    if v != 0 {
        register_native(Native {
            backend: Backend::Glx,
            display: d as usize,
            drawable: v as usize,
            window: w,
            xdisplay: d as usize,
        });
    }
    v
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetDisplay(d: *mut c_void) -> *mut c_void {
    EG_DISPLAY
        .map(|f| unsafe { f(d) })
        .unwrap_or(std::ptr::null_mut())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetPlatformDisplay(
    p: c_uint,
    d: *mut c_void,
    a: *const isize,
) -> *mut c_void {
    let v = EG_PLATFORM
        .map(|f| unsafe { f(p, d, a) })
        .unwrap_or(std::ptr::null_mut());
    register_display(v as usize, d as usize, p);
    v
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetPlatformDisplayEXT(
    p: c_uint,
    d: *mut c_void,
    a: *const c_int,
) -> *mut c_void {
    let v = EG_PLATFORM_EXT
        .map(|f| unsafe { f(p, d, a) })
        .unwrap_or(std::ptr::null_mut());
    register_display(v as usize, d as usize, p);
    v
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreateWindowSurface(
    d: *mut c_void,
    c: *mut c_void,
    w: *mut c_void,
    a: *const c_int,
) -> *mut c_void {
    let v = EG_WINDOW
        .map(|f| unsafe { f(d, c, w, a) })
        .unwrap_or(std::ptr::null_mut());
    register_egl(d as usize, v as usize, w as usize);
    v
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreatePlatformWindowSurface(
    d: *mut c_void,
    c: *mut c_void,
    w: *mut c_void,
    a: *const isize,
) -> *mut c_void {
    let v = EG_PLATFORM_WINDOW
        .map(|f| unsafe { f(d, c, w, a) })
        .unwrap_or(std::ptr::null_mut());
    let mapping = DISPLAYS.try_lock().and_then(|m| {
        m.iter()
            .flatten()
            .find(|v| v.display == d as usize)
            .copied()
    });
    if mapping.is_some_and(|m| m.platform == 0x31d5) && !w.is_null() {
        register_egl(d as usize, v as usize, unsafe { *w.cast::<c_ulong>() }
            as usize);
    }
    v
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglCreatePlatformWindowSurfaceEXT(
    d: *mut c_void,
    c: *mut c_void,
    w: *mut c_void,
    a: *const c_int,
) -> *mut c_void {
    let v = EG_PLATFORM_WINDOW_EXT
        .map(|f| unsafe { f(d, c, w, a) })
        .unwrap_or(std::ptr::null_mut());
    let mapping = DISPLAYS.try_lock().and_then(|m| {
        m.iter()
            .flatten()
            .find(|v| v.display == d as usize)
            .copied()
    });
    if mapping.is_some_and(|m| m.platform == 0x31d5) && !w.is_null() {
        register_egl(d as usize, v as usize, unsafe { *w.cast::<c_ulong>() }
            as usize);
    }
    v
}
// Keep names from another current context until its next present or destruction.
// At most 256 active renderers; creation pauses at 256 pending retirements.
// Fixed 512 retirement slots also accommodate removing all remaining surfaces.
static RETIRED: LazyLock<Mutex<Vec<(Backend, usize, Renderer)>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(512)));
fn reap_retired(backend: Backend, current: usize) {
    if let Some(mut retired) = RETIRED.try_lock() {
        let mut index = 0;
        while index < retired.len() {
            if (retired[index].0 == backend
                || retired[index].0 == Backend::Sdl
                || backend == Backend::Sdl)
                && retired[index].2.matches(current)
            {
                let (_, _, mut renderer) = retired.swap_remove(index);
                unsafe {
                    renderer.destroy_if_current(current);
                }
            } else {
                index += 1;
            }
        }
    }
}
api!(
    EG_TERMINATE,
    unsafe extern "C" fn(*mut c_void) -> c_uint,
    "eglTerminate"
);
api!(
    X_DESTROY,
    unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int,
    "XDestroyWindow"
);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglTerminate(display: *mut c_void) -> c_uint {
    let current = unsafe { context(Backend::Egl) };
    for surface in take_surfaces(|s| s.backend == Backend::Egl && s.display == display as usize)
        .into_iter()
        .flatten()
    {
        surface.client.lock().disconnect();
        let mut rendering = surface.rendering.lock();
        for renderer in &mut rendering.renderers {
            unsafe {
                renderer.destroy_if_current(current);
            }
        }
    }
    RETIRED.lock().retain_mut(|(backend, owner, renderer)| {
        if *backend == Backend::Egl && *owner == display as usize {
            unsafe {
                renderer.destroy_if_current(current);
            }
            false
        } else {
            true
        }
    });
    {
        let mut map = NATIVES.lock();
        for entry in map.iter_mut() {
            if entry.is_some_and(|v| v.backend == Backend::Egl && v.display == display as usize) {
                *entry = None;
            }
        }
    }
    {
        let mut map = DISPLAYS.lock();
        for entry in map.iter_mut() {
            if entry.is_some_and(|v| v.display == display as usize) {
                *entry = None;
            }
        }
    }
    EG_TERMINATE.map(|f| unsafe { f(display) }).unwrap_or(0)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn XDestroyWindow(display: *mut c_void, window: c_ulong) -> c_int {
    crate::x11input::forget_window(display, window);
    for surface in take_surfaces(|s| s.native == window).into_iter().flatten() {
        retire_surface(&surface);
    }
    {
        let mut map = NATIVES.lock();
        for entry in map.iter_mut() {
            if entry.is_some_and(|v| v.xdisplay == display as usize && v.window == window) {
                *entry = None;
            }
        }
    }
    X_DESTROY
        .map(|f| unsafe { f(display, window) })
        .unwrap_or(0)
}
