#![cfg(target_os = "linux")]

mod backend;
mod ipc;
pub mod protocol;
mod ring;

pub use backend::{FrameBackend, FrameMailbox};
pub use ipc::{Connection, Listener, Received, default_endpoint};
pub use ring::{Consumer, Producer, ReadFrame, RingHandle};
