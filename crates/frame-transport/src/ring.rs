//! Single producer, newest-frame consumer. A read lease cannot be overwritten.
//!
//! Geometry trust boundary: both processes map the ring writable, so the
//! shared header geometry can change after handoff. Width, height, frame size
//! and slot offsets come only from `Geometry`, validated at creation or
//! receipt and kept in process-local memory. Shared copies are only compared,
//! to report corruption, and never feed pointer arithmetic, lengths, damage
//! clipping or copy ranges, so altered metadata cannot move an access outside
//! the mapping. Nothing here makes payload bytes, slot control words or the
//! damage history trustworthy; those remain a cooperative contract between
//! both processes, and history rectangles are clipped to local geometry.
use crate::protocol::{invalid, pixel_bytes};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering, fence},
    },
};
use vosulay_frame::{DamageRect, DamageRects};

const SLOTS: usize = 3;
/// Damage rectangles a consumer can still combine. A consumer further behind
/// than this uploads the whole frame.
///
/// A publication with several separate rectangles takes one sequence number,
/// and one history entry, per rectangle, and its slot carries the last of
/// them. Older consumers union every entry between two slot sequences, which
/// yields the publication's bounding box, so the layout (and `MAGIC`) stays
/// the same in both directions: a mismatched host and compositor upload a
/// bounding box instead of separate rectangles, never stale pixels.
const HISTORY: usize = 64;
// Native-endian load of these bytes; the shared memory holds them verbatim.
const MAGIC: u64 = u64::from_ne_bytes(*b"VLAYRNG2");
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
/// Region changed by one publication relative to the one before it, written
/// as a seqlock: `sequence` is zero while the rectangle is being replaced.
#[repr(C, align(32))]
struct History {
    sequence: AtomicU64,
    x: AtomicU32,
    y: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
}
// Header fields are atomics so the ordinary (atomic) metadata stores of a
// cooperating peer do not race with these loads. They are not a defence
// against arbitrary hostile writes; geometry safety instead rests on never
// using these fields for offsets or lengths. Atomic integers keep their plain
// layout, so the wire bytes are unchanged.
#[repr(C, align(64))]
struct Header {
    magic: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    frame_bytes: AtomicU64,
    slots: [Slot; SLOTS],
    history: [History; HISTORY],
}
const HEADER_BYTES: usize = size_of::<Header>();
const _: () = assert!(
    std::mem::offset_of!(Header, width) == 8
        && std::mem::offset_of!(Header, height) == 12
        && std::mem::offset_of!(Header, frame_bytes) == 16
        && std::mem::offset_of!(Header, slots) == 64
        && std::mem::offset_of!(Header, history) == 256
        && size_of::<History>() == 32
        && HEADER_BYTES == 256 + HISTORY * 32
);

/// Process-local ring geometry. It is validated before mapping and is the only
/// source of offsets and lengths; the shared header copy is only compared.
#[derive(Clone, Copy)]
struct Geometry {
    width: u32,
    height: u32,
    frame_bytes: usize,
}
impl Geometry {
    fn new(width: u32, height: u32) -> io::Result<Self> {
        Ok(Self {
            width,
            height,
            frame_bytes: pixel_bytes(width, height)?,
        })
    }
    fn mapping_bytes(self) -> usize {
        HEADER_BYTES + SLOTS * self.frame_bytes
    }
}

