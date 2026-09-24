//! A reused host-visible staging buffer for per-frame uploads.
//!
//! Data that changes every frame cannot be written straight into the buffer
//! the GPU reads from: a buffer is either mapped by the host or read by a
//! submission, never both. The usual shortcut, [`wgpu::Queue::write_buffer`],
//! hides that by allocating a temporary staging buffer per call and
//! submitting the copy itself — a fresh allocation every frame, plus a
//! submission the caller cannot batch with the rest of the frame's work.
//!
//! [`StagingBuffer`] keeps those staging buffers instead: one per destination
//! buffer, written through a mapping carried across frames, with the copy
//! recorded into an encoder the caller already owns. A steady scene then
//! uploads without allocating and without an extra submission.
//!
//! ```
//! # let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! # let target = device.create_buffer(&wgpu::BufferDescriptor {
//! #     label: Some("target"),
//! #     size: 64,
//! #     usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
//! #     mapped_at_creation: false,
//! # });
//! use wgpu_unlit_render::staging::StagingBuffer;
//!
//! let mut staging = StagingBuffer::new();
//! let mut encoder = device.create_command_encoder(&Default::default());
//! staging.write(&device, &mut encoder, &target, 0, &[0u8; 16]);
//! queue.submit([encoder.finish()]);
//! ```
//!
//! # How a staging buffer is recycled
//!
//! A staging buffer is created mapped, so the first frame writes into it
//! without waiting for anything. To copy out of it, it is unmapped and the
//! copy is recorded; the mapping is then re-requested with
//! [`wgpu::CommandEncoder::map_buffer_on_submit`], whose callback runs once
//! the submission holding the copy has finished and marks the buffer writable
//! again.
//!
//! Nothing polls: [`wgpu::Queue::submit`] maintains the device itself, which
//! is what fires the callbacks. Until one has fired the buffer is treated as
//! busy, and a frame that needs a staging buffer while the previous ones are
//! still in flight — a frame or two ahead of the GPU — gets one of its own.
//! The pool therefore settles at as many buffers as there are frames in
//! flight, rather than growing per frame.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// One staging buffer and whether the host may write into its mapping.
struct Slot {
    /// The staging buffer itself: host-writable and copyable from.
    buffer: wgpu::Buffer,
    /// Size in bytes.
    size: u64,
    /// Whether the host owns the mapping, and so may write the next upload
    /// into this buffer. Set when the buffer is created mapped and again by the
    /// mapping callback; cleared by every write.
    writable: Arc<AtomicBool>,
}

impl Slot {
    /// Allocate a buffer of `size` bytes, mapped and ready to be written.
    fn new(device: &wgpu::Device, size: u64) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgpu_unlit_render::staging"),
            size,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        Self {
            buffer,
            size,
            writable: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Whether this buffer is writable and holds at least `size` bytes.
    fn can_reuse(&self, size: u64) -> bool {
        self.size >= size && self.writable.load(Ordering::Acquire)
    }
}

/// A pool of staging buffers for one destination buffer, reused across frames.
///
/// One of these belongs to each buffer a caller uploads to every frame; see
/// the module docs for the recycling scheme.
#[derive(Default)]
pub struct StagingBuffer {
    slots: Vec<Slot>,
}

impl StagingBuffer {
    /// A pool holding no staging buffer yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Upload `data` into `target` at `offset`, recording the copy into
    /// `encoder`.
    ///
    /// The bytes reach a reused staging buffer by host write and the GPU on
    /// the copy, so `target` needs [`wgpu::BufferUsages::COPY_DST`] and must
    /// not be mapped. `offset` and the length of `data` must both be multiples
    /// of [`wgpu::COPY_BUFFER_ALIGNMENT`], as [`wgpu::Queue::write_buffer`]
    /// requires. An empty `data` records no copy.
    ///
    /// # Panics
    ///
    /// If `offset` or `data.len()` is not a multiple of
    /// [`wgpu::COPY_BUFFER_ALIGNMENT`].
    pub fn write(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Buffer,
        offset: u64,
        data: &[u8],
    ) {
        let size = data.len() as u64;
        if size == 0 {
            return;
        }
        assert!(
            offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            "a staging upload offset of {offset} is not a multiple of `COPY_BUFFER_ALIGNMENT`"
        );
        assert!(
            size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            "a staging upload of {size} bytes is not a multiple of `COPY_BUFFER_ALIGNMENT`"
        );

        let slot = self.writable_slot(device, size);
        slot.buffer
            .slice(..size)
            .get_mapped_range_mut()
            .expect("a staging buffer is mapped while the host owns it")
            .copy_from_slice(data);
        // Unmapping ends the host's access, which the copy below needs; the
        // mapping is asked for again so the buffer outlives the copy.
        slot.buffer.unmap();
        encoder.copy_buffer_to_buffer(&slot.buffer, 0, target, offset, size);

        let writable = Arc::clone(&slot.writable);
        encoder.map_buffer_on_submit(&slot.buffer, wgpu::MapMode::Write, .., move |result| {
            // A failed mapping leaves the buffer marked unwritable: it is
            // never written again, which is safer than reusing memory the
            // device never handed back.
            if result.is_ok() {
                writable.store(true, Ordering::Release);
            }
        });
    }

