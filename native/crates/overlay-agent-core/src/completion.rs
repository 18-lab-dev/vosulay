//! Bounded, lock-free completion ring shared by the graphics backends.
//!
//! Frame release acknowledgements are produced on the game's presentation
//! thread and consumed by the control thread that owns the socket. Publishing
//! them through a mutex is not acceptable: the control thread holds that mutex
//! while `Seqpacket::send()` waits for writability, which can take up to the
//! socket's send deadline, so a present would block behind host backpressure.
//!
//! This is a Vyukov bounded MPMC queue. `push` and `pop` are wait-free apart
//! from contention retries, never allocate, and never block, so the present
//! path can publish a completion and continue immediately.

use overlay_protocol::FrameConsumed;
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Keeps the two hot indices off one another's cache line.
#[repr(align(64))]
struct Padded<T>(T);

struct Slot {
    /// Lamport stamp: equals the slot's index when writable, index + 1 when
    /// readable, and advances by the capacity on every lap.
    sequence: AtomicUsize,
    value: UnsafeCell<MaybeUninit<FrameConsumed>>,
}

pub struct CompletionQueue {
    slots: Box<[Slot]>,
    mask: usize,
    enqueue: Padded<AtomicUsize>,
    dequeue: Padded<AtomicUsize>,
    dropped: AtomicU64,
}

// The queue owns its slots and every access is index-arbitrated through the
// per-slot sequence stamp, so exactly one thread ever touches a slot's value.
unsafe impl Send for CompletionQueue {}
unsafe impl Sync for CompletionQueue {}

impl CompletionQueue {
    /// A capacity that comfortably exceeds the number of frames that can be in
    /// flight between two control-thread drains. Overflow means the host has
    /// stopped draining entirely, which the session watchdog handles.
    pub const DEFAULT_CAPACITY: usize = 256;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    /// `capacity` is rounded up to a power of two so the index wrap is a mask.
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        let slots = (0..capacity)
            .map(|index| Slot {
                sequence: AtomicUsize::new(index),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            mask: capacity - 1,
            enqueue: Padded(AtomicUsize::new(0)),
            dequeue: Padded(AtomicUsize::new(0)),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Number of completions that could not be published because the ring was
    /// full. A nonzero value means the host is no longer recycling frames.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Publish a completion. Returns `false` only when the ring is full, in
    /// which case the drop is counted rather than blocking the caller.
    pub fn push(&self, consumed: FrameConsumed) -> bool {
        let mut position = self.enqueue.0.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position & self.mask];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let difference = sequence as isize - position as isize;
            if difference == 0 {
                match self.enqueue.0.compare_exchange_weak(
                    position,
                    position + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // The stamp reserved this slot for us alone.
                        unsafe { (*slot.value.get()).write(consumed) };
                        slot.sequence.store(position + 1, Ordering::Release);
                        return true;
                    }
                    Err(current) => position = current,
                }
            } else if difference < 0 {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            } else {
                position = self.enqueue.0.load(Ordering::Relaxed);
            }
        }
    }

    pub fn pop(&self) -> Option<FrameConsumed> {
        let mut position = self.dequeue.0.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position & self.mask];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let difference = sequence as isize - (position + 1) as isize;
            if difference == 0 {
                match self.dequeue.0.compare_exchange_weak(
                    position,
                    position + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let value = unsafe { (*slot.value.get()).assume_init_read() };
                        slot.sequence
                            .store(position.wrapping_add(self.mask + 1), Ordering::Release);
                        return Some(value);
                    }
                    Err(current) => position = current,
                }
            } else if difference < 0 {
                return None;
            } else {
                position = self.dequeue.0.load(Ordering::Relaxed);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        let dequeue = self.dequeue.0.load(Ordering::Acquire);
        let slot = &self.slots[dequeue & self.mask];
        slot.sequence.load(Ordering::Acquire) != dequeue + 1
    }
}

impl Default for CompletionQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CompletionQueue {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn consumed(sequence: u64) -> FrameConsumed {
        FrameConsumed {
            target_id: 7,
            sequence,
        }
    }

    #[test]
    fn preserves_order_and_reports_emptiness() {
        let queue = CompletionQueue::with_capacity(4);
        assert!(queue.is_empty());
        assert!(queue.push(consumed(1)));
        assert!(queue.push(consumed(2)));
        assert!(!queue.is_empty());
        assert_eq!(queue.pop(), Some(consumed(1)));
        assert_eq!(queue.pop(), Some(consumed(2)));
        assert_eq!(queue.pop(), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn full_ring_counts_drops_instead_of_blocking() {
        let queue = CompletionQueue::with_capacity(2);
        assert!(queue.push(consumed(1)));
        assert!(queue.push(consumed(2)));
        assert!(!queue.push(consumed(3)));
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pop(), Some(consumed(1)));
        // Draining frees the slot again, so publication resumes.
        assert!(queue.push(consumed(4)));
    }

    #[test]
    fn wraps_repeatedly_without_losing_entries() {
        let queue = CompletionQueue::with_capacity(2);
        for sequence in 0..64 {
            assert!(queue.push(consumed(sequence)));
            assert_eq!(queue.pop(), Some(consumed(sequence)));
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn concurrent_producers_and_one_consumer_lose_nothing() {
        const PRODUCERS: u64 = 4;
        const PER_PRODUCER: u64 = 2_000;

        let queue = Arc::new(CompletionQueue::with_capacity(64));
        let producers = (0..PRODUCERS)
            .map(|producer| {
                let queue = Arc::clone(&queue);
                thread::spawn(move || {
                    for index in 0..PER_PRODUCER {
                        let value = FrameConsumed {
                            target_id: producer,
                            sequence: index,
                        };
                        // Retry rather than drop so the test asserts on totals.
                        while !queue.push(value) {
                            std::hint::spin_loop();
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        let mut seen = vec![0u64; PRODUCERS as usize];
        let mut total = 0;
        while total < PRODUCERS * PER_PRODUCER {
            if let Some(value) = queue.pop() {
                assert_eq!(value.sequence, seen[value.target_id as usize]);
                seen[value.target_id as usize] += 1;
                total += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        for producer in producers {
            producer.join().unwrap();
        }
        assert_eq!(queue.pop(), None);
        assert_eq!(seen, vec![PER_PRODUCER; PRODUCERS as usize]);
    }
}
