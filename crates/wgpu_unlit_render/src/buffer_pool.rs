//! A pool that packs many small ranges into one GPU buffer.
//!
//! Uploading a mesh per buffer is wasteful: every `wgpu::Buffer` carries
//! driver overhead, and a draw can only bind a handful of them. A
//! [`BufferPool`] instead owns one large buffer and hands out byte ranges
//! inside it with an [`Allocator`](crate::offset_allocator::Allocator), so
//! every mesh of a kind shares the same buffer.
//!
//! A range is a [`BufferRange`]: a byte offset and a size, plus the
//! [`Allocation`] that keeps the range reserved until it is handed back with
//! [`BufferPool::release`]. A range names no buffer of its own, so it stays
//! cheap to store and copy; read the buffer from [`BufferPool::buffer`]
//! whenever you need to bind or write into it.
//!
//! Ranges never move while they live — growing the pool copies the buffer and
//! appends free space rather than re-packing — so an offset stays valid across
//! a grow and only the buffer handle changes.
//!
//! Every range starts at a multiple of [`COPY_BUFFER_ALIGNMENT`], which is
//! what a GPU buffer sub-allocation needs. The pool grows by doubling when a
//! range does not fit, copying the old contents into the new buffer.
//!
//! # Sizing
//!
//! A range's allocation covers the requested size rounded up to the
//! alignment, which [`BufferRange::size`] reports. Sizing the pool with
//! [`min_allocator_size`](crate::offset_allocator::min_allocator_size) is not
//! needed here: the pool grows on demand.
//!
//! # Example
//!
//! ```
//! use wgpu_unlit_render::buffer_pool::BufferPool;
//!
//! let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let mut pool = BufferPool::new(
//!     &device,
//!     "example::pool",
//!     wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
//!     1024,
//! );
//!
//! let range = pool.allocate(&device, &queue, 64).expect("the pool has room");
//! queue.write_buffer(pool.buffer(), range.offset() as u64, &[0u8; 64]);
//! pool.release(range.allocation());
//! ```

use crate::offset_allocator::{Allocation, Allocator, min_allocator_size};
use core::num::NonZeroU32;

/// The alignment every range starts at, in bytes.
///
/// This is wgpu's own copy alignment, which a vertex- or index-buffer offset
/// also has to satisfy; a multiple of it is a multiple of an index format's
/// size too, so one alignment serves every buffer kind.
const ALIGNMENT: NonZeroU32 = match NonZeroU32::new(wgpu::COPY_BUFFER_ALIGNMENT as u32) {
    Some(alignment) => alignment,
    None => unreachable!(),
};

/// How many ranges a pool tracks unless the caller says otherwise.
///
/// The allocator's own default (128 Ki nodes, roughly 3.5 MB of metadata) is
/// sized for a general-purpose allocator; a pool holds one range per mesh
/// part, so it needs far fewer.
const DEFAULT_MAX_RANGES: u32 = 4096;

/// The smallest pool size, so that the first grow has something to grow from.
const MIN_SIZE: u64 = wgpu::COPY_BUFFER_ALIGNMENT;

/// A byte range inside a [`BufferPool`]'s buffer.
///
/// The range stays valid — and its data stays put — until its
/// [`allocation`](Self::allocation) is handed back with
/// [`BufferPool::release`]. Growing the pool replaces the buffer but keeps
/// every offset, so read the buffer from [`BufferPool::buffer`] instead of
/// holding one.
#[derive(Clone, Copy, Debug)]
pub struct BufferRange {
    /// The allocation that keeps the range reserved.
    allocation: Allocation,
    /// The size reserved for the range, in bytes.
    ///
    /// Cached at allocation time so the handle stays self-contained: the
    /// allocator entry is the pool's, and the pool may grow behind it.
    allocation_size: u32,
}

impl BufferRange {
    /// The byte offset the range starts at.
    ///
    /// This is always a multiple of [`COPY_BUFFER_ALIGNMENT`].
    pub fn offset(&self) -> u32 {
        self.allocation.offset
    }

    /// The size reserved for the range, in bytes.
    ///
    /// This is the requested size rounded up to the alignment, so it is never
    /// smaller than what was asked for.
    pub fn size(&self) -> u32 {
        // The allocation outlives the pool's allocator entry for it, and its
        // size only changes when it is released, so reading it here is safe
        // even after the pool has grown.
        self.allocation_size
    }

    /// The allocation backing the range, to hand back to
    /// [`BufferPool::release`].
    pub fn allocation(&self) -> Allocation {
        self.allocation
    }
}

/// One GPU buffer sub-allocated into many ranges.
///
/// See the [module documentation](self) for what a pool is for.
pub struct BufferPool {
    /// The buffer every range lives in.
    buffer: wgpu::Buffer,
    /// The label the buffer is created with, reused when it is replaced.
    label: &'static str,
    /// The usages the buffer is created with.
    usage: wgpu::BufferUsages,
    /// Where the free space is.
    allocator: Allocator,
}

