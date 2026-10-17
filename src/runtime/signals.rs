//! Ctrl+C, `kill` and a closed terminal (SIGINT, SIGTERM, SIGHUP) ask vosulay to exit the way
//! the tray's Exit does: edits are flushed, input is released and CEF shuts down. A second signal
//! exits at once, for a shutdown that hangs.
//!
//! CEF installs Chromium's own handlers for these signals when it initializes. They forward the
//! first signal to Chromium's quit path, which never reaches vosulay's externally pumped loop, so
//! the host installs these handlers again after CEF starts (see [`install`]).
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    thread::{self, Thread},
};

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The write end of the pipe that wakes the watcher thread; `-1` until [`install`] runs.
static WAKE: AtomicI32 = AtomicI32::new(-1);
/// Set by the first signal and never cleared: a later one forces the exit.
static RECEIVED: AtomicBool = AtomicBool::new(false);
/// The first signal's exit request, until the host takes it.
static PENDING: AtomicBool = AtomicBool::new(false);

/// Routes the exit signals to vosulay, waking `host` when one arrives. Call it again whenever
/// something may have replaced the handlers (CEF does during initialization); the watcher thread
/// and its pipe are created on the first call. Only the host thread calls this.
pub(super) fn install(host: &Thread) -> io::Result<()> {
    if WAKE.load(Ordering::Acquire) < 0 {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for both pipe ends.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe2 returned two new descriptors that nothing else owns.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let host = host.clone();
        thread::Builder::new()
            .name("vosulay-signals".into())
            .spawn(move || watch(read, host))?;
        // The write end lives for the whole process: the handler may run at any time.
        WAKE.store(write.into_raw_fd(), Ordering::Release);
    }
    for signal in SIGNALS {
        // SAFETY: a zeroed sigaction with an empty mask is valid; `handle` is async-signal-safe.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Whether an exit signal arrived since the last call.
pub(super) fn take_exit_request() -> bool {
    PENDING.swap(false, Ordering::AcqRel)
}

fn watch(read: OwnedFd, host: Thread) {
    let mut byte = 0_u8;
    loop {
        // SAFETY: reads at most one byte into `byte` from a descriptor this thread owns.
        let count = unsafe { libc::read(read.as_raw_fd(), (&raw mut byte).cast(), 1) };
        match count {
            1 => host.unpark(),
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

extern "C" fn handle(signal: libc::c_int) {
    // SAFETY: only async-signal-safe calls follow; errno is restored for the interrupted code.
    unsafe {
        let errno = *libc::__errno_location();
        if RECEIVED.swap(true, Ordering::AcqRel) {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        } else {
            PENDING.store(true, Ordering::Release);
            let byte = 1_u8;
            libc::write(WAKE.load(Ordering::Acquire), (&raw const byte).cast(), 1);
        }
        *libc::__errno_location() = errno;
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    const CHILD: &str = "VOSULAY_SIGNALS_TEST_CHILD";

    /// Runs `scenario` in a fresh copy of this test binary, since the second signal ends the
    /// process it arrives in.
    fn run_child(test: &str, scenario: &str) -> (std::process::ExitStatus, String) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(CHILD, scenario)
            .output()
            .unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn raise(signal: libc::c_int) {
        // SAFETY: delivers a signal to this process.
        unsafe { libc::raise(signal) };
    }

    #[test]
    fn first_signal_requests_exit_and_the_second_ends_the_process() {
        use std::os::unix::process::ExitStatusExt;
        const TEST: &str =
            "runtime::signals::tests::first_signal_requests_exit_and_the_second_ends_the_process";
        match std::env::var(CHILD).as_deref() {
            Ok("chromium") => {
                super::install(&std::thread::current()).unwrap();
                // CEF swaps in a handler of its own while initializing; vosulay installs again.
                extern "C" fn foreign(_: libc::c_int) {}
                // SAFETY: installs a no-op handler, as Chromium's would swallow the signal.
                unsafe {
                    libc::signal(
                        libc::SIGINT,
                        foreign as extern "C" fn(libc::c_int) as libc::sighandler_t,
                    )
                };
                super::install(&std::thread::current()).unwrap();
                assert!(!super::take_exit_request());
                raise(libc::SIGINT);
                // The watcher thread wakes this thread.
                std::thread::park_timeout(std::time::Duration::from_secs(5));
                assert!(super::take_exit_request());
                assert!(!super::take_exit_request(), "a request is taken once");
                println!("survived first signal");
                raise(libc::SIGTERM);
                println!("survived second signal");
            }
            Ok(other) => panic!("unknown scenario {other}"),
            Err(_) => {
                let (status, stdout) = run_child(TEST, "chromium");
                assert!(stdout.contains("survived first signal"), "{stdout}");
                assert!(!stdout.contains("survived second signal"), "{stdout}");
                assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?} {stdout}");
            }
        }
    }
}
