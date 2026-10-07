//! Single producer, newest-frame consumer. A read lease cannot be overwritten.
use crate::protocol::{invalid, pixel_bytes};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use vosulay_frame::DamageRect;

const SLOTS: usize = 3;
const MAGIC: [u8; 8] = *b"VLAYRNG1";
const FREE: u64 = 0;
const WRITING: u64 = 1;
const READY: u64 = 2;
const READING: u64 = 3;
const STATE_MASK: u64 = 3;
const SEALS: i32 = libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

#[repr(C, align(64))]
struct Slot {
    control: AtomicU64,
}
#[repr(C, align(64))]
struct Header {
    magic: [u8; 8],
    width: u32,
    height: u32,
    frame_bytes: u64,
    slots: [Slot; SLOTS],
}
const HEADER_BYTES: usize = size_of::<Header>();

struct Mapping {
    fd: OwnedFd,
    pointer: NonNull<u8>,
    bytes: usize,
}
// SAFETY: shared control is accessed atomically. Consumers own payloads only
// while leased and read them immutably; the sole producer writes only after a
// successful slot-state CAS. The producer may also compare its last published
// payload while readers access it because neither side writes that payload.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn create(width: u32, height: u32) -> io::Result<Self> {
        let frame_bytes = pixel_bytes(width, height)?;
        let bytes = HEADER_BYTES + SLOTS * frame_bytes;
        // SAFETY: static terminated name; the returned descriptor is uniquely owned.
        let raw = unsafe {
            libc::memfd_create(
                c"vosulay-frames".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a new descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: bounded extent, writable new memfd.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), bytes as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sealing-enabled memfd; retain write access while fixing its extent.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mapping = Self::map(fd, bytes)?;
        let header = Header {
            magic: MAGIC,
            width,
            height,
            frame_bytes: frame_bytes as u64,
            slots: std::array::from_fn(|_| Slot {
                control: AtomicU64::new(FREE),
            }),
        };
        // SAFETY: page alignment satisfies Header, mapping is writable and not yet shared.
        unsafe {
            mapping.pointer.as_ptr().cast::<Header>().write(header);
        }
        Ok(mapping)
    }
    fn receive(fd: OwnedFd, width: u32, height: u32) -> io::Result<Self> {
        let frame_bytes = pixel_bytes(width, height)?;
        let bytes = HEADER_BYTES + SLOTS * frame_bytes;
        // SAFETY: stat is a writable output struct; descriptor is live.
        let mut status: libc::stat = unsafe { zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut status) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if status.st_mode & libc::S_IFMT != libc::S_IFREG || status.st_size != bytes as libc::off_t
        {
            return Err(invalid("frame ring extent mismatch"));
        }
        // SAFETY: read-only descriptor metadata operation.
        let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & SEALS != SEALS {
            return Err(invalid("unsealed frame ring"));
        }
        let mapping = Self::map(fd, bytes)?;
        let header = mapping.header();
        if header.magic != MAGIC
            || header.width != width
            || header.height != height
            || header.frame_bytes != frame_bytes as u64
        {
            return Err(invalid("frame ring header mismatch"));
        }
        Ok(mapping)
    }
    fn map(fd: OwnedFd, bytes: usize) -> io::Result<Self> {
        // SAFETY: descriptor extent was checked and sealed; map owns this address until Drop.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(pointer) = NonNull::new(pointer.cast()) else {
            // SAFETY: a null successful mapping is not representable by this owner.
            unsafe {
                libc::munmap(pointer, bytes);
            }
            return Err(invalid("null frame mapping"));
        };
        Ok(Self { fd, pointer, bytes })
    }
    fn header(&self) -> &Header {
        // SAFETY: mapping is aligned and at least HEADER_BYTES; immutable header
        // fields are initialized before handoff and never changed afterward.
        unsafe { &*self.pointer.as_ptr().cast::<Header>() }
    }
    fn payload(&self, slot: usize) -> *mut u8 {
        // SAFETY: callers use an in-range slot and the validated header extent.
        unsafe {
            self.pointer
                .as_ptr()
                .add(HEADER_BYTES + slot * self.header().frame_bytes as usize)
        }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this owner holds exactly this mapping; Arc prevents premature unmap.
        unsafe {
            libc::munmap(self.pointer.as_ptr().cast(), self.bytes);
        }
    }
}