impl BufferPool {
    /// Creates a pool over a buffer of `size` bytes with the given usages.
    ///
    /// The buffer is created immediately. `size` is rounded up to a multiple
    /// of [`COPY_BUFFER_ALIGNMENT`].
    ///
    /// # Panics
    ///
    /// Panics if the usages do not include [`wgpu::BufferUsages::COPY_DST`],
    /// which the pool needs to grow.
    pub fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> Self {
        assert!(
            usage.contains(wgpu::BufferUsages::COPY_DST),
            "a pool grows by copying, so it needs COPY_DST"
        );

        let size = round_up_size(size);
        let buffer = create_buffer(device, label, usage, size);
        Self {
            buffer,
            label,
            usage,
            allocator: Allocator::with_max_nodes_and_alignment(
                size as u32,
                DEFAULT_MAX_RANGES,
                ALIGNMENT,
            ),
        }
    }

    /// The buffer every range lives in.
    ///
    /// This changes when the pool grows, so read it again instead of holding
    /// one: a range stays valid across a grow, but names no buffer of its own
    /// to read it from.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// The pool's total size, in bytes.
    pub fn size(&self) -> u64 {
        u64::from(self.allocator.size())
    }

    /// The free space left, in bytes.
    pub fn free_space(&self) -> u32 {
        self.allocator.storage_report().total_free_space
    }

    /// The largest range that can be allocated in one piece, in bytes.
    pub fn largest_free_range(&self) -> u32 {
        self.allocator.storage_report().largest_free_region
    }

    /// Reserves a range of `size` bytes, growing the pool if it does not fit.
    ///
    /// Returns `None` only if the pool cannot grow any further (its size would
    /// overflow a `u32`, or it has run out of node slots for its ranges). The
    /// returned range's offset is a multiple of [`COPY_BUFFER_ALIGNMENT`], and
    /// its allocation covers `size` rounded up to it.
    ///
    /// Growing allocates a new buffer of twice the pool's size, copies the old
    /// contents into it, and appends the difference to the allocator. Existing
    /// ranges keep their offsets, but the buffer they name changes, so a
    /// caller holding one must re-read [`BufferRange::buffer`].
    pub fn allocate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: u32,
    ) -> Option<BufferRange> {
        if let Some(allocation) = self.allocator.allocate(size) {
            return Some(self.range(allocation));
        }

        // The allocation may fail because the pool is full or because the
        // ranges are fragmented; growing helps in both cases as long as there
        // is a free node to describe the appended space.
        self.grow(device, queue, size)?;
        let allocation = self
            .allocator
            .allocate(size)
            .expect("the grown pool has room for the range that did not fit");
        Some(self.range(allocation))
    }

    /// Hands a range back to the pool.
    ///
    /// # Panics
    ///
    /// Panics if the allocation was already released.
    pub fn release(&mut self, allocation: Allocation) {
        self.allocator.free(allocation);
    }

    /// Builds the handle for a fresh allocation.
    fn range(&self, allocation: Allocation) -> BufferRange {
        BufferRange {
            allocation,
            allocation_size: self.allocator.allocation_size(allocation),
        }
    }

    /// Doubles the pool until `size` fits, copying the contents across.
    ///
    /// Returns `None` if the pool cannot grow, leaving it unchanged.
    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> Option<()> {
        // Grow by the larger of a doubling and what the range that did not fit
        // needs, so one grow is always enough for it.
        //
        // The size to grow by is the allocator's minimum for `size`, not
        // `size` itself: a free range is filed under its rounded-down size
        // while an allocation searches from its rounded-up size, so a range
        // sized exactly `size` would not be found by an allocation of `size`.
        // `min_allocator_size` is the smallest size that both holds `size` and
        // is representable by a bin, which is what makes the range findable.
        let current = self.allocator.size();
        let needed = min_allocator_size(size, ALIGNMENT);
        let additional = current.max(needed);
        let target = current.checked_add(additional)?;

        // Reserve the node for the appended range before allocating anything:
        // `extend` is the step that can fail, and a failure here must not
        // leave a replaced buffer behind.
        if !self.allocator.extend(additional) {
            return None;
        }
        debug_assert_eq!(self.allocator.size(), target);

        let new_buffer = create_buffer(
            device,
            self.label,
            self.usage,
            u64::from(self.allocator.size()),
        );

        // Copy the old contents across. Everything the allocator tracks lives
        // below the old size, so the copy covers every live range.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(self.label),
        });
        encoder.copy_buffer_to_buffer(&self.buffer, 0, &new_buffer, 0, u64::from(current));
        queue.submit([encoder.finish()]);

        self.buffer = new_buffer;
        Some(())
    }
}