    /// A buffer of at least `size` bytes the host may write into.
    ///
    /// The smallest writable buffer that fits is preferred, so a buffer grown
    /// for one heavy frame does not become the only candidate. A writable
    /// buffer too small for the frame is rebuilt in place: the frame outgrew
    /// it, and a buffer sized for the largest frame so far is the one later
    /// frames will reuse — accumulating one buffer per size ever seen is what
    /// makes reuse grow without bound. When every buffer is in flight another
    /// is added, so the pool settles at as many buffers as there are frames in
    /// flight rather than growing per frame.
    fn writable_slot(&mut self, device: &wgpu::Device, size: u64) -> &mut Slot {
        let index = match self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.can_reuse(size))
            .min_by_key(|(_, slot)| slot.size)
            .map(|(index, _)| index)
        {
            Some(index) => index,
            None => {
                match self
                    .slots
                    .iter()
                    .position(|slot| slot.writable.load(Ordering::Acquire))
                {
                    Some(index) => {
                        // Writable but too small: the frame has outgrown it.
                        self.slots[index] = Slot::new(device, size);
                        index
                    }
                    None => {
                        self.slots.push(Slot::new(device, size));
                        self.slots.len() - 1
                    }
                }
            }
        };
        // The caller is about to take the mapping, so the buffer is no longer
        // writable until the mapping callback hands it back — including a
        // buffer that was just created mapped.
        self.slots[index].writable.store(false, Ordering::Release);
        &mut self.slots[index]
    }

    /// Release every staging buffer the host is free to write, keeping only
    /// the ones still in flight.
    ///
    /// A buffer is kept automatically because it may be the one a later frame
    /// reuses, so a pool settles at the frames in flight rather than growing.
    /// That also means a frame that needed far more data than the ones after
    /// it leaves its buffer behind. Call this once uploads are known to have
    /// shrunk — a scene change, a resize — to give the memory back; the pool
    /// rebuilds from the frames in flight.
    pub fn release_idle(&mut self) {
        self.slots
            .retain(|slot| !slot.writable.load(Ordering::Acquire));
    }

    /// How many staging buffers the pool holds.
    ///
    /// Mostly useful to a caller deciding whether to [release idle
    /// buffers](Self::release_idle).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the pool holds no staging buffer at all.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test::noop_device;

    /// A destination buffer the tests upload into and read back from.
    fn target(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test::target"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    }

    /// Start an encoder, write `data` into `target` through `staging`, and
    /// submit it.
    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        staging: &mut StagingBuffer,
        target: &wgpu::Buffer,
        data: &[u8],
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        staging.write(device, &mut encoder, target, 0, data);
        queue.submit([encoder.finish()]);
    }

    /// Read `target` back and return its first `size` bytes.
    fn read_back(device: &wgpu::Device, target: &wgpu::Buffer, size: u64) -> Vec<u8> {
        let slice = target.slice(..size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).expect("the test reads the result");
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().expect("the mapping callback runs").unwrap();
        let bytes = slice
            .get_mapped_range()
            .expect("the mapping succeeded")
            .to_vec();
        target.unmap();
        bytes
    }

