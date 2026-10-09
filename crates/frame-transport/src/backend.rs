use crate::{Producer, RingHandle};
use anyhow::{Result, anyhow, ensure};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use vosulay_frame::{BrowserFrame, BrowserFrameBackend};

/// One newest ring generation for asynchronous game attachment, never a queue.
#[derive(Default)]
pub struct FrameMailbox {
    ring: Mutex<Option<(u64, RingHandle)>>,
    visible: AtomicBool,
    wake: Option<Arc<OwnedFd>>,
    generation: AtomicU64,
}
impl FrameMailbox {
    pub fn with_wake(wake: Arc<OwnedFd>) -> Self {
        Self {
            wake: Some(wake),
            ..Self::default()
        }
    }
    pub fn wake_control(&self) {
        let Some(wake) = &self.wake else {
            return;
        };
        let value = 1_u64;
        loop {
            let written =
                unsafe { libc::write(wake.as_raw_fd(), (&value as *const u64).cast(), 8) };
            if written >= 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
        // A saturated nonblocking eventfd is already readable.
    }
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
}
impl FrameBackend {
    /// The controller owns one live backend; dropping it releases its mailbox ring.
    pub fn new(mailbox: Arc<FrameMailbox>) -> Self {
        Self {
            mailbox,
            producer: None,
            extent: None,
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
            .mailbox
            .generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| anyhow!("frame ring generation exhausted"))?
            + 1;
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
        self.mailbox.wake_control();
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
        if self.mailbox.visible.swap(visible, Ordering::AcqRel) != visible {
            self.mailbox.wake_control();
        }
        Ok(())
    }
}

impl Drop for FrameBackend {
    fn drop(&mut self) {
        self.mailbox.visible.store(false, Ordering::Release);
        *self
            .mailbox
            .ring
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.mailbox.wake_control();
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, BrowserFrameBackend, FrameBackend, FrameMailbox};

    #[test]
    fn detached_backend_releases_ring_without_reusing_generation() {
        let mailbox = Arc::new(FrameMailbox::default());
        {
            let mut backend = FrameBackend::new(mailbox.clone());
            backend.resize(2, 2, 1.0).unwrap();
            backend.set_visible(true).unwrap();
            assert_eq!(mailbox.snapshot().unwrap().unwrap().0, 1);
        }
        assert!(!mailbox.visible());
        assert!(mailbox.snapshot().unwrap().is_none());
        let mut backend = FrameBackend::new(mailbox.clone());
        backend.resize(2, 2, 1.0).unwrap();
        assert_eq!(mailbox.snapshot().unwrap().unwrap().0, 2);
    }
}
