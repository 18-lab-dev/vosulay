#[doc(hidden)]
pub const BUILD_OUTPUT_DIRECTORY: &str = env!("OUT_DIR");

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod linux {
    use libc::{c_char, c_void};
    use std::{
        mem,
        sync::{
            LazyLock,
            atomic::{AtomicPtr, Ordering},
        },
    };

    type Lookup = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
    type Intercept = unsafe extern "C" fn(*const c_char, *mut c_void, *const c_void) -> *mut c_void;
    static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn original_address() -> *mut c_void {
        let cached = ORIGINAL.load(Ordering::Acquire);
        if !cached.is_null() {
            return cached;
        }
        // An unversioned default lookup can find this interceptor itself.
        let libc =
            unsafe { libc::dlopen(c"libc.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        let address = if libc.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { libc::dlvsym(libc, c"dlsym".as_ptr(), c"GLIBC_2.2.5".as_ptr()) }
        };
        if address.is_null() {
            unsafe { libc::abort() };
        }
        ORIGINAL.store(address, Ordering::Release);
        unsafe { libc::dlclose(libc) };
        address
    }

    static CHAIN: LazyLock<(Lookup, Intercept)> = LazyLock::new(|| {
        let lookup: Lookup = unsafe { mem::transmute(original_address()) };
        let next = unsafe { lookup(libc::RTLD_NEXT, c"dlsym".as_ptr()) };
        let intercept = unsafe { lookup(libc::RTLD_DEFAULT, c"vosulay_lookup_v1".as_ptr()) };
        if next.is_null() || intercept.is_null() {
            unsafe { libc::abort() };
        }
        unsafe {
            (
                mem::transmute::<*mut c_void, Lookup>(next),
                mem::transmute::<*mut c_void, Intercept>(intercept),
            )
        }
    });

    unsafe extern "C" fn explicit_lookup(
        handle: *mut c_void,
        name: *const c_char,
        caller: *const c_void,
    ) -> *mut c_void {
        // Putting the compositor before OBS would draw into its capture. Only
        // lookup runs first: foreign GL wrappers stay ahead of our native swap.
        let (lookup, intercept) = *CHAIN;
        let address = unsafe { lookup(handle, name) };
        unsafe { intercept(name, address, caller) }
    }

    /// # Safety
    /// `handle` and `name` must satisfy glibc's dlsym contract.
    /// Pseudo-handles need the original caller's return address, not a Rust
    /// forwarding call whose scope excludes the caller's RTLD_LOCAL group.
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
        core::arch::naked_asm!(
            "test rdi, rdi",
            "jz 2f",
            "cmp rdi, -1",
            "je 2f",
            "mov rdx, [rsp]",
            "jmp {explicit}",
            "2:",
            "push rdi",
            "push rsi",
            "sub rsp, 8",
            "call {original}",
            "add rsp, 8",
            "pop rsi",
            "pop rdi",
            "jmp rax",
            original = sym original_address,
            explicit = sym explicit_lookup,
        );
    }
}
