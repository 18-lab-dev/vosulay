//! Explicit-handle lookups used by .NET/SDL bypass ordinary ELF interposition.
//! Keep pseudo-handle lookups in the original caller's scope, not this library's.
use libc::{c_char, c_void};
use std::{
    ffi::CStr,
    mem,
    sync::atomic::{AtomicPtr, Ordering},
};

type Lookup = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static SDL: [AtomicPtr<c_void>; 2] = [const { AtomicPtr::new(std::ptr::null_mut()) }; 2];

extern "C" fn original_address() -> *mut c_void {
    let cached = ORIGINAL.load(Ordering::Acquire);
    if !cached.is_null() {
        return cached;
    }
    // A late-injected library can follow libc in the link map. Use its explicit
    // handle; RTLD_NEXT is not sufficient here and an unversioned default lookup
    // could find our own export. dlvsym does not recurse through dlsym.
    let libc = unsafe { libc::dlopen(c"libc.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
    let address = if libc.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { libc::dlvsym(libc, c"dlsym".as_ptr(), c"GLIBC_2.2.5".as_ptr()) }
    };
    if address.is_null() {
        // This symbol is part of the supported Linux x86_64 glibc ABI.
        unsafe { libc::abort() };
    }
    ORIGINAL.store(address, Ordering::Release);
    unsafe { libc::dlclose(libc) };
    address
}

pub(crate) unsafe fn resolve(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let lookup: Lookup = unsafe { mem::transmute(original_address()) };
    unsafe { lookup(handle, name) }
}

pub(crate) unsafe fn sdl_symbol(name: *const c_char) -> *mut c_void {
    for provider in &SDL {
        let handle = provider.load(Ordering::Acquire);
        if !handle.is_null() {
            let address = unsafe { resolve(handle, name) };
            if !address.is_null() {
                return address;
            }
        }
    }
    std::ptr::null_mut()
}

unsafe fn retain_sdl(index: usize, path: *const c_char) {
    if !SDL[index].load(Ordering::Acquire).is_null() {
        return;
    }
    // SDL can be RTLD_LOCAL under its bundled absolute filename. Retain one
    // reference so the cached original API remains valid throughout injection.
    let handle = unsafe { libc::dlopen(path, libc::RTLD_NOW | libc::RTLD_NOLOAD) };
    if !handle.is_null()
        && SDL[index]
            .compare_exchange(
                std::ptr::null_mut(),
                handle,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        unsafe { libc::dlclose(handle) };
    }
}

unsafe extern "C" fn explicit_lookup(
    handle: *mut c_void,
    name: *const c_char,
    lookup: Lookup,
    caller: *const c_void,
) -> *mut c_void {
    let address = unsafe { lookup(handle, name) };
    if address.is_null() || name.is_null() {
        return address;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    let Some(replacement) = crate::hooks::hooked(name) else {
        return address;
    };
    let mut caller_info: libc::Dl_info = unsafe { mem::zeroed() };
    if !caller.is_null()
        && unsafe { libc::dladdr(caller, &mut caller_info) } != 0
        && !caller_info.dli_fname.is_null()
    {
        let caller_path = unsafe { CStr::from_ptr(caller_info.dli_fname) }.to_bytes();
        let caller_file = caller_path
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or(caller_path);
        if (caller_file.starts_with(b"libSDL2.so") || caller_file.starts_with(b"libSDL2-2.0.so"))
            && name.starts_with(b"SDL_")
        {
            return address;
        }
    }
    let mut provider: libc::Dl_info = unsafe { mem::zeroed() };
    if unsafe { libc::dladdr(address, &mut provider) } == 0 || provider.dli_fname.is_null() {
        return address;
    }
    let path = unsafe { CStr::from_ptr(provider.dli_fname) }.to_bytes();
    let file = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
    let sdl = if file.starts_with(b"libSDL2.so") || file.starts_with(b"libSDL2-2.0.so") {
        Some(0)
    } else if file.starts_with(b"libSDL3.so") {
        Some(1)
    } else {
        None
    };
    let graphics = (name.starts_with(b"glX")
        && (file.starts_with(b"libGL.so") || file.starts_with(b"libGLX")))
        || (name.starts_with(b"egl") && file.starts_with(b"libEGL"))
        || (name.starts_with(b"X") && file.starts_with(b"libX11.so"));
    if let Some(index) = sdl {
        if !name.starts_with(b"SDL_") {
            return address;
        }
        unsafe { retain_sdl(index, provider.dli_fname) };
        if SDL[index].load(Ordering::Acquire).is_null() {
            return address;
        }
    } else if !graphics {
        return address;
    }
    replacement.cast_mut()
}

/// The tail jump preserves the caller return address required by glibc for
/// RTLD_NEXT and RTLD_DEFAULT. A normal Rust/C forwarding call changes scope.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    core::arch::naked_asm!(
        "push rdi",
        "push rsi",
        "sub rsp, 8",
        "call {original}",
        "add rsp, 8",
        "pop rsi",
        "pop rdi",
        "test rdi, rdi",
        "jz 2f",
        "cmp rdi, -1",
        "je 2f",
        "mov rdx, rax",
        "mov rcx, [rsp]",
        "jmp {explicit}",
        "2:",
        "jmp rax",
        original = sym original_address,
        explicit = sym explicit_lookup,
    );
}
