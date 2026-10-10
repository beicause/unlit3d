//! A pool that packs many variable-size slices into one array.
//!
//! The arrays a frame reads are frame-wide: one [`Array`] per kind serves every
//! draw, and a draw names its slice inside it rather than binding a resource of
//! its own. That works when the slices are of a fixed size — the metadata
//! entries are — but a mesh's morph displacements are as long as the mesh has
//! vertices, so its slice has to be sub-allocated rather than indexed.
//!
//! An [`ArrayPool`] is that: an [`Array`] plus an [`Allocator`] over its
//! elements, handing out [`ArrayRange`]s. It keeps a CPU mirror of the array's
//! bytes, so writing a slice and publishing the array are separate steps —
//! [`ArrayPool::write`] fills the mirror, [`ArrayPool::upload`] copies it to
//! the resource. That is what lets several meshes' slices be written into the
//! one array before a single upload reaches the GPU.
//!
//! # Element granularity
//!
//! Ranges are counted in *elements*, the unit the array's [`ArrayHandle`]
//! indexes by, so a range's element offset is exactly what a shader adds to its
//! own base index. Elements are the smallest unit the pool deals in, so an
//! allocator over them needs no alignment beyond one element.
//!
//! # Growing
//!
//! The pool doubles rather than re-packs: an [`Array`] cannot be resized, so
//! growing replaces the resource and every range keeps its element offset. The
//! CPU mirror carries the live data across, which is why the caller has to
//! [`upload`](ArrayPool::upload) after an allocation that grew the pool.
//!
//! # Example
//!
//! ```rust
//! use unlit_wgpu::array_pool::ArrayPool;
//!
//! let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! // Elements of four bytes: one `f32` each.
//! let mut pool = ArrayPool::new(&device, "example::arrays", 4, 16, None);
//!
//! let range = pool.allocate(&device, 3).expect("the pool has room");
//! pool.write(range, &[1.0f32.to_le_bytes(), 2.0f32.to_le_bytes(), 3.0f32.to_le_bytes()]
//!     .concat());
//!
//! let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
//!     label: Some("example"),
//! });
//! pool.upload(&device, &mut encoder);
//! pool.release(range.allocation());
//! ```

use crate::offset_allocator::{Allocation, Allocator, min_allocator_size};
use crate::texel_array::{Array, ArrayHandle};
use core::num::NonZeroU32;

/// The alignment elements are handed out at.
///
/// One element is the unit, so an offset never needs rounding.
const ELEMENT_ALIGNMENT: NonZeroU32 = match NonZeroU32::new(1) {
    Some(alignment) => alignment,
    None => unreachable!(),
};

/// How many ranges a pool tracks unless the caller says otherwise.
///
/// The allocator's own default (128 Ki nodes, roughly 3.5 MB of metadata) is
/// sized for a general-purpose allocator; a pool holds one range per mesh part,
/// so it needs far fewer.
const DEFAULT_MAX_RANGES: u32 = 4096;

/// The smallest pool size, so the first grow has something to grow from.
const MIN_ELEMENTS: u32 = 1;

/// A range of elements inside an [`ArrayPool`]'s array.
///
/// The range stays valid — and its data stays put — until its
/// [`allocation`](Self::allocation) is handed back with [`ArrayPool::release`].
/// Growing the pool replaces the array but keeps every element offset, so read
/// the handle from [`ArrayPool::handle`] instead of holding one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayRange {
    /// The allocation that keeps the range reserved.
    allocation: Allocation,
    /// How many elements the range covers.
    count: u32,
}

impl ArrayRange {
    /// The first element index of the range.
    ///
    /// This is what a shader adds to its own base index to reach the slice.
    pub fn offset(&self) -> u32 {
        self.allocation.offset
    }

    /// How many elements the range covers.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// The allocation backing the range, to hand back to
    /// [`ArrayPool::release`].
    pub fn allocation(&self) -> Allocation {
        self.allocation
    }
}

/// One array sub-allocated into element ranges, with a CPU mirror of its bytes.
///
/// See the [module documentation](self) for what a pool is for and how growing
/// works.
pub struct ArrayPool {
    /// The array every range lives in.
    array: Array,
    /// The label the array is created with, reused when it is replaced.
    label: Option<String>,
    /// Where the free elements are.
    allocator: Allocator,
    /// The array's bytes, element for element, kept so a slice can be written
    /// without re-reading the GPU resource.
    data: Vec<u8>,
    /// Whether the mirror holds bytes the array has not received yet.
    dirty: bool,
    /// Whether the array was replaced since the last [`Self::upload`].
    ///
    /// A replacement is what a caller has to act on: every bind group built
    /// from the old handle is stale. Reporting it from the upload keeps the
    /// check and the copy in one place, so a caller cannot upload without
    /// learning that the array moved.
    replaced: bool,
}

