use std::path::PathBuf;

#[cfg(windows)]
mod d3d11;
#[cfg(windows)]
mod foreground;
#[cfg(windows)]
mod gpu_mailbox;
#[cfg(windows)]
mod platform;
#[cfg(any(windows, test))]
mod target_selection;
#[cfg(windows)]
mod tray;
#[cfg(any(windows, test))]
mod worker_lifecycle;
#[cfg(windows)]
pub use platform::WindowsController;

#[derive(Clone, Debug)]
pub struct WindowsOptions {
    pub dll_dir: PathBuf,
    pub pids: Vec<u32>,
    pub process_names: Vec<String>,
}