    #[test]
    fn a_write_reaches_the_target_through_the_staging_buffer() {
        let (device, queue) = noop_device();
        let target = target(&device, 16);
        let data: Vec<u8> = (0..16).collect();

        let mut staging = StagingBuffer::new();
        upload(&device, &queue, &mut staging, &target, &data);

        assert_eq!(read_back(&device, &target, 16), data);
    }

    #[test]
    fn the_pool_settles_instead_of_growing_per_frame() {
        let (device, queue) = noop_device();
        let target = target(&device, 16);

        // A frame's mapping is handed back once the submission that copied out
        // of its buffer completes, which a later submit drives. Frame N+1 can
        // therefore still find frame N's buffer in flight and add another, so
        // the pool settles at the frames in flight rather than at one buffer.
        let mut staging = StagingBuffer::new();
        for _ in 0..16 {
            upload(&device, &queue, &mut staging, &target, &[7; 16]);
        }

        assert_eq!(
            staging.len(),
            2,
            "a steady scene stops allocating after the frames in flight"
        );
        assert_eq!(read_back(&device, &target, 16), vec![7; 16]);
    }

    #[test]
    fn a_frame_arriving_while_the_last_is_in_flight_gets_its_own_buffer() {
        let (device, _queue) = noop_device();
        let target = target(&device, 16);
        let mut staging = StagingBuffer::new();

        // Two frames are recorded without submitting either, so the first
        // frame's mapping has not been handed back when the second runs.
        let mut first = device.create_command_encoder(&Default::default());
        staging.write(&device, &mut first, &target, 0, &[1; 16]);
        let mut second = device.create_command_encoder(&Default::default());
        staging.write(&device, &mut second, &target, 0, &[2; 16]);

        assert_eq!(
            staging.len(),
            2,
            "the second frame cannot write the buffer the first still holds"
        );
    }

    #[test]
    fn a_staging_buffer_too_small_for_the_frame_is_replaced_not_kept() {
        let (device, queue) = noop_device();
        let target = target(&device, 64);
        let mut staging = StagingBuffer::new();

        // The first frame sizes the pool at 16 bytes; the frames after it need
        // 64, so every buffer that was only large enough for the first is
        // rebuilt rather than kept alongside a larger one.
        upload(&device, &queue, &mut staging, &target, &[1; 16]);
        for _ in 0..4 {
            upload(&device, &queue, &mut staging, &target, &[2; 64]);
        }

        assert!(
            staging.slots.iter().all(|slot| slot.size >= 64),
            "a buffer sized for an earlier, smaller frame is not kept"
        );
        assert_eq!(read_back(&device, &target, 64), vec![2; 64]);
    }

    #[test]
    fn an_empty_write_records_nothing() {
        let (device, _queue) = noop_device();
        let target = target(&device, 16);
        let mut staging = StagingBuffer::new();
        let mut encoder = device.create_command_encoder(&Default::default());

        staging.write(&device, &mut encoder, &target, 0, &[]);

        assert!(staging.is_empty(), "nothing was staged");
    }

    #[test]
    fn releasing_idle_buffers_gives_back_the_grown_ones() {
        let (device, queue) = noop_device();
        let target = target(&device, 64);
        let mut staging = StagingBuffer::new();

        // A heavy frame grows the pool; the frames after it are small again.
        upload(&device, &queue, &mut staging, &target, &[1; 64]);
        upload(&device, &queue, &mut staging, &target, &[2; 16]);
        assert!(!staging.is_empty());

        staging.release_idle();

        assert!(
            staging.slots.iter().all(|slot| !slot.can_reuse(64)),
            "the buffer grown for the heavy frame was released"
        );
    }

    #[test]
    #[should_panic(expected = "COPY_BUFFER_ALIGNMENT")]
    fn a_write_of_unaligned_size_panics() {
        let (device, _queue) = noop_device();
        let target = target(&device, 16);
        let mut staging = StagingBuffer::new();
        let mut encoder = device.create_command_encoder(&Default::default());

        staging.write(&device, &mut encoder, &target, 0, &[0; 6]);
    }
}