impl ArrayPool {
    /// Create a pool whose array holds `initial_elements` elements of
    /// `element_size` bytes each.
    ///
    /// `max_dimension` is `Some` of the device's
    /// [`wgpu::Limits::max_texture_dimension_2d`] to hold the elements in a
    /// texture, or `None` to hold them in a storage buffer.
    ///
    /// # Panics
    ///
    /// Under the conditions [`Array::new`] panics.
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        element_size: u64,
        initial_elements: u32,
        max_dimension: Option<u32>,
    ) -> Self {
        let elements = initial_elements.max(MIN_ELEMENTS);
        let array = Array::new(
            device,
            Some(label),
            element_size,
            u64::from(elements),
            max_dimension,
        );
        // The array may round its capacity up — a texel array does, to keep a
        // whole number of elements per row — so the mirror follows the array
        // rather than the request.
        let capacity = array.capacity();
        Self {
            array,
            label: Some(label.to_owned()),
            allocator: Allocator::with_max_nodes_and_alignment(
                min_allocator_size(elements, ELEMENT_ALIGNMENT),
                DEFAULT_MAX_RANGES,
                ELEMENT_ALIGNMENT,
            ),
            data: vec![0; (capacity * element_size) as usize],
            dirty: false,
            replaced: false,
        }
    }

    /// The bindable handle to the pool's array.
    ///
    /// Cloned rather than borrowed so a caller can move it into the resource
    /// graph, which is what makes a replaced array mark the bind groups built
    /// from it dirty.
    pub fn handle(&self) -> ArrayHandle {
        self.array.handle()
    }

    /// The array itself, for a caller that needs more than the handle.
    pub fn array(&self) -> &Array {
        &self.array
    }

    /// How many elements the array can hold.
    pub fn capacity(&self) -> u64 {
        self.array.capacity()
    }

    /// Reserves `count` elements, growing the array if they do not fit.
    ///
    /// Returns `None` only if the pool cannot grow any further (its size would
    /// overflow a `u32`, or it has run out of node slots for its ranges).
    ///
    /// Growing replaces the array, so a caller that holds a handle has to read
    /// it again — and has to [`upload`](Self::upload) before the GPU sees the
    /// data the mirror holds.
    ///
    /// # Panics
    ///
    /// If `count` is zero, since a range covers at least one element.
    pub fn allocate(&mut self, device: &wgpu::Device, count: u32) -> Option<ArrayRange> {
        assert!(count > 0, "an array allocation covers at least one element");

        if let Some(allocation) = self.allocator.allocate(count) {
            self.reserve(device, allocation.offset + count);
            return Some(ArrayRange { allocation, count });
        }

        // The allocation may fail because the pool is full or because its
        // ranges are fragmented; growing helps in both cases as long as there
        // is a free node to describe the appended space.
        self.grow(device, count)?;
        let allocation = self
            .allocator
            .allocate(count)
            .expect("the grown pool has room for the range that did not fit");
        self.reserve(device, allocation.offset + count);
        Some(ArrayRange { allocation, count })
    }

    /// Hands a range's elements back to the pool.
    ///
    /// # Panics
    ///
    /// Panics if the allocation was already released.
    pub fn release(&mut self, allocation: Allocation) {
        self.allocator.free(allocation);
    }

    /// Write `bytes` — one element per `element_size` — into `range`.
    ///
    /// This only fills the CPU mirror; the GPU sees the data once
    /// [`upload`](Self::upload) runs.
    ///
    /// # Panics
    ///
    /// If `bytes` is not exactly `range`'s elements, or if the range does not
    /// belong to this pool.
    pub fn write(&mut self, range: ArrayRange, bytes: &[u8]) {
        let element_size = self.array.element_size();
        assert_eq!(
            bytes.len() as u64,
            u64::from(range.count) * element_size,
            "the bytes must be exactly the range's elements"
        );
        let start = u64::from(range.offset()) * element_size;
        let end = start + bytes.len() as u64;
        assert!(
            end <= self.data.len() as u64,
            "the range lies inside the array the pool allocated from"
        );
        self.data[start as usize..end as usize].copy_from_slice(bytes);
        self.dirty = true;
    }

    /// Copy the CPU mirror to the array, recording the copy into `encoder`.
    ///
    /// Returns whether the array had to be *replaced* since the caller last
    /// read its handle, which is what says every bind group built from the old
    /// one is stale. An upload of an unchanged pool does nothing.
    pub fn upload(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> bool {
        let replaced = core::mem::take(&mut self.replaced);
        if core::mem::take(&mut self.dirty) {
            self.array.upload(device, encoder, &self.data);
        }
        replaced
    }

    /// Grow the array until it covers `end` elements.
    ///
    /// The mirror is resized with it, so the data already written stays put.
    fn reserve(&mut self, device: &wgpu::Device, end: u32) {
        let end = u64::from(end);
        if end <= self.array.capacity() {
            return;
        }
        let wanted = growth_step(self.array.capacity() as u32, end as u32);
        if self
            .array
            .grow_to(device, self.label.as_deref(), u64::from(wanted))
        {
            self.data.resize(
                (self.array.capacity() * self.array.element_size()) as usize,
                0,
            );
            // The replacement has to reach the GPU before it is read, so the
            // whole mirror is rewritten; a grow that copied nothing would
            // otherwise leave the new resource empty.
            self.dirty = true;
            self.replaced = true;
        }
    }

    /// Grow the allocator until `count` elements fit.
    ///
    /// Returns `None` if it cannot grow, leaving the pool unchanged.
    fn grow(&mut self, device: &wgpu::Device, count: u32) -> Option<()> {
        // Grow by 1.5x and at least as much as the allocation that did not fit
        // needs, so one grow is always enough for it.
        //
        // The size to grow by is the allocator's minimum for `count`, not
        // `count` itself: a free range is filed under its rounded-down size
        // while an allocation searches from its rounded-up size, so a range
        // sized exactly `count` would not be found by an allocation of `count`.
        let current = self.allocator.size();
        let needed = min_allocator_size(count, ELEMENT_ALIGNMENT);
        let additional = needed.max(current / 2);
        let target = current.checked_add(additional)?;
        if !self.allocator.extend(additional) {
            return None;
        }
        debug_assert_eq!(self.allocator.size(), target);
        self.reserve(device, target);
        Some(())
    }
}

