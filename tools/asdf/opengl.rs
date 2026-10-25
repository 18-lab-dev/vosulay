// Adapted from storycraft/asdf-overlay c925244c8d14ca36d82585834cdfebf13d6ce86b.
// Copyright storycraft and contributors; MIT OR Apache-2.0.
// Vosulay: context-owned renderers and lock-free native callback boundaries.
mod context;
mod data;
mod proc;
mod util;

use core::{
    ffi::{CStr, c_void},
    mem,
};
use std::{
    collections::HashSet,
    ffi::CString,
    sync::{Arc, Mutex},
};

use anyhow::{Context, bail};
use asdf_overlay_event::SurfaceType;
use asdf_overlay_hook::DetourHook;
use once_cell::sync::{Lazy, OnceCell};
use tracing::{Level, debug, error, info, trace, warn};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LUID},
        Graphics::{
            Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1},
            Gdi::{HDC, WindowFromDC},
            OpenGL::{
                HGLRC, wglGetCurrentContext, wglGetCurrentDC, wglGetProcAddress, wglMakeCurrent,
            },
        },
        System::LibraryLoader::{GetModuleHandleA, GetProcAddress},
    },
    core::{BOOL, PCSTR, s},
};

use crate::{
    event_sink::OverlayEventSink,
    gl,
    hook::opengl::{data::with_renderer_gl_data, util::get_client_size},
    interop::DxInterop,
    renderer::opengl::OpenglRenderer,
    surface::{SurfaceState, Surfaces},
    types::IntDashMap,
    util::find_adapter_by_luid,
    wgl,
};

struct Hook {
    wgl_delete_context: DetourHook<WglDeleteContextFn>,
    wgl_swap_buffers: DetourHook<WglSwapBuffersFn>,
}

static HOOK: OnceCell<Hook> = OnceCell::new();

struct GlData {
    hdc: usize,
    hwnd: u32,
    extensions: HashSet<String>,
    renderer: Mutex<Option<OpenglRenderer>>,
}

impl Drop for GlData {
    fn drop(&mut self) {
        // The game may delete an inactive context whose DC has already disappeared.
        // Its GL objects die with that context. Never run their GL destructors on
        // whichever unrelated context happens to release the last registry reference.
        if let Some(renderer) = self
            .renderer
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            mem::forget(renderer);
        }
    }
}

// HGLRC -> context-owned renderer. A startup HDC can outlive several contexts.
static MAP: Lazy<IntDashMap<usize, Arc<GlData>>> = Lazy::new(IntDashMap::default);
static GL_INIT: Mutex<()> = Mutex::new(());

#[tracing::instrument(level = Level::DEBUG)]
pub fn hook(dummy_hwnd: HWND) {
    fn inner() -> anyhow::Result<()> {
        let addrs = get_wgl_addrs().context("failed to load opengl addrs")?;

        HOOK.get_or_try_init(|| unsafe {
            debug!("hooking WglDeleteContext");
            let wgl_delete_context =
                DetourHook::attach(addrs.delete_context, hooked_wgl_delete_context as _)?;

            debug!("hooking WglSwapBuffers");
            let wgl_swap_buffers =
                DetourHook::attach(addrs.swap_buffers, hooked_wgl_swap_buffers as _)?;

            Ok::<_, anyhow::Error>(Hook {
                wgl_delete_context,
                wgl_swap_buffers,
            })
        })?;

        Ok(())
    }

    if let Err(err) = inner() {
        error!("failed to hook opengl. err: {err:?}");
    }
}

fn current_context() -> (usize, usize) {
    unsafe {
        (
            wglGetCurrentDC().0 as usize,
            wglGetCurrentContext().0 as usize,
        )
    }
}

fn bind_context((hdc, hglrc): (usize, usize)) -> bool {
    unsafe { wglMakeCurrent(HDC(hdc as _), HGLRC(hglrc as _)).is_ok() }
}

#[tracing::instrument(level = Level::DEBUG)]
extern "system" fn hooked_wgl_delete_context(hglrc: HGLRC) -> BOOL {
    let key = hglrc.0 as usize;
    // Clone and release the registry before calling the driver or window procedures.
    let data = MAP.get(&key).map(|entry| Arc::clone(entry.value()));
    if let Some(data) = data.as_ref() {
        match data.renderer.try_lock() {
            Ok(mut renderer) => {
                let cleanup = context::with_owner_context(
                    (data.hdc, key),
                    current_context,
                    bind_context,
                    || drop(renderer.take()),
                );
                if let Err(error) = cleanup {
                    warn!("OpenGL context {key:#x} cleanup bind failed: {error:?}");
                }
            }
            Err(_) => debug!("OpenGL context {key:#x} is busy during deletion"),
        }
    }
    let result = unsafe { HOOK.wait().wgl_delete_context.original_fn()(hglrc) };
    debug!(
        "OpenGL delete context {key:#x}: success={}",
        result.as_bool()
    );
    if result.as_bool() {
        MAP.remove(&key);
        Surfaces::cleanup_state(key as _);
    }
    result
}

