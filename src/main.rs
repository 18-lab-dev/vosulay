fn main() {
    // Linux dispatches CEF subprocesses through this executable. The Windows
    // package instead invokes RunWinMain through CEF's sandbox bootstrap.
    match unsafe { vosulay::runtime::run(std::ptr::null_mut()) } {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Vosulay: {error:#}");
            std::process::exit(1);
        }
    }
}