impl core::fmt::Debug for ArrayPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArrayPool")
            .field("label", &self.label)
            .field("element_size", &self.array.element_size())
            .field("capacity", &self.capacity())
            .field("is_texel", &self.array.is_texel())
            .finish()
    }
}

/// The element count to grow `current` to so it covers `end`.
///
/// Growth is geometric (1.5x) so repeated growth does not reallocate every
/// time, and never rounds below the minimum the pool needs.
fn growth_step(current: u32, end: u32) -> u32 {
    end.max(current + current / 2).max(MIN_ELEMENTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_device() -> (wgpu::Device, wgpu::Queue) {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
    }

    fn pool(device: &wgpu::Device, elements: u32) -> ArrayPool {
        ArrayPool::new(device, "test::arrays", 4, elements, None)
    }

    #[test]
    fn ranges_are_distinct_and_a_released_one_comes_back() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 64);

        let first = pool.allocate(&device, 8).expect("the pool has room");
        let second = pool.allocate(&device, 8).expect("the pool has room");
        assert!(
            first.offset() + first.count() <= second.offset()
                || second.offset() + second.count() <= first.offset(),
            "the two ranges do not overlap"
        );

        pool.release(first.allocation());
        let third = pool.allocate(&device, 8).expect("the pool has room");
        assert_eq!(third.offset(), first.offset(), "the freed range comes back");
    }

    #[test]
    fn writing_a_range_fills_only_that_range() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 16);
        let first = pool.allocate(&device, 2).expect("the pool has room");
        let second = pool.allocate(&device, 2).expect("the pool has room");

        pool.write(first, &[1, 2, 3, 4, 5, 6, 7, 8]);
        pool.write(second, &[9, 10, 11, 12, 13, 14, 15, 16]);

        let first_start = first.offset() as usize * 4;
        assert_eq!(
            &pool.data[first_start..first_start + 8],
            &[1, 2, 3, 4, 5, 6, 7, 8]
        );
        let second_start = second.offset() as usize * 4;
        assert_eq!(
            &pool.data[second_start..second_start + 8],
            &[9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    #[should_panic(expected = "exactly the range's elements")]
    fn writing_the_wrong_length_panics() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 16);
        let range = pool.allocate(&device, 2).expect("the pool has room");
        pool.write(range, &[1, 2, 3, 4]);
    }

    #[test]
    fn growing_keeps_offsets_and_preserves_the_written_data() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 4);
        let first = pool.allocate(&device, 2).expect("the pool has room");
        pool.write(first, &[1, 2, 3, 4, 5, 6, 7, 8]);

        // This does not fit in the remaining elements, so the pool grows.
        let second = pool.allocate(&device, 32).expect("the pool grows");
        assert_eq!(first.offset(), 0, "the first range did not move");
        assert!(pool.capacity() >= u64::from(second.offset() + second.count()));
        let start = first.offset() as usize * 4;
        assert_eq!(
            &pool.data[start..start + 8],
            &[1, 2, 3, 4, 5, 6, 7, 8],
            "the data written before the grow survives it"
        );
    }

    #[test]
    fn an_allocation_reports_whether_the_array_was_replaced() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 4);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });

        let first = pool.allocate(&device, 2).expect("the pool has room");
        pool.write(first, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(
            !pool.upload(&device, &mut encoder),
            "an in-place write replaces nothing"
        );

        // A range that does not fit grows the array, which every bind group
        // built from the old handle has to learn about.
        pool.allocate(&device, 32).expect("the pool grows");
        assert!(
            pool.upload(&device, &mut encoder),
            "a grow replaces the array"
        );
        assert!(
            !pool.upload(&device, &mut encoder),
            "the replacement is reported once"
        );
    }

    #[test]
    fn an_upload_of_an_unchanged_pool_reports_nothing() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 4);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
        pool.allocate(&device, 2).expect("the pool has room");
        assert!(!pool.upload(&device, &mut encoder));
    }

    #[test]
    #[should_panic(expected = "at least one element")]
    fn a_zero_element_allocation_panics() {
        let (device, _queue) = noop_device();
        let mut pool = pool(&device, 4);
        pool.allocate(&device, 0);
    }
}