fn draw_overlay(hdc: HDC) {
    let (current_dc, key) = current_context();
    if key == 0 || current_dc != hdc.0 as usize {
        // A startup swap can happen before a rendering context is current.
        return;
    }
    if !OverlayEventSink::connected() {
        // Hooks remain installed for reconnection. After disconnect this is only
        // cleanup in the owning context, followed by the original present path.
        if let Some(data) = MAP.get(&key).map(|entry| Arc::clone(entry.value())) {
            let cleaned = if let Ok(mut renderer) = data.renderer.try_lock() {
                if renderer.is_some() {
                    with_renderer_gl_data(|| drop(renderer.take()));
                }
                true
            } else {
                false
            };
            if cleaned {
                MAP.remove(&key);
                Surfaces::cleanup_state(key as _);
            }
        }
        return;
    }

    let data = match context::get_or_create(
        || MAP.get(&key).map(|entry| Arc::clone(entry.value())),
        || setup_gl_data(hdc).map(Arc::new),
        |data| Arc::clone(MAP.entry(key).or_insert(data).value()),
    ) {
        Ok(data) => data,
        Err(error) => {
            error!("Failed to setup OpenGL context {key:#x}: {error:?}");
            return;
        }
    };
    // Never block a recursive native callback on a renderer already in use.
    let Ok(mut slot) = data.renderer.try_lock() else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        // Acquire retains the state, not a registry guard. Both construction and
        // GL drawing may synchronously re-enter the window procedure.
        let state = Surfaces::acquire(key as _, || {
            // A retained bootstrap context must not remain the host's selected
            // surface after this same DC starts presenting a replacement context.
            // Release the iterator guard before destroying D3D surface resources.
            loop {
                let stale = MAP
                    .iter()
                    .find(|entry| {
                        *entry.key() != key
                            && entry.hdc == current_dc
                            && Surfaces::contains(*entry.key() as _)
                    })
                    .map(|entry| *entry.key());
                let Some(stale) = stale else { break };
                debug!("OpenGL DC {current_dc:#x} changed context {stale:#x} -> {key:#x}");
                Surfaces::cleanup_state(stale as _);
            }
            if slot.is_some() {
                with_renderer_gl_data(|| drop(slot.take()));
            }
            setup_fn(hdc)
        })?;
        with_renderer_gl_data(|| {
            let renderer = match slot.as_mut() {
                Some(renderer) => renderer,
                None => {
                    info!(
                        "Initializing OpenGL renderer context={key:#x} dc={:#x}",
                        data.hdc
                    );
                    slot.insert(OpenglRenderer::new().context("renderer initialization")?)
                }
            };
            let Some(surface_size) = state.texture_size() else {
                return Ok(());
            };
            if state.texture.take_update(&mut renderer.texture_generation) {
                renderer
                    .update_texture(
                        &state.interop.device,
                        state.texture.get().as_ref(),
                        &data.extensions,
                    )
                    .context("renderer texture update")?;
            }
            renderer
                .draw(state.position(), surface_size, state.size())
                .context("renderer draw")
        })
    })();
    if let Err(error) = result {
        error!("Failed to draw OpenGL overlay context={key:#x}: {error:?}");
    }
}

fn setup_fn(hdc: HDC) -> anyhow::Result<SurfaceState> {
    let hwnd = unsafe { WindowFromDC(hdc) };
    let size = get_client_size(hwnd).unwrap_or_default();
    let interop = DxInterop::new(get_dxgi_adapter().as_ref())?;

    SurfaceState::new(
        interop,
        size,
        SurfaceType::Opengl {
            window_id: hwnd.0 as _,
        },
    )
}

fn setup_gl_data(hdc: HDC) -> anyhow::Result<GlData> {
    {
        let _init = GL_INIT.try_lock().map_err(|_| {
            anyhow::anyhow!("OpenGL bindings initialization is already in progress")
        })?;
        if !gl::GetIntegerv::is_loaded() {
            debug!("Setting up opengl");
            setup_gl().context("Opengl setup")?;
        }
    }

    if hdc.is_invalid() {
        bail!("invalid hdc");
    }

    let hwnd = unsafe { WindowFromDC(hdc) };
    if hwnd.is_invalid() {
        bail!("invalid hwnd");
    }

    debug!(
        "OpenGL first swap context={:#x} dc={:#x} window={:#x}",
        unsafe { wglGetCurrentContext() }.0 as usize,
        hdc.0 as usize,
        hwnd.0 as usize
    );
    proc::install(hwnd);

    let mut extensions = HashSet::new();

    // Load gl extensions
    load_gl_extensions(&mut extensions);

    // Load wgl extensions
    if wgl::GetExtensionsStringARB::is_loaded() {
        load_wgl_extensions(&mut extensions);
    }

    Ok(GlData {
        hdc: hdc.0 as usize,
        hwnd: hwnd.0 as u32,
        renderer: Mutex::new(None),
        extensions,
    })
}

