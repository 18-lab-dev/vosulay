pub mod hotkey;
mod input;
pub use input::{BrowserInput, InputModifiers, KeyKind, MouseButton, PlatformEvent, Viewport};

use anyhow::Result;

/// Changed physical pixels since the preceding CPU-frame submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl DamageRect {
    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
    fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && u64::from(self.x) + u64::from(self.width)
                >= u64::from(other.x) + u64::from(other.width)
            && u64::from(self.y) + u64::from(self.height)
                >= u64::from(other.y) + u64::from(other.height)
    }
}

/// Most separate rectangles one damage set keeps; more merge pairwise.
pub const MAX_DAMAGE_RECTS: usize = 8;

/// Changed regions as up to [`MAX_DAMAGE_RECTS`] rectangles. Counters spread
/// over the screen change in small separate places, and their single
/// bounding box would cover most of the frame. Adding a rectangle beyond the
/// limit merges the pair whose bounding box adds the least area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRects {
    rects: [DamageRect; MAX_DAMAGE_RECTS],
    len: usize,
}

impl Default for DamageRects {
    fn default() -> Self {
        Self {
            rects: [DamageRect::full(0, 0); MAX_DAMAGE_RECTS],
            len: 0,
        }
    }
}

impl From<DamageRect> for DamageRects {
    fn from(rect: DamageRect) -> Self {
        let mut rects = Self::default();
        rects.push(rect);
        rects
    }
}

impl DamageRects {
    pub fn full(width: u32, height: u32) -> Self {
        DamageRect::full(width, height).into()
    }

    pub fn as_slice(&self) -> &[DamageRect] {
        &self.rects[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Adds `rect`; empty ones and ones inside an existing rectangle add
    /// nothing, and existing rectangles inside it are absorbed.
    pub fn push(&mut self, rect: DamageRect) {
        if rect.width == 0 || rect.height == 0 || self.as_slice().iter().any(|r| r.contains(rect)) {
            return;
        }
        let mut kept = 0;
        for index in 0..self.len {
            if !rect.contains(self.rects[index]) {
                self.rects[kept] = self.rects[index];
                kept += 1;
            }
        }
        self.len = kept;
        if self.len < MAX_DAMAGE_RECTS {
            self.rects[self.len] = rect;
            self.len += 1;
            return;
        }
        // Full: merge the cheapest pair among the existing rectangles and
        // the new one, by the area its bounding box adds.
        let mut all = [rect; MAX_DAMAGE_RECTS + 1];
        all[..MAX_DAMAGE_RECTS].copy_from_slice(&self.rects);
        let mut best = (0, 1, u64::MAX);
        for first in 0..all.len() {
            for second in first + 1..all.len() {
                let cost = all[first]
                    .union(all[second])
                    .area()
                    .saturating_sub(all[first].area() + all[second].area());
                if cost < best.2 {
                    best = (first, second, cost);
                }
            }
        }
        let merged = all[best.0].union(all[best.1]);
        let mut next = Self::default();
        for (index, &existing) in all.iter().enumerate() {
            if index != best.0 && index != best.1 {
                next.rects[next.len] = existing;
                next.len += 1;
            }
        }
        *self = next;
        // The merged box may now contain others; push absorbs them.
        self.push(merged);
    }

    pub fn extend(&mut self, other: &Self) {
        for &rect in other.as_slice() {
            self.push(rect);
        }
    }

    /// The bounding box of every rectangle, or None when empty.
    pub fn bounds(&self) -> Option<DamageRect> {
        self.as_slice().iter().copied().reduce(DamageRect::union)
    }

    /// Clips every rectangle to the frame; None when nothing remains.
    pub fn clipped(&self, width: u32, height: u32) -> Option<Self> {
        let mut clipped = Self::default();
        for rect in self.as_slice() {
            if let Some(rect) = rect.clipped(width, height) {
                clipped.push(rect);
            }
        }
        (!clipped.is_empty()).then_some(clipped)
    }

    /// Pixels covered, counting overlaps twice.
    pub fn pixels(&self) -> u64 {
        self.as_slice().iter().map(|rect| rect.area()).sum()
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
        damage: Option<DamageRects>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> DamageRect {
        DamageRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn contained_and_empty_rectangles_add_nothing_and_containers_absorb() {
        let mut rects = DamageRects::from(rect(10, 10, 5, 5));
        rects.push(rect(11, 11, 2, 2));
        rects.push(rect(50, 50, 0, 4));
        assert_eq!(rects.as_slice(), &[rect(10, 10, 5, 5)]);
        rects.push(rect(100, 0, 1, 1));
        rects.push(rect(0, 0, 20, 20));
        assert_eq!(rects.as_slice(), &[rect(100, 0, 1, 1), rect(0, 0, 20, 20)]);
    }

    #[test]
    fn beyond_the_limit_the_cheapest_pair_merges_and_distant_rects_stay() {
        let mut rects = DamageRects::default();
        for index in 0..MAX_DAMAGE_RECTS as u32 {
            rects.push(rect(index * 100, 0, 10, 10));
        }
        // Next to the rectangle at x=300: merging those two costs nothing
        // (their bounding box is exactly both), any other pair costs more.
        rects.push(rect(310, 0, 10, 10));
        assert_eq!(rects.as_slice().len(), MAX_DAMAGE_RECTS);
        assert!(rects.as_slice().contains(&rect(300, 0, 20, 10)));
        assert!(rects.as_slice().contains(&rect(0, 0, 10, 10)));
        assert!(rects.as_slice().contains(&rect(700, 0, 10, 10)));
        assert_eq!(rects.pixels(), (MAX_DAMAGE_RECTS as u64 + 1) * 100);
    }

    #[test]
    fn clipping_keeps_the_visible_parts_and_rejects_nothing_visible() {
        let mut rects = DamageRects::from(rect(90, 90, 20, 20));
        rects.push(rect(200, 0, 5, 5));
        rects.push(rect(0, 0, 4, 4));
        let clipped = rects.clipped(100, 100).unwrap();
        assert_eq!(
            clipped.as_slice(),
            &[rect(90, 90, 10, 10), rect(0, 0, 4, 4)]
        );
        assert_eq!(
            DamageRects::from(rect(200, 200, 5, 5)).clipped(100, 100),
            None
        );
        assert_eq!(clipped.bounds(), Some(rect(0, 0, 100, 100)));
    }
}
