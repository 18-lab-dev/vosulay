// Adapted from storycraft/asdf-overlay c925244c8d14ca36d82585834cdfebf13d6ce86b.
// Copyright storycraft and contributors; MIT OR Apache-2.0.
// Vosulay: retain surface ownership without holding registry guards in callbacks.
//! Manage rendering surface states through [`Surfaces::state`].
//!
//! Surface IDs identify graphics surfaces, not windows. Several surfaces may belong
//! to one window, and composition surfaces may have no window at all.

pub mod texture;

use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Context;
use asdf_overlay_event::{Event, SurfaceEvent, SurfaceInfo, SurfaceType};
use dashmap::Entry;
use once_cell::sync::Lazy;

use crate::{
    event_sink::OverlayEventSink, interop::DxInterop, surface::texture::OverlayTextureSlot,
    types::IntDashMap,
};

static SURFACES: Lazy<Surfaces> = Lazy::new(|| Surfaces {
    map: IntDashMap::default(),
});

/// The registry of graphics surfaces available for overlays.
///
/// IDs can become invalid at any time. Callbacks retain owned references, never
/// registry guards: native setup, draws and resource drops can re-enter a window proc.
pub struct Surfaces {
    map: IntDashMap<u64, Arc<SurfaceState>>,
}

impl Surfaces {
    /// Iterate over tracked surface IDs in unspecified order.
    pub fn iter() -> impl Iterator<Item = u64> {
        SURFACES.map.iter().map(|r| *r.key())
    }

    /// Access a surface state, or return [`None`] if the ID is not associated to any surfaces.
    pub fn state<R>(id: u64, f: impl FnOnce(&SurfaceState) -> R) -> Option<R> {
        let state = SURFACES
            .map
            .get(&id)
            .map(|entry| Arc::clone(entry.value()))?;
        Some(f(&state))
    }

    /// Return whether a surface ID is currently tracked.
    pub fn contains(id: u64) -> bool {
        SURFACES.map.contains_key(&id)
    }

    /// Reset all overlay positions and textures while retaining the tracked surfaces.
    pub fn reset() {
        let states: Vec<_> = SURFACES
            .map
            .iter()
            .map(|entry| Arc::clone(entry.value()))
            .collect();
        for state in states {
            state.reset();
        }
    }

    #[doc(hidden)]
    pub fn with<R>(
        id: u64,
        setup_fn: impl FnOnce() -> anyhow::Result<SurfaceState>,
        f: impl FnOnce(&SurfaceState) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let backend = Self::acquire(id, setup_fn)?;
        f(&backend)
    }

    #[doc(hidden)]
    pub fn acquire(
        id: u64,
        setup_fn: impl FnOnce() -> anyhow::Result<SurfaceState>,
    ) -> anyhow::Result<Arc<SurfaceState>> {
        let existing = SURFACES.map.get(&id).map(|entry| Arc::clone(entry.value()));
        if let Some(backend) = existing {
            return Ok(backend);
        }

        // D3D device creation may synchronously dispatch a callback into state().
        // Construct before taking the entry guard, and retain ownership across f().
        let candidate = Arc::new(setup_fn().context("failed to setup surface state")?);
        let (backend, added) = match SURFACES.map.entry(id) {
            Entry::Occupied(entry) => (Arc::clone(entry.get()), false),
            Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&candidate));
                (Arc::clone(&candidate), true)
            }
        };
        if added {
            let (width, height) = backend.size();
            OverlayEventSink::emit(Event::Surface {
                id,
                event: SurfaceEvent::Added {
                    width,
                    height,
                    info: backend.info,
                },
            });
        }
        Ok(backend)
    }

    #[doc(hidden)]
    pub fn cleanup_state(id: u64) -> bool {
        if SURFACES.map.remove(&id).is_none() {
            return false;
        }

        OverlayEventSink::emit(Event::Surface {
            id,
            event: SurfaceEvent::Destroyed,
        });
        true
    }
}

/// Overlay state for a graphics surface.
///
/// Size and position use physical pixels. Coordinate pairs are not atomic snapshots
/// and may contain components from different concurrent updates.
#[non_exhaustive]
pub struct SurfaceState {
    position: (AtomicI32, AtomicI32),
    size: (AtomicU32, AtomicU32),

    pub interop: DxInterop,
    pub info: SurfaceInfo,

    #[doc(hidden)]
    pub texture: OverlayTextureSlot,
}

impl SurfaceState {
    /// Create an unregistered state at `(0, 0)` with no overlay texture.
    pub fn new(interop: DxInterop, size: (u32, u32), api: SurfaceType) -> anyhow::Result<Self> {
        let surface = OverlayTextureSlot::new();
        let info = SurfaceInfo {
            api,
            gpu_id: interop.gpu_id,
            keyed_mutex: interop.keyed_mutex,
        };

        Ok(Self {
            position: (AtomicI32::new(0), AtomicI32::new(0)),
            size: (AtomicU32::new(size.0), AtomicU32::new(size.1)),
            interop,
            info,
            texture: surface,
        })
    }

    #[doc(hidden)]
    pub fn texture_size(&self) -> Option<(u32, u32)> {
        self.texture.get().as_ref().map(|surface| surface.size())
    }

    /// Return the target graphics-surface dimensions.
    pub fn size(&self) -> (u32, u32) {
        (
            self.size.0.load(Ordering::Relaxed),
            self.size.1.load(Ordering::Relaxed),
        )
    }

    #[doc(hidden)]
    pub fn resize(&self, width: u32, height: u32) {
        self.size.0.store(width, Ordering::Relaxed);
        self.size.1.store(height, Ordering::Relaxed);
    }

    /// Return the overlay offset relative to the target surface.
    pub fn position(&self) -> (i32, i32) {
        (
            self.position.0.load(Ordering::Relaxed),
            self.position.1.load(Ordering::Relaxed),
        )
    }

    /// Set the overlay offset relative to the target surface, without clamping.
    pub fn reposition(&self, x: i32, y: i32) {
        self.position.0.store(x, Ordering::Relaxed);
        self.position.1.store(y, Ordering::Relaxed);
    }

    /// Replace the overlay texture, or remove it with [`None`].
    ///
    /// The texture must use this surface's GPU adapter. On success, ownership of an NT
    /// handle transfers to the texture; on failure, the caller retains it and the old
    /// texture remains. Release keyed mutexes at key zero before rendering.
    ///
    /// Success does not wait for presentation.
    pub fn commit_overlay_texture(
        &self,
        handle: Option<SharedTextureHandle>,
    ) -> anyhow::Result<()> {
        self.texture.update(&self.interop.device, handle)
    }

    /// Reset the surface state to its initial state.
    /// This will reset the position to (0, 0) and remove the overlay texture
    pub fn reset(&self) {
        self.reposition(0, 0);
        _ = self.commit_overlay_texture(None);
    }
}

/// A shared D3D11 texture handle.
///
/// This value does not close handles or duplicate them when copied. NT handles must
/// be valid in the receiving process; transferring one to a texture gives that
/// texture ownership. KMT handles require the source resource to remain alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedTextureHandle {
    /// KMT handle.
    Kmt(u32),

    /// Owned NT handle.
    Nt(u32),
}

impl SharedTextureHandle {
    /// Return the raw handle value.
    pub fn as_raw(&self) -> u32 {
        match self {
            Self::Kmt(handle) | Self::Nt(handle) => *handle,
        }
    }
}
