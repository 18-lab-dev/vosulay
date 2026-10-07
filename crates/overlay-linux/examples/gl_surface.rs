//! Real surfaceless EGL/ES3 pbuffer target for private compositor pixel proofs.
//! Run with LD_PRELOAD=.../libvosulay_overlay.so VOSULAY_ENDPOINT=...;
//! writes the actual post-swap RGB framebuffer to the supplied PPM path.
#[cfg(target_os = "linux")]
use std::{
    ffi::{c_char, c_void},
    io::Write,
    time::Duration,
};
#[cfg(target_os = "linux")]
type Display = *mut c_void;
#[cfg(target_os = "linux")]
type Surface = *mut c_void;
#[cfg(target_os = "linux")]
type Context = *mut c_void;
#[cfg(target_os = "linux")]
#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetProcAddress(name: *const c_char) -> *const c_void;
    fn eglInitialize(d: Display, major: *mut i32, minor: *mut i32) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        d: Display,
        attrs: *const i32,
        config: *mut *mut c_void,
        count: i32,
        found: *mut i32,
    ) -> u32;
    fn eglCreatePbufferSurface(d: Display, config: *mut c_void, attrs: *const i32) -> Surface;
    fn eglCreateContext(
        d: Display,
        config: *mut c_void,
        share: Context,
        attrs: *const i32,
    ) -> Context;
    fn eglMakeCurrent(d: Display, draw: Surface, read: Surface, context: Context) -> u32;
    fn eglSwapBuffers(d: Display, s: Surface) -> u32;
    fn eglDestroyContext(d: Display, c: Context) -> u32;
    fn eglDestroySurface(d: Display, s: Surface) -> u32;
    fn eglTerminate(d: Display) -> u32;
}
#[cfg(target_os = "linux")]
unsafe fn gl<T: Copy>(name: &[u8]) -> T {
    let p = unsafe { eglGetProcAddress(name.as_ptr().cast()) };
    assert!(!p.is_null(), "GL entry point missing");
    unsafe { std::mem::transmute_copy(&p) }
}
#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "gl-surface.ppm".into());
    let frames = std::env::args()
        .nth(2)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(300);
    unsafe {
        let platform: unsafe extern "C" fn(u32, *mut c_void, *const i32) -> Display =
            gl(b"eglGetPlatformDisplayEXT\0");
        let display = platform(0x31dd, std::ptr::null_mut(), std::ptr::null());
        assert!(!display.is_null());
        let (mut major, mut minor) = (0, 0);
        assert_ne!(eglInitialize(display, &mut major, &mut minor), 0);
        assert_ne!(eglBindAPI(0x30a0), 0);
        let attributes = [
            0x3033, 1, 0x3040, 0x40, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
        ];
        let mut config = std::ptr::null_mut();
        let mut found = 0;
        assert_ne!(
            eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut found),
            0
        );
        assert_eq!(found, 1);
        let surface =
            eglCreatePbufferSurface(display, config, [0x3057, 320, 0x3056, 200, 0x3038].as_ptr());
        assert!(!surface.is_null());
        let context = eglCreateContext(
            display,
            config,
            std::ptr::null_mut(),
            [0x3098, 3, 0x3038].as_ptr(),
        );
        assert!(!context.is_null());
        assert_ne!(eglMakeCurrent(display, surface, surface, context), 0);
        let clear_color: unsafe extern "C" fn(f32, f32, f32, f32) = gl(b"glClearColor\0");
        let clear: unsafe extern "C" fn(u32) = gl(b"glClear\0");
        let read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void) =
            gl(b"glReadPixels\0");
        let finish: unsafe extern "C" fn() = gl(b"glFinish\0");
        let error: unsafe extern "C" fn() -> u32 = gl(b"glGetError\0");
        for _ in 0..frames {
            clear_color(0.08, 0.18, 0.30, 1.0);
            clear(0x4000);
            assert_ne!(eglSwapBuffers(display, surface), 0);
            std::thread::sleep(Duration::from_millis(16));
        }
        finish();
        let mut rgba = vec![0u8; 320 * 200 * 4];
        read(0, 0, 320, 200, 0x1908, 0x1401, rgba.as_mut_ptr().cast());
        assert_eq!(error(), 0, "GL error after composition/readback");
        let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
        write!(file, "P6\n320 200\n255\n")?;
        for row in (0..200).rev() {
            for pixel in rgba[row * 320 * 4..(row + 1) * 320 * 4].chunks_exact(4) {
                file.write_all(&pixel[..3])?;
            }
        }
        file.flush()?;
        assert_ne!(
            eglMakeCurrent(
                display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ),
            0
        );
        assert_ne!(eglDestroyContext(display, context), 0);
        assert_ne!(eglDestroySurface(display, surface), 0);
        assert_ne!(eglTerminate(display), 0);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "gl_surface is a Linux-only EGL compositor example",
    ))
}
