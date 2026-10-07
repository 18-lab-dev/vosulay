use crate::{Producer, RingHandle};
use anyhow::{Result, anyhow, ensure};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use vosulay_frame::{BrowserFrame, BrowserFrameBackend};

/// One newest ring generation for asynchronous game attachment, never a queue.
#[derive(Default)]
pub struct FrameMailbox {
    ring: Mutex<Option<(u64, RingHandle)>>,
    visible: AtomicBool,
}
impl FrameMailbox {
    pub fn snapshot(&self) -> Result<Option<(u64, RingHandle)>> {
        Ok(self
            .ring
            .lock()
            .map_err(|_| anyhow!("frame mailbox poisoned"))?
            .clone())
    }
    pub fn visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }
}

pub struct FrameBackend {
    mailbox: Arc<FrameMailbox>,
    producer: Option<Producer>,
    extent: Option<(u32, u32)>,
    generation: u64,
}
impl FrameBackend {
    pub fn new(mailbox: Arc<FrameMailbox>) -> Self {
        Self {
            mailbox,
            producer: None,
            extent: None,
            generation: 0,
        }
    }
}
impl BrowserFrameBackend for FrameBackend {
    fn resize(&mut self, width: u32, height: u32, scale: f32) -> Result<()> {
        ensure!(scale.is_finite() && scale > 0.0, "invalid browser scale");
        if self.extent == Some((width, height)) {
            return Ok(());
        }
        let producer = Producer::new(width, height)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("frame ring generation exhausted"))?;
        let handle = producer.handle();
        // Allocate/map before locking. The controller only clones the descriptor
        // owner under this lock; it never performs IPC or waits for the game here.
        *self
            .mailbox
            .ring
            .lock()
            .map_err(|_| anyhow!("frame mailbox poisoned"))? = Some((generation, handle));
        self.producer = Some(producer);
        self.extent = Some((width, height));
        self.generation = generation;
        Ok(())
    }
    fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()> {
        let BrowserFrame::Bgra {
            pixels,
            width,
            height,
            damage,
        } = frame;
        if self.extent != Some((width, height)) {
            self.resize(width, height, 1.0)?;
        }
        // Backpressure deliberately drops this frame. No producer-owned pointer
        // survives the callback, and no wait for a reader is possible.
        let _ = self
            .producer
            .as_mut()
            .expect("resize creates producer")
            .publish(pixels, damage)?;
        Ok(())
    }
    fn set_visible(&mut self, visible: bool) -> Result<()> {
        self.mailbox.visible.store(visible, Ordering::Release);
        Ok(())
    }
}
