fn main() -> anyhow::Result<()> {
    // Linux's main thread uses the platform sandbox and needs no Windows context.
    unsafe { vosulay_cef_smoke::run(std::ptr::null_mut()) }
}