struct Mapping {
    fd: OwnedFd,
    pointer: NonNull<u8>,
    bytes: usize,
    geometry: Geometry,
}
// SAFETY: shared control is accessed atomically. Consumers own payloads only
// while leased and read them immutably; the sole producer writes only after a
// successful slot-state CAS. The producer may also compare its last published
// payload while readers access it because neither side writes that payload.
// Offsets and lengths come from immutable process-local geometry, never from
// peer-writable header fields.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn create(width: u32, height: u32) -> io::Result<Self> {
        let geometry = Geometry::new(width, height)?;
        let bytes = geometry.mapping_bytes();
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
        let mapping = Self::map(fd, geometry)?;
        let header = Header {
            magic: AtomicU64::new(MAGIC),
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
            frame_bytes: AtomicU64::new(geometry.frame_bytes as u64),
            slots: std::array::from_fn(|_| Slot {
                control: AtomicU64::new(FREE),
            }),
            history: std::array::from_fn(|_| History {
                sequence: AtomicU64::new(0),
                x: AtomicU32::new(0),
                y: AtomicU32::new(0),
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
            }),
        };
        // SAFETY: page alignment satisfies Header, mapping is writable and not yet shared.
        unsafe {
            mapping.pointer.as_ptr().cast::<Header>().write(header);
        }
        Ok(mapping)
    }
    fn receive(fd: OwnedFd, width: u32, height: u32) -> io::Result<Self> {
        let geometry = Geometry::new(width, height)?;
        let bytes = geometry.mapping_bytes();
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
        let mapping = Self::map(fd, geometry)?;
        mapping.check_header()?;
        Ok(mapping)
    }
    fn map(fd: OwnedFd, geometry: Geometry) -> io::Result<Self> {
        let bytes = geometry.mapping_bytes();
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
        Ok(Self {
            fd,
            pointer,
            bytes,
            geometry,
        })
    }
    fn header(&self) -> &Header {
        // SAFETY: mapping is aligned and at least HEADER_BYTES. Fields are only
        // accessed atomically and geometry fields are only compared.
        unsafe { &*self.pointer.as_ptr().cast::<Header>() }
    }
    /// Reports peer tampering with the shared header. Passing grants no trust:
    /// the peer may store again immediately, so callers keep using `geometry`
    /// instead of anything loaded here.
    fn check_header(&self) -> io::Result<()> {
        let header = self.header();
        let geometry = self.geometry;
        if header.magic.load(Ordering::Relaxed) != MAGIC
            || header.width.load(Ordering::Relaxed) != geometry.width
            || header.height.load(Ordering::Relaxed) != geometry.height
            || header.frame_bytes.load(Ordering::Relaxed) != geometry.frame_bytes as u64
        {
            return Err(invalid("frame ring header mismatch"));
        }
        Ok(())
    }
    /// Records the region `sequence` changed; call before publishing its slot.
    fn record_damage(&self, sequence: u64, rect: DamageRect) {
        let entry = &self.header().history[(sequence % HISTORY as u64) as usize];
        entry.sequence.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        entry.x.store(rect.x, Ordering::Relaxed);
        entry.y.store(rect.y, Ordering::Relaxed);
        entry.width.store(rect.width, Ordering::Relaxed);
        entry.height.store(rect.height, Ordering::Relaxed);
        entry.sequence.store(sequence, Ordering::Release);
    }
    /// The region `sequence` changed, or None once that entry is overwritten.
    fn damage(&self, sequence: u64) -> Option<DamageRect> {
        let entry = &self.header().history[(sequence % HISTORY as u64) as usize];
        if entry.sequence.load(Ordering::Acquire) != sequence {
            return None;
        }
        let rect = DamageRect {
            x: entry.x.load(Ordering::Relaxed),
            y: entry.y.load(Ordering::Relaxed),
            width: entry.width.load(Ordering::Relaxed),
            height: entry.height.load(Ordering::Relaxed),
        };
        fence(Ordering::Acquire);
        (entry.sequence.load(Ordering::Relaxed) == sequence).then_some(rect)
    }
    fn payload(&self, slot: usize) -> *mut u8 {
        assert!(slot < SLOTS, "frame ring slot out of range");
        // SAFETY: slot < SLOTS and the local frame size keep the slot inside
        // the mapped extent, which `map` sized from the same geometry.
        unsafe {
            self.pointer
                .as_ptr()
                .add(HEADER_BYTES + slot * self.geometry.frame_bytes)
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
        self.mapping.geometry.width
    }
    pub fn height(&self) -> u32 {
        self.mapping.geometry.height
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
    damage: [Option<DamageRects>; SLOTS],
    /// Change since the last successful publication, for the shared history.
    unpublished: Option<DamageRects>,
}
impl Producer {
    pub fn new(width: u32, height: u32) -> io::Result<Self> {
        Ok(Self {
            handle: RingHandle {
                mapping: Arc::new(Mapping::create(width, height)?),
            },
            sequence: 0,
            last_published_slot: None,
            damage: [Some(DamageRects::full(width, height)); SLOTS],
            unpublished: Some(DamageRects::full(width, height)),
        })
    }
    pub fn handle(&self) -> RingHandle {
        self.handle.clone()
    }
    /// Publish a complete BGRA snapshot without waiting for a leased slot.
    /// Damage must include changes since the preceding call, including dropped
    /// frames. None requires a full copy. Returns false for unchanged pixels or
    /// when all slots are leased; every readable slot remains a complete image.
    /// Fails with `InvalidData` if the peer has altered the shared header.
    pub fn publish(&mut self, pixels: &[u8], damage: Option<DamageRects>) -> io::Result<bool> {
        let mapping = &self.handle.mapping;
        let header = mapping.header();
        let Geometry {
            width,
            height,
            frame_bytes,
        } = mapping.geometry;
        mapping.check_header()?;
        if pixels.len() != frame_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame byte length mismatch",
            ));
        }
        let damage = damage
            .and_then(|rects| rects.clipped(width, height))
            .unwrap_or_else(|| DamageRects::full(width, height));
        // Each slot has its own older snapshot. Accumulate even if this call
        // is dropped, and clear only the slot actually brought up to date.
        for pending in self.damage.iter_mut().chain([&mut self.unpublished]) {
            let mut accumulated = pending.unwrap_or_default();
            accumulated.extend(&damage);
            *pending = Some(accumulated);
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
                if self.damage[index].is_none_or(|rects| {
                    rects.as_slice().iter().all(|&rect| {
                        damage_rows(rect, width)
                            .all(|range| previous[range.clone()] == pixels[range])
                    })
                }) {
                    // These pixels equal the last publication exactly.
                    self.unpublished = None;
                    return Ok(false);
                }
            }
        }
        let changed = self.unpublished.expect("publication damage accumulated");
        // One sequence (and history entry) per rectangle; the slot carries
        // the last. See `HISTORY`.
        let Some(sequence) = self
            .sequence
            .checked_add(changed.as_slice().len() as u64)
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
                // The accumulated region covers every change since this slot
                // was last written and is clipped to local geometry, so each
                // range lies within both `pixels` and this slot.
                let damage = self.damage[index].expect("current damage accumulated");
                for &rect in damage.as_slice() {
                    for range in damage_rows(rect, width) {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                pixels.as_ptr().add(range.start),
                                mapping.payload(index).add(range.start),
                                range.len(),
                            );
                        }
                    }
                }
                self.damage[index] = None;
                let first = sequence + 1 - changed.as_slice().len() as u64;
                for (offset, &rect) in changed.as_slice().iter().enumerate() {
                    mapping.record_damage(first + offset as u64, rect);
                }
                self.unpublished = None;
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
        self.mapping.geometry.width
    }
    pub fn height(&self) -> u32 {
        self.mapping.geometry.height
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Regions that changed between publication `uploaded` of this ring and
    /// this frame. None means copy the whole frame: `uploaded` is not an
    /// earlier publication, or its history has been overwritten.
    pub fn damage_since(&self, uploaded: u64) -> Option<DamageRects> {
        if uploaded == 0 || uploaded >= self.sequence || self.sequence - uploaded > HISTORY as u64 {
            return None;
        }
        let Geometry { width, height, .. } = self.mapping.geometry;
        let mut regions = DamageRects::default();
        for sequence in uploaded + 1..=self.sequence {
            regions.push(self.mapping.damage(sequence)?.clipped(width, height)?);
        }
        Some(regions)
    }
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: READING lease protects initialized bytes until this guard
        // drops; the length is the receipt-validated local frame size.
        unsafe {
            std::slice::from_raw_parts(
                self.mapping.payload(self.index),
                self.mapping.geometry.frame_bytes,
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
        assert!(producer.publish(&pixels, Some(rect(0).into())).unwrap());
        let retained = retained_reader.latest().unwrap();
        for (step, pixel) in [0, 7, 2, 5, 1, 6, 3, 4].into_iter().enumerate() {
            pixels[pixel as usize * 4..pixel as usize * 4 + 4].fill(step as u8 + 20);
            assert!(producer.publish(&pixels, Some(rect(pixel).into())).unwrap());
            assert_eq!(retained.pixels(), &[9; 32]);
        }
        assert_eq!(latest_reader.latest().unwrap().pixels(), &pixels);
        assert!(!producer.publish(&pixels, Some(rect(4).into())).unwrap());
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
        assert!(producer.publish(&pixels, Some(rect(5).into())).unwrap());
        let b = second.latest().unwrap();
        pixels[8..12].fill(3);
        assert!(producer.publish(&pixels, Some(rect(2).into())).unwrap());
        let c = third.latest().unwrap();
        let leased_c = pixels;
        pixels[28..32].fill(4);
        assert!(!producer.publish(&pixels, Some(rect(7).into())).unwrap());
        pixels[4..8].fill(5);
        assert!(!producer.publish(&pixels, Some(rect(1).into())).unwrap());
        drop(b);
        pixels[16..20].fill(6);
        assert!(producer.publish(&pixels, Some(rect(4).into())).unwrap());
        assert_eq!(second.latest().unwrap().pixels(), &pixels);
        assert_eq!(a.pixels(), &[1; 32]);
        assert_eq!(c.pixels(), &leased_c);
    }

    #[test]
    fn consumer_damage_spans_skipped_and_dropped_publications() {
        let mut producer = Producer::new(8, 4).unwrap();
        let mut game = consumer(&producer);
        let mut first = consumer(&producer);
        let mut second = consumer(&producer);
        let rect = |pixel: usize| DamageRect {
            x: pixel as u32 % 8,
            y: pixel as u32 / 8,
            width: 1,
            height: 1,
        };
        let mut pixels = [1; 128];
        let change = |pixels: &mut [u8; 128], pixel: usize, value: u8| {
            pixels[pixel * 4..pixel * 4 + 4].fill(value);
            Some(rect(pixel).into())
        };
        assert!(producer.publish(&pixels, None).unwrap());
        let uploaded = game.latest().unwrap();
        let damage = change(&mut pixels, 9, 2);
        assert!(producer.publish(&pixels, damage).unwrap());
        let a = first.latest().unwrap();
        let damage = change(&mut pixels, 10, 3);
        assert!(producer.publish(&pixels, damage).unwrap());
        let b = second.latest().unwrap();
        let after_b = b.sequence();
        // All three slots are leased: this change is dropped, so its damage
        // must travel with the next successful publication.
        let damage = change(&mut pixels, 11, 4);
        assert!(!producer.publish(&pixels, damage).unwrap());
        drop(a);
        let damage = change(&mut pixels, 12, 5);
        assert!(producer.publish(&pixels, damage).unwrap());
        drop(b);
        let base = uploaded.sequence();
        drop(uploaded);
        let frame = game.latest().unwrap();
        assert_eq!(frame.pixels(), &pixels);
        let row = |x: u32, width: u32| DamageRect {
            x,
            y: 1,
            width,
            height: 1,
        };
        // Each changed pixel stays its own rectangle; together they span
        // what a single bounding box would.
        let since_base = frame.damage_since(base).unwrap();
        assert_eq!(since_base.as_slice(), &[1, 2, 3, 4].map(|x| row(x, 1)));
        assert_eq!(since_base.bounds(), Some(row(1, 4)));
        let since_b = frame.damage_since(after_b).unwrap();
        assert_eq!(since_b.as_slice(), &[row(3, 1), row(4, 1)]);
        assert_eq!(frame.damage_since(frame.sequence()), None);
        assert_eq!(frame.damage_since(0), None);
    }

    #[test]
    fn separate_rectangles_reach_new_consumers_and_union_for_older_ones() {
        let mut producer = Producer::new(8, 4).unwrap();
        let mut game = consumer(&producer);
        let mut pixels = [0; 128];
        assert!(producer.publish(&pixels, None).unwrap());
        let base = game.latest().unwrap().sequence();
        let top_left = DamageRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let bottom_right = DamageRect {
            x: 6,
            y: 3,
            width: 2,
            height: 1,
        };
        pixels[..8].fill(1);
        pixels[120..].fill(1);
        let mut damage = DamageRects::from(top_left);
        damage.push(bottom_right);
        assert!(producer.publish(&pixels, Some(damage)).unwrap());
        let frame = game.latest().unwrap();
        assert_eq!(frame.pixels(), &pixels);
        assert_eq!(
            frame.damage_since(base).unwrap().as_slice(),
            &[top_left, bottom_right]
        );
        // A compositor from before separate rectangles unions every history
        // entry between the two slot sequences: the bounding box, so it
        // still uploads every changed pixel.
        let older = (base + 1..=frame.sequence())
            .map(|sequence| frame.mapping.damage(sequence).unwrap())
            .reduce(DamageRect::union);
        assert_eq!(older, Some(DamageRect::full(8, 4)));
    }

    #[test]
    fn damage_older_than_the_history_window_requires_a_full_copy() {
        let mut producer = Producer::new(4, 2).unwrap();
        let mut game = consumer(&producer);
        let mut pixels = [0; 32];
        assert!(producer.publish(&pixels, None).unwrap());
        let base = game.latest().unwrap().sequence();
        let single = DamageRect {
            x: 3,
            y: 1,
            width: 1,
            height: 1,
        };
        for step in 1..=HISTORY as u8 {
            pixels[28..32].fill(step);
            assert!(producer.publish(&pixels, Some(single.into())).unwrap());
        }
        let frame = game.latest().unwrap();
        assert_eq!(frame.sequence() - base, HISTORY as u64);
        assert_eq!(frame.damage_since(base), Some(single.into()));
        pixels[28..32].fill(0xff);
        drop(frame);
        assert!(producer.publish(&pixels, Some(single.into())).unwrap());
        let frame = game.latest().unwrap();
        assert_eq!(frame.damage_since(base), None);
        assert_eq!(
            frame.damage_since(frame.sequence() - 1),
            Some(single.into())
        );
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

    #[test]
    fn consumer_corrupted_geometry_fails_publish_without_out_of_bounds_access() {
        let mut producer = Producer::new(4, 2).unwrap();
        let mut consumer = consumer(&producer);
        // Tamper through the game's own mmap of the descriptor, as a hostile
        // consumer would, rather than through the producer's view.
        let game_mapping = Arc::clone(&consumer.mapping);
        assert_ne!(game_mapping.pointer, producer.handle.mapping.pointer);
        let shared = game_mapping.header();
        let mut pixels = [7; 32];
        assert!(producer.publish(&pixels, None).unwrap());
        let leased = consumer.latest().unwrap();
        let everything = DamageRect {
            x: 0,
            y: 0,
            width: u32::MAX,
            height: u32::MAX,
        };
        for (width, height, frame_bytes) in [
            (u32::MAX, 2, 32),
            (4, u32::MAX, 32),
            (8, 1, 32),
            (4, 2, 0),
            (4, 2, 64),
            (4, 2, u64::MAX / 2 + 1),
            (4, 2, u64::MAX),
            (u32::MAX, u32::MAX, u64::MAX),
        ] {
            shared.width.store(width, Ordering::Relaxed);
            shared.height.store(height, Ordering::Relaxed);
            shared.frame_bytes.store(frame_bytes, Ordering::Relaxed);
            // Changed pixels force a copy attempt rather than a no-op compare.
            let next = pixels[0] + 1;
            pixels.fill(next);
            for damage in [None, Some(everything.into())] {
                let error = producer.publish(&pixels, damage).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
            // A caller buffer sized to the forged frame length is refused too.
            let error = producer.publish(&[1; 64], None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!((leased.width(), leased.height()), (4, 2));
            assert_eq!(leased.pixels(), &[7; 32]);
        }
        shared.width.store(4, Ordering::Relaxed);
        shared.height.store(2, Ordering::Relaxed);
        shared.frame_bytes.store(32, Ordering::Relaxed);
        drop(leased);
        pixels[4..8].fill(99);
        assert!(producer.publish(&pixels, Some(everything.into())).unwrap());
        let frame = consumer.latest().unwrap();
        assert_eq!(frame.sequence(), 2);
        assert_eq!((frame.width(), frame.height()), (4, 2));
        assert_eq!(frame.pixels(), &pixels);
    }
}
