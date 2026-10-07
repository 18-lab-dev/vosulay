mod input;
pub use input::{BrowserInput, InputModifiers, KeyKind, MouseButton, PlatformEvent, Viewport};

use anyhow::Result;

/// Changed physical pixels since the preceding CPU-frame submission.
#[derive(Clone, Copy, Debug)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DamageRect {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    pub fn clipped(self, width: u32, height: u32) -> Option<Self> {
        let right = self.x.saturating_add(self.width).min(width);
        let bottom = self.y.saturating_add(self.height).min(height);
        (self.x < right && self.y < bottom).then_some(Self {
            x: self.x,
            y: self.y,
            width: right.saturating_sub(self.x),
            height: bottom.saturating_sub(self.y),
        })
    }

    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

/// Borrowed until submit_frame returns. The consumer must copy the pixels or
/// shared texture before returning, never retain producer-owned resources.
pub enum BrowserFrame<'a> {
    Bgra {
        pixels: &'a [u8],
        width: u32,
        height: u32,
        /// None requires a full copy; pixels always contain the complete image.
        damage: Option<DamageRect>,
    },
    #[cfg(windows)]
    D3d11 {
        handle: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        source_x: u32,
        source_y: u32,
    },
}

pub trait BrowserFrameBackend {
    fn resize(&mut self, width: u32, height: u32, scale: f32) -> Result<()>;
    fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()>;
    fn set_visible(&mut self, visible: bool) -> Result<()>;
}