fn load_gl_extensions(set: &mut HashSet<String>) {
    let mut len = 0;

    unsafe {
        gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut len);
        set.reserve(len as _);

        for i in 0..len {
            let ext = gl::GetStringi(gl::EXTENSIONS, i as _);
            if ext.is_null() {
                continue;
            }

            let Ok(str) = CStr::from_ptr(ext as _).to_str() else {
                continue;
            };

            set.insert(str.to_string());
        }
    }
}

fn load_wgl_extensions(set: &mut HashSet<String>) {
    let ext = unsafe { wgl::GetExtensionsStringARB(wgl::GetCurrentDC()) };
    if ext.is_null() {
        return;
    }

    let Ok(str) = unsafe { CStr::from_ptr(ext as _) }.to_str() else {
        return;
    };

    for ext in str.split_whitespace() {
        set.insert(ext.to_string());
    }
}

#[tracing::instrument(level = Level::TRACE)]
extern "system" fn hooked_wgl_swap_buffers(hdc: HDC) -> BOOL {
    trace!("WglSwapBuffers called");

    context::present(
        || draw_overlay(hdc),
        || unsafe { HOOK.wait().wgl_swap_buffers.original_fn()(hdc) },
    )
}

type WglSwapBuffersFn = unsafe extern "system" fn(HDC) -> BOOL;
type WglDeleteContextFn = unsafe extern "system" fn(HGLRC) -> BOOL;

struct WglAddrs {
    delete_context: WglDeleteContextFn,
    swap_buffers: WglSwapBuffersFn,
}

#[tracing::instrument(level = Level::TRACE)]
fn get_wgl_addrs() -> anyhow::Result<WglAddrs> {
    // Grab a handle to opengl32.dll
    let opengl32module = unsafe { GetModuleHandleA(s!("opengl32.dll"))? };

    let func = unsafe {
        GetProcAddress(opengl32module, s!("wglDeleteContext"))
            .context("wglDeleteContext not found")?
    };
    debug!("wglDeleteContext found: {:p}", func);
    let delete_context =
        unsafe { mem::transmute::<unsafe extern "system" fn() -> isize, WglDeleteContextFn>(func) };

    let func = unsafe {
        GetProcAddress(opengl32module, s!("wglSwapBuffers")).context("wglSwapBuffers not found")?
    };
    debug!("WglSwapBuffers found: {:p}", func);
    let swap_buffers =
        unsafe { mem::transmute::<unsafe extern "system" fn() -> isize, WglSwapBuffersFn>(func) };

    Ok(WglAddrs {
        delete_context,
        swap_buffers,
    })
}

#[tracing::instrument(level = Level::TRACE)]
fn setup_gl() -> anyhow::Result<()> {
    let opengl32module = unsafe { GetModuleHandleA(s!("opengl32.dll"))? };

    #[tracing::instrument(level = Level::TRACE)]
    fn loader(module: HMODULE, s: &str) -> *const c_void {
        let name = CString::new(s).unwrap();

        let addr = unsafe {
            let addr = PCSTR(name.as_ptr() as _);
            let fn_ptr = wglGetProcAddress(addr);
            if let Some(ptr) = fn_ptr {
                ptr as _
            } else {
                GetProcAddress(module, addr).map_or(std::ptr::null(), |fn_ptr| fn_ptr as *const _)
            }
        };
        trace!("found: {:p}", addr);

        addr
    }

    wgl::load_with(|s| loader(opengl32module, s));
    gl::load_with(|s| loader(opengl32module, s));

    Ok(())
}

fn get_dxgi_adapter() -> Option<IDXGIAdapter> {
    if !gl::GetUnsignedBytevEXT::is_loaded() {
        return None;
    }

    let mut luid = LUID::default();
    unsafe {
        _ = gl::GetError();
        gl::GetUnsignedBytevEXT(gl::DEVICE_LUID_EXT, &mut luid as *mut _ as _);
        if gl::GetError() != gl::NO_ERROR {
            return None;
        }
    }

    let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>().ok()? };
    find_adapter_by_luid(&factory, luid)
}