impl core::fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let report = self.allocator.storage_report();
        f.debug_struct("BufferPool")
            .field("label", &self.label)
            .field("size", &self.size())
            .field("free_space", &report.total_free_space)
            .field("largest_free_range", &report.largest_free_region)
            .finish()
    }
}

/// Rounds a byte size up to the pool's alignment, with a floor of one unit.
fn round_up_size(size: u64) -> u64 {
    let alignment = u64::from(ALIGNMENT.get());
    let rounded = size.max(MIN_SIZE).div_ceil(alignment) * alignment;
    assert!(
        rounded <= u64::from(u32::MAX),
        "a pool cannot be larger than u32::MAX bytes"
    );
    rounded
}

/// Creates the pool's buffer with the usages it needs to grow.
fn create_buffer(
    device: &wgpu::Device,
    label: &'static str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_device() -> (wgpu::Device, wgpu::Queue) {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
    }

    fn pool(device: &wgpu::Device, size: u64) -> BufferPool {
        BufferPool::new(
            device,
            "test::pool",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            size,
        )
    }

    #[test]
    fn ranges_are_aligned_and_do_not_overlap() {
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 1024);

        let mut ranges = Vec::new();
        for size in [1u32, 3, 4, 5, 100, 200, 300] {
            let range = pool
                .allocate(&device, &queue, size)
                .expect("the pool has room");
            assert_eq!(range.offset() % ALIGNMENT.get(), 0);
            assert!(range.size() >= size);
            ranges.push((range.offset(), range.size()));
        }

        ranges.sort_unstable();
        for pair in ranges.windows(2) {
            let (offset, size) = pair[0];
            assert!(
                offset + size <= pair[1].0,
                "range {offset}..{} overlaps {}",
                offset + size,
                pair[1].0
            );
        }
    }

    #[test]
    fn growing_keeps_offsets_and_preserves_the_old_ranges() {
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 64);

        let first = pool
            .allocate(&device, &queue, 32)
            .expect("the pool has room");
        assert_eq!(first.offset(), 0);
        let size_before = pool.size();

        // This does not fit in the remaining 32 bytes, so the pool grows.
        let second = pool.allocate(&device, &queue, 200).expect("the pool grows");
        assert!(pool.size() > size_before, "the pool grew");
        assert_eq!(first.offset(), 0, "the first range did not move");
        assert!(
            second.offset() + second.size() <= pool.size() as u32,
            "the new range fits in the pool"
        );
        assert!(second.size() >= 200);
        // The old range and the new one do not overlap.
        assert!(
            second.offset() >= first.offset() + first.size()
                || second.offset() + second.size() <= first.offset()
        );

        // Both ranges are still reserved: releasing them returns the whole
        // pool to free space.
        pool.release(first.allocation());
        pool.release(second.allocation());
        assert_eq!(pool.free_space(), pool.size() as u32);
    }

    #[test]
    fn growing_replaces_the_buffer_and_keeps_the_ranges() {
        // A caller syncing a pool into a resource graph detects a grow by
        // comparing the buffer it holds against the pool's, so a grow has to
        // hand out a buffer that compares unequal to the old one.
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 64);

        let before = pool.buffer().clone();
        let first = pool
            .allocate(&device, &queue, 32)
            .expect("the pool has room");
        assert_eq!(pool.buffer(), &before, "allocating replaces nothing");

        pool.allocate(&device, &queue, 200).expect("the pool grows");
        assert_ne!(pool.buffer(), &before, "growing replaces the buffer");
        assert_eq!(first.offset(), 0, "the first range did not move");
    }

    #[test]
    fn a_released_range_is_reused() {
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 4096);

        let first = pool
            .allocate(&device, &queue, 512)
            .expect("the pool has room");
        assert_eq!(first.offset(), 0);
        pool.release(first.allocation());

        let second = pool
            .allocate(&device, &queue, 512)
            .expect("the pool has room");
        assert_eq!(second.offset(), 0, "the freed range is handed out again");
    }

    #[test]
    fn a_range_larger_than_the_pool_grows_it_once() {
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 64);

        let range = pool
            .allocate(&device, &queue, 4096)
            .expect("the pool grows");
        assert!(range.size() >= 4096);
        assert!(
            pool.size() >= 4096,
            "the pool grew enough for the range that did not fit"
        );
        assert_eq!(range.offset(), 0);
    }

    #[test]
    fn releasing_twice_panics() {
        let (device, queue) = noop_device();
        let mut pool = pool(&device, 1024);
        let range = pool
            .allocate(&device, &queue, 16)
            .expect("the pool has room");
        pool.release(range.allocation());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.release(range.allocation());
        }));
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "COPY_DST")]
    fn a_pool_without_copy_dst_is_rejected() {
        let (device, _queue) = noop_device();
        BufferPool::new(&device, "test::pool", wgpu::BufferUsages::VERTEX, 1024);
    }
}