/// Descriptor ownership can outlive the producer during asynchronous handoff.
#[derive(Clone)]
pub struct RingHandle {
    mapping: Arc<Mapping>,
}
impl RingHandle {
    pub fn descriptor(&self) -> BorrowedFd<'_> {
        self.mapping.fd.as_fd()
    }
    pub fn width(&self) -> u32 {
        self.mapping.header().width
    }
    pub fn height(&self) -> u32 {
        self.mapping.header().height
    }
    pub fn mapping_len(&self) -> usize {
        self.mapping.bytes
    }
}

fn damage_rows(rect: DamageRect, width: u32) -> impl Iterator<Item = std::ops::Range<usize>> {
    let stride = width as usize * 4;
    let start = rect.y as usize * stride + rect.x as usize * 4;
    let (length, rows) = if rect.x == 0 && rect.width == width {
        (stride * rect.height as usize, 1)
    } else {
        (rect.width as usize * 4, rect.height as usize)
    };
    (0..rows).map(move |row| {
        let offset = start + row * stride;
        offset..offset + length
    })
}

pub struct Producer {
    handle: RingHandle,
    sequence: u64,
    last_published_slot: Option<usize>,
    damage: [Option<DamageRect>; SLOTS],
}
impl Producer {
    pub fn new(width: u32, height: u32) -> io::Result<Self> {
        Ok(Self {
            handle: RingHandle {
                mapping: Arc::new(Mapping::create(width, height)?),
            },
            sequence: 0,
            last_published_slot: None,
            damage: [Some(DamageRect::full(width, height)); SLOTS],
        })
    }
    pub fn handle(&self) -> RingHandle {
        self.handle.clone()
    }
    /// Publish a complete BGRA snapshot without waiting for a leased slot.
    /// Damage must include changes since the preceding call, including dropped
    /// frames. None requires a full copy. Returns false for unchanged pixels or
    /// when all slots are leased; every readable slot remains a complete image.
    pub fn publish(&mut self, pixels: &[u8], damage: Option<DamageRect>) -> io::Result<bool> {
        let mapping = &self.handle.mapping;
        let header = mapping.header();
        if pixels.len() != header.frame_bytes as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame byte length mismatch",
            ));
        }
        let damage = damage
            .and_then(|rect| rect.clipped(header.width, header.height))
            .unwrap_or_else(|| DamageRect::full(header.width, header.height));
        // Each slot has its own older snapshot. Accumulate even if this call
        // is dropped, and clear only the slot actually brought up to date.
        for pending in &mut self.damage {
            *pending = Some(pending.map_or(damage, |previous| previous.union(damage)));
        }
        if let Some(index) = self.last_published_slot {
            let control = header.slots[index].control.load(Ordering::Acquire);
            if control >> 2 == self.sequence && control & STATE_MASK != WRITING {
                // SAFETY: this slot contains the last successful publication.
                // Consumers may transition its control state, but only read
                // its payload; this single producer cannot overwrite it until
                // after the comparison below finishes.
                let previous =
                    unsafe { std::slice::from_raw_parts(mapping.payload(index), pixels.len()) };
                if self.damage[index].is_none_or(|rect| {
                    damage_rows(rect, header.width)
                        .all(|range| previous[range.clone()] == pixels[range])
                }) {
                    return Ok(false);
                }
            }
        }
        let Some(sequence) = self
            .sequence
            .checked_add(1)
            .filter(|sequence| *sequence <= u64::MAX >> 2)
        else {
            return Err(invalid("frame sequence exhausted"));
        };
        // At most two scans and three CAS attempts per scan. Prefer a free slot,
        // otherwise replace the oldest complete frame; never touch a reader.
        for _ in 0..2 {
            let mut candidates = [(usize::MAX, u64::MAX); SLOTS];
            for (index, slot) in header.slots.iter().enumerate() {
                let control = slot.control.load(Ordering::Acquire);
                if control & STATE_MASK == FREE || control & STATE_MASK == READY {
                    candidates[index] = (index, control);
                }
            }
            candidates.sort_unstable_by_key(|(_, control)| *control);
            for (index, control) in candidates {
                if index == usize::MAX {
                    continue;
                }
                let writing = sequence << 2 | WRITING;
                if header.slots[index]
                    .control
                    .compare_exchange(control, writing, Ordering::AcqRel, Ordering::Relaxed)
                    .is_err()
                {
                    continue;
                }
                // SAFETY: the CAS acquired exclusive payload write ownership.
                // The accumulated clipped region covers every change since
                // this slot was last written, and every byte range is in bounds.
                let damage = self.damage[index].expect("current damage accumulated");
                for range in damage_rows(damage, header.width) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            pixels.as_ptr().add(range.start),
                            mapping.payload(index).add(range.start),
                            range.len(),
                        );
                    }
                }
                self.damage[index] = None;
                header.slots[index]
                    .control
                    .store(sequence << 2 | READY, Ordering::Release);
                self.sequence = sequence;
                self.last_published_slot = Some(index);
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub struct Consumer {
    mapping: Arc<Mapping>,
    last_sequence: u64,
}
impl Consumer {
    pub fn from_handle(memory: OwnedFd, width: u32, height: u32) -> io::Result<Self> {
        Ok(Self {
            mapping: Arc::new(Mapping::receive(memory, width, height)?),
            last_sequence: 0,
        })
    }
    /// Reacquire retained pixels after graphics resources lose their texture.
    /// Normal presentation uses `latest` and never uploads an idle frame twice.
    pub fn replay_latest(&mut self) -> Option<ReadFrame> {
        self.last_sequence = 0;
        self.latest()
    }
    /// Acquire the newest complete frame in bounded time. The guard spans only
    /// CPU access/upload, not subsequent GPU execution or presentation.
    /// The mapping stays alive even if control replaces or drops the consumer.
    pub fn latest(&mut self) -> Option<ReadFrame> {
        for _ in 0..2 {
            let mut newest = None;
            for (index, slot) in self.mapping.header().slots.iter().enumerate() {
                let control = slot.control.load(Ordering::Acquire);
                let sequence = control >> 2;
                if control & STATE_MASK == READY
                    && sequence > self.last_sequence
                    && newest.is_none_or(|(_, newest_control)| control > newest_control)
                {
                    newest = Some((index, control));
                }
            }
            let (index, control) = newest?;
            // Packed sequence/state makes this CAS ABA-safe when a producer
            // replaces the candidate between the scan and ownership acquisition.
            if self.mapping.header().slots[index]
                .control
                .compare_exchange(
                    control,
                    control & !STATE_MASK | READING,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                continue;
            }
            let sequence = control >> 2;
            self.last_sequence = sequence;
            for (other_index, slot) in self.mapping.header().slots.iter().enumerate() {
                if other_index == index {
                    continue;
                }
                let stale = slot.control.load(Ordering::Acquire);
                if stale & STATE_MASK == READY && stale >> 2 < sequence {
                    let _ = slot.control.compare_exchange(
                        stale,
                        FREE,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    );
                }
            }
            return Some(ReadFrame {
                mapping: Arc::clone(&self.mapping),
                index,
                sequence,
            });
        }
        None
    }
}

