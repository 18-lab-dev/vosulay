use crate::d3d11::TextureLease;
use asdf_overlay_common::event::surface::SurfaceInfo;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub generation: u64,
    pub pid: u32,
    pub surface: u64,
    pub info: SurfaceInfo,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct HandleUpdate {
    pub binding: Binding,
    /// None removes composition without destroying the tracked game surface.
    pub handle: Option<u32>,
    /// Hold the producer texture through the asynchronous OpenSharedResource ACK.
    pub lease: Option<Arc<TextureLease>>,
}

pub(crate) struct GpuMailbox {
    pub binding: Mutex<Option<Binding>>,
    pub update: Mutex<Option<HandleUpdate>>,
    pub visible: AtomicBool,
    pub worker_wakeup: tokio::sync::Notify,
}

impl Default for GpuMailbox {
    fn default() -> Self {
        Self {
            binding: Mutex::new(None),
            update: Mutex::new(None),
            visible: AtomicBool::new(false),
            worker_wakeup: tokio::sync::Notify::new(),
        }
    }
}
