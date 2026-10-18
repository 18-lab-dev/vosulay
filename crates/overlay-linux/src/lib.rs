#![cfg(target_os = "linux")]

// Cargo install removes its build tree, so the product build script retains
// this crate's cdylib before installation instead of using that temporary path.
#[doc(hidden)]
pub const BUILD_OUTPUT_DIRECTORY: &str = env!("OUT_DIR");

mod client;
mod gl;
mod hooks;
mod late;
#[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
mod lookup;
#[cfg(feature = "profile")]
mod profile;
mod sdl;
mod sdl3;
mod vulkan;
mod x11input;
mod xfocus;