pub struct ReadFrame {
    mapping: Arc<Mapping>,
    index: usize,
    sequence: u64,
}
impl ReadFrame {
    pub fn width(&self) -> u32 {
        self.mapping.header().width
    }
    pub fn height(&self) -> u32 {
        self.mapping.header().height
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: READING lease protects initialized bytes until this guard drops.
        unsafe {
            std::slice::from_raw_parts(
                self.mapping.payload(self.index),
                self.mapping.header().frame_bytes as usize,
            )
        }
    }
}
impl Drop for ReadFrame {
    fn drop(&mut self) {
        // Preserve the newest complete pixels for a restarted game/context.
        // Writers may replace READY slots; they still cannot touch a read lease.
        self.mapping.header().slots[self.index]
            .control
            .store(self.sequence << 2 | READY, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn consumer(producer: &Producer) -> Consumer {
        let handle = producer.handle();
        let memory = handle.descriptor();
        Consumer::from_handle(
            memory.try_clone_to_owned().unwrap(),
            handle.width(),
            handle.height(),
        )
        .unwrap()
    }
    #[test]
    fn duplicate_pixels_do_not_publish_a_new_frame() {
        let mut producer = Producer::new(2, 1).unwrap();
        let mut consumer = consumer(&producer);
        assert!(producer.publish(&[17; 8], None).unwrap());
        let first = consumer.latest().unwrap();
        assert_eq!(first.sequence(), 1);
        assert!(!producer.publish(&[17; 8], None).unwrap());
        assert_eq!(first.pixels(), &[17; 8]);
        drop(first);
        assert!(consumer.latest().is_none());
        let replayed = consumer.replay_latest().unwrap();
        assert_eq!(replayed.sequence(), 1);
        assert_eq!(replayed.pixels(), &[17; 8]);
    }
    #[test]
    fn changed_pixels_can_return_to_a_previous_image() {
        let mut producer = Producer::new(2, 1).unwrap();
        let mut consumer = consumer(&producer);
        assert!(producer.publish(&[1; 8], None).unwrap());
        assert!(producer.publish(&[2; 8], None).unwrap());
        assert!(producer.publish(&[1; 8], None).unwrap());
        let frame = consumer.latest().unwrap();
        assert_eq!(frame.sequence(), 3);
        assert_eq!(frame.pixels(), &[1; 8]);
    }
    #[test]
    fn new_generation_publishes_first_pixels_even_when_zeroed() {
        let mut producer = Producer::new(1, 1).unwrap();
        let mut initial_consumer = consumer(&producer);
        assert!(producer.publish(&[0; 4], None).unwrap());
        assert_eq!(initial_consumer.latest().unwrap().sequence(), 1);
        assert!(!producer.publish(&[0; 4], None).unwrap());
        let mut resized = Producer::new(1, 1).unwrap();
        let mut resized_consumer = consumer(&resized);
        assert!(resized.publish(&[0; 4], None).unwrap());
        assert_eq!(resized_consumer.latest().unwrap().sequence(), 1);
    }
    #[test]
    fn delayed_reader_gets_newest_complete_frame() {
        let mut producer = Producer::new(2, 1).unwrap();
        let mut consumer = consumer(&producer);
        for value in 1..=40 {
            assert!(producer.publish(&[value; 8], None).unwrap());
        }
        let frame = consumer.latest().unwrap();
        assert_eq!(frame.sequence(), 40);
        assert_eq!(frame.pixels(), &[40; 8]);
        drop(frame);
        assert!(consumer.latest().is_none());
    }
    #[test]
    fn recreated_consumer_receives_idle_pixels_without_another_browser_paint() {
        let mut producer = Producer::new(2, 1).unwrap();
        let mut first = consumer(&producer);
        assert!(producer.publish(&[17; 8], None).unwrap());
        assert_eq!(first.latest().unwrap().pixels(), &[17; 8]);
        assert!(first.latest().is_none());
        let mut replacement = consumer(&producer);
        let frame = replacement
            .latest()
            .expect("new graphics context needs retained idle pixels");
        assert_eq!(frame.sequence(), 1);
        assert_eq!(frame.pixels(), &[17; 8]);
        drop(frame);
        assert!(replacement.latest().is_none());
        assert_eq!(replacement.replay_latest().unwrap().pixels(), &[17; 8]);
    }
    #[test]
    fn leased_pixels_are_stable_while_stale_slots_are_replaced() {
        let mut producer = Producer::new(4, 4).unwrap();
        let mut consumer = consumer(&producer);
        producer.publish(&[11; 64], None).unwrap();
        let frame = consumer.latest().unwrap();
        for value in 12..=200 {
            assert!(producer.publish(&[value; 64], None).unwrap());
            assert_eq!(frame.pixels(), &[11; 64]);
        }
        drop(frame);
        assert_eq!(consumer.latest().unwrap().pixels(), &[200; 64]);
    }
    #[test]
    fn read_lease_survives_consumer_replacement_and_keeps_pixels_protected() {
        let mut producer = Producer::new(2, 1).unwrap();
        let mut reader = consumer(&producer);
        assert!(producer.publish(&[17; 8], None).unwrap());
        let frame = reader.latest().unwrap();
        drop(reader);
        for value in 18..=40 {
            assert!(producer.publish(&[value; 8], None).unwrap());
        }
        let mut replacement = consumer(&producer);
        assert_eq!(replacement.latest().unwrap().pixels(), &[40; 8]);
        assert_eq!(frame.pixels(), &[17; 8]);
        drop(frame);
        assert!(producer.publish(&[41; 8], None).unwrap());
        assert_eq!(replacement.latest().unwrap().pixels(), &[41; 8]);
    }
    #[test]
    fn saturated_ring_drops_without_overwriting_readers() {
        let mut producer = Producer::new(1, 1).unwrap();
        let mut first = consumer(&producer);
        let mut second = consumer(&producer);
        let mut third = consumer(&producer);
        producer.publish(&[1; 4], None).unwrap();
        let a = first.latest().unwrap();
        producer.publish(&[2; 4], None).unwrap();
        let b = second.latest().unwrap();
        producer.publish(&[3; 4], None).unwrap();
        let c = third.latest().unwrap();
        assert!(!producer.publish(&[4; 4], None).unwrap());
        assert_eq!(a.pixels(), &[1; 4]);
        assert_eq!(b.pixels(), &[2; 4]);
        assert_eq!(c.pixels(), &[3; 4]);
        drop(b);
        assert!(producer.publish(&[4; 4], None).unwrap());
        let frame = second.latest().unwrap();
        assert_eq!(frame.sequence(), 4);
        assert_eq!(frame.pixels(), &[4; 4]);
    }
    #[test]
    fn partial_publications_keep_complete_snapshots_across_slot_reuse() {
        let mut producer = Producer::new(4, 2).unwrap();
        let mut retained_reader = consumer(&producer);
        let mut latest_reader = consumer(&producer);
        let mut pixels = [9; 32];
        let rect = |pixel: u32| DamageRect {
            x: pixel % 4,
            y: pixel / 4,
            width: 1,
            height: 1,
        };
        // A newly allocated slot still needs the complete initial snapshot.
        assert!(producer.publish(&pixels, Some(rect(0))).unwrap());
        let retained = retained_reader.latest().unwrap();
        for (step, pixel) in [0, 7, 2, 5, 1, 6, 3, 4].into_iter().enumerate() {
            pixels[pixel as usize * 4..pixel as usize * 4 + 4].fill(step as u8 + 20);
            assert!(producer.publish(&pixels, Some(rect(pixel))).unwrap());
            assert_eq!(retained.pixels(), &[9; 32]);
        }
        assert_eq!(latest_reader.latest().unwrap().pixels(), &pixels);
        assert!(!producer.publish(&pixels, Some(rect(4))).unwrap());
        assert!(latest_reader.latest().is_none());
    }

    #[test]
    fn dropped_partial_frames_accumulate_damage_before_a_slot_is_reused() {
        let mut producer = Producer::new(4, 2).unwrap();
        let mut first = consumer(&producer);
        let mut second = consumer(&producer);
        let mut third = consumer(&producer);
        let rect = |pixel: u32| DamageRect {
            x: pixel % 4,
            y: pixel / 4,
            width: 1,
            height: 1,
        };
        let mut pixels = [1; 32];
        assert!(producer.publish(&pixels, None).unwrap());
        let a = first.latest().unwrap();
        pixels[20..24].fill(2);
        assert!(producer.publish(&pixels, Some(rect(5))).unwrap());
        let b = second.latest().unwrap();
        pixels[8..12].fill(3);
        assert!(producer.publish(&pixels, Some(rect(2))).unwrap());
        let c = third.latest().unwrap();
        let leased_c = pixels;
        pixels[28..32].fill(4);
        assert!(!producer.publish(&pixels, Some(rect(7))).unwrap());
        pixels[4..8].fill(5);
        assert!(!producer.publish(&pixels, Some(rect(1))).unwrap());
        drop(b);
        pixels[16..20].fill(6);
        assert!(producer.publish(&pixels, Some(rect(4))).unwrap());
        assert_eq!(second.latest().unwrap().pixels(), &pixels);
        assert_eq!(a.pixels(), &[1; 32]);
        assert_eq!(c.pixels(), &leased_c);
    }

    #[test]
    fn racing_publish_and_consume_never_tears_pixels() {
        let mut producer = Producer::new(64, 64).unwrap();
        let mut consumer = consumer(&producer);
        let writer = std::thread::spawn(move || {
            let mut pixels = vec![0; 64 * 64 * 4];
            for sequence in 1_u64..=2000 {
                pixels.fill((sequence % 251) as u8);
                assert!(producer.publish(&pixels, None).unwrap());
            }
        });
        let mut last = 0;
        while !writer.is_finished() {
            if let Some(frame) = consumer.latest() {
                assert!(frame.sequence() > last);
                last = frame.sequence();
                assert!(
                    frame
                        .pixels()
                        .iter()
                        .all(|byte| *byte == (last % 251) as u8)
                );
            } else {
                std::thread::yield_now();
            }
        }
        writer.join().unwrap();
        if let Some(frame) = consumer.latest() {
            last = frame.sequence();
            assert!(
                frame
                    .pixels()
                    .iter()
                    .all(|byte| *byte == (last % 251) as u8)
            );
        }
        assert_eq!(last, 2000);
    }
}
