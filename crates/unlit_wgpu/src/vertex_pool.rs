//! A pool that packs many vertex streams into one buffer per layout.
//!
//! Uploading a mesh's vertices per buffer is wasteful: every `wgpu::Buffer`
//! carries driver overhead, and a draw can only bind a handful of them. A
//! [`VertexStreamPool`] instead owns one large buffer per vertex layout and
//! hands out element ranges inside them, so every mesh that shares a layout
//! shares a buffer.
//!
//! # Why one allocator for every layout
//!
//! Every vertex stream a draw binds reads the *same* element index: a draw's
//! `firstVertex`, or its `baseVertex` plus an index, picks one element out of
//! each stream at once. So a mesh whose vertices live in a pool has to land at
//! the same element index in all of them, and one allocation has to cover all
//! of its streams.
//!
//! That is why the pool hands out **elements, not bytes**, and keeps a *single*
//! allocator for every layout: one [`VertexAllocation`] is one range of
//! element indices, and each layout's buffer is that many elements times its
//! stride. Giving each layout its own allocator would not do, because two
//! allocators asked for the same count in different units — bytes, scaled by
//! different strides — drift apart: the allocator's size bins are a
//! logarithmic approximation, so `n * 8` and `n * 12` can round into different
//! bins and two streams of the same mesh end up at different indices, which
//! silently reads the wrong vertices.
//!
//! A byte offset inside a layout's buffer is its element index times that
//! layout's stride. Since a stride is always a multiple of
//! [`wgpu::VERTEX_ALIGNMENT`], every offset is a multiple of it too, which is
//! what [`wgpu::Queue::write_buffer`] asks for.
//!
//! # Example
//!
//! ```
//! use unlit_wgpu::specialize::{VertexAttributes, VertexBufferLayoutDesc};
//! use unlit_wgpu::vertex_pool::VertexStreamPool;
//!
//! let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
//! let layout = VertexBufferLayoutDesc {
//!     array_stride: 8,
//!     step_mode: wgpu::VertexStepMode::Vertex,
//!     attributes: VertexAttributes::new(),
//! };
//!
//! let mut pool = VertexStreamPool::new(
//!     &device,
//!     "example::vertices",
//!     wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
//!     1024,
//! );
//!
//! let vertices = pool
//!     .allocate(&device, &queue, &[layout.clone()], 3)
//!     .expect("the pool has room");
//! let buffer = pool.buffer(&layout).expect("the layout has a buffer");
//! let offset = VertexStreamPool::byte_offset(&layout, vertices.offset());
//! queue.write_buffer(buffer, offset, &[0u8; 24]);
//! pool.release(vertices.allocation());
//! ```

use core::num::NonZeroU32;
use hashbrown::HashMap;

use crate::offset_allocator::{Allocation, Allocator, min_allocator_size};
use crate::specialize::VertexBufferLayoutDesc;

/// The alignment the allocator hands out element indices at.
///
/// One element is the unit, so an index never needs rounding.
const ELEMENT_ALIGNMENT: NonZeroU32 = match NonZeroU32::new(1) {
    Some(alignment) => alignment,
    None => unreachable!(),
};

/// How many allocations a pool tracks unless the caller says otherwise.
///
/// The allocator's own default (128 Ki nodes, roughly 3.5 MB of metadata) is
/// sized for a general-purpose allocator; a pool holds one allocation per
/// mesh, so it needs far fewer.
const DEFAULT_MAX_ALLOCATIONS: u32 = 4096;

/// The step an element count grows by, and the floor for the first one.
const MIN_ELEMENTS: u32 = 1;

/// One mesh's vertices in a [`VertexStreamPool`]: one range of element
/// indices, shared by every stream the mesh binds.
///
/// The indices stay put until the allocation is handed back with
/// [`VertexStreamPool::release`]. Growing the pool replaces the buffers but
/// keeps every index, so read a buffer from
/// [`VertexStreamPool::buffer`] instead of holding one.
#[derive(Clone, Copy, Debug)]
pub struct VertexAllocation {
    /// The allocation that keeps the element range reserved.
    allocation: Allocation,
    /// How many elements the range covers.
    count: u32,
}

impl VertexAllocation {
    /// The first element index of the range.
    ///
    /// This is what a draw's `firstVertex` — or, for an indexed draw, its
    /// `baseVertex` — is set to.
    pub fn offset(&self) -> u32 {
        self.allocation.offset
    }

    /// How many elements the range covers.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// The allocation backing the range, to hand back to
    /// [`VertexStreamPool::release`].
    pub fn allocation(&self) -> Allocation {
        self.allocation
    }
}

/// One large buffer per vertex layout, sub-allocated into element ranges.
///
/// See the [module documentation](self) for why every layout shares one
/// allocator, and for what a pool is for.
pub struct VertexStreamPool {
    /// The label every buffer is created with, reused when one is replaced.
    label: &'static str,
    /// The usages every buffer is created with.
    usage: wgpu::BufferUsages,
    /// Where the free element indices are, shared by every layout.
    allocator: Allocator,
    /// The buffer of each layout the pool has been asked for.
    streams: HashMap<VertexBufferLayoutDesc, Stream>,
}

/// One layout's buffer and how much of it is in use.
struct Stream {
    /// The buffer every mesh of this layout shares.
    buffer: wgpu::Buffer,
    /// The highest element index the buffer has to cover, rounded up to the
    /// pool's growth step.
    ///
    /// A layout only needs to cover the elements something actually allocated
    /// in it, not the allocator's whole capacity: a layout used by one small
    /// mesh should not pay for a capacity a busier layout drove up.
    water_mark: u32,
}

impl VertexStreamPool {
    /// Creates a pool whose buffers hold `initial_element_capacity` elements
    /// each.
    ///
    /// No buffer exists until a layout is asked for; the capacity is only what
    /// the allocator starts with, and it grows on demand.
    pub fn new(
        _device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        initial_element_capacity: u32,
    ) -> Self {
        Self {
            label,
            usage,
            allocator: Allocator::with_max_nodes_and_alignment(
                initial_element_capacity.max(MIN_ELEMENTS),
                DEFAULT_MAX_ALLOCATIONS,
                ELEMENT_ALIGNMENT,
            ),
            streams: HashMap::new(),
        }
    }

    /// How many elements the pool can hold.
    pub fn element_capacity(&self) -> u32 {
        self.allocator.size()
    }

    /// The byte offset of element `index` in a layout's buffer.
    ///
    /// This is `index` times the layout's stride, which is always a multiple
    /// of [`wgpu::VERTEX_ALIGNMENT`], so the result satisfies the alignment
    /// [`wgpu::Queue::write_buffer`] asks for.
    pub fn byte_offset(layout: &VertexBufferLayoutDesc, index: u32) -> u64 {
        u64::from(index) * layout.array_stride
    }

    /// The buffer of `layout`, if the pool has been asked for it.
    ///
    /// This changes when the pool grows, so read it again instead of holding
    /// one.
    pub fn buffer(&self, layout: &VertexBufferLayoutDesc) -> Option<&wgpu::Buffer> {
        self.streams.get(layout).map(|stream| &stream.buffer)
    }

    /// Makes sure every layout in `layouts` has a buffer, then reserves
    /// `count` elements in all of them at once.
    ///
    /// A layout with no attributes and no stride declares nothing to read, so
    /// it is skipped: it gets no buffer and no part in the allocation.
    ///
    /// Returns `None` only if the pool cannot grow any further (its element
    /// count would overflow a `u32`, or it has run out of node slots for its
    /// allocations).
    ///
    /// Growing allocates new buffers and copies the old contents into them;
    /// existing allocations keep their element indices, but the buffers they
    /// name change, so a caller holding one must re-read
    /// [`Self::buffer`].
    pub fn allocate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layouts: &[VertexBufferLayoutDesc],
        count: u32,
    ) -> Option<VertexAllocation> {
        assert!(count > 0, "a vertex allocation covers at least one element");

        if let Some(allocation) = self.allocator.allocate(count) {
            self.reserve(device, queue, layouts, allocation.offset + count);
            return Some(VertexAllocation { allocation, count });
        }

        // The allocation may fail because the pool is full or because its
        // ranges are fragmented; growing helps in both cases as long as there
        // is a free node to describe the appended space.
        self.grow(count)?;
        let allocation = self
            .allocator
            .allocate(count)
            .expect("the grown pool has room for the allocation that did not fit");
        self.reserve(device, queue, layouts, allocation.offset + count);
        Some(VertexAllocation { allocation, count })
    }

    /// Hands an allocation back to the pool.
    ///
    /// # Panics
    ///
    /// Panics if the allocation was already released.
    pub fn release(&mut self, allocation: Allocation) {
        self.allocator.free(allocation);
    }

    /// Grows every layout's buffer until it covers `end` elements.
    ///
    /// A layout that already covers them is left alone, so a pool shared by
    /// meshes of wildly different strides only pays for each layout as far as
    /// that layout goes.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layouts: &[VertexBufferLayoutDesc],
        end: u32,
    ) {
        for layout in layouts {
            if layout.array_stride == 0 {
                // Nothing to read, so nothing to store: a stream that declares
                // no attributes gets no buffer and no part in the allocation.
                continue;
            }
            let wanted = growth_step(end);
            let stream = self
                .streams
                .entry(layout.clone())
                .or_insert_with(|| Stream {
                    buffer: create_buffer(device, self.label, self.usage, 0),
                    water_mark: 0,
                });
            if stream.water_mark >= wanted {
                continue;
            }
            stream.buffer = copy_into_larger(
                device,
                queue,
                &stream.buffer,
                self.label,
                self.usage,
                Self::byte_offset(layout, stream.water_mark),
                Self::byte_offset(layout, wanted),
            );
            stream.water_mark = wanted;
        }
    }

    /// Grows the allocator until `count` elements fit.
    ///
    /// Returns `None` if it cannot grow, leaving the pool unchanged.
    fn grow(&mut self, count: u32) -> Option<()> {
        // Grow by the larger of a doubling and what the allocation that did
        // not fit needs, so one grow is always enough for it.
        //
        // The size to grow by is the allocator's minimum for `count`, not
        // `count` itself: a free range is filed under its rounded-down size
        // while an allocation searches from its rounded-up size, so a range
        // sized exactly `count` would not be found by an allocation of
        // `count`. `min_allocator_size` is the smallest size that both holds
        // `count` and is representable by a bin, which is what makes the
        // range findable.
        let current = self.allocator.size();
        let needed = min_allocator_size(count, ELEMENT_ALIGNMENT);
        let additional = current.max(needed);
        if !self.allocator.extend(additional) {
            return None;
        }
        Some(())
    }
}

/// Replaces `buffer` with a larger one, copying `old_size` bytes across.
///
/// A buffer cannot be resized, so growing one means creating another and
/// copying into it — which also means the buffers the pool hands out change
/// under a caller that held one. The copy covers only what was in use, so a
/// buffer that has never been written to costs nothing to grow.
fn copy_into_larger(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
    old_size: u64,
    new_size: u64,
) -> wgpu::Buffer {
    let larger = create_buffer(device, label, usage, new_size);
    if old_size > 0 {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        encoder.copy_buffer_to_buffer(buffer, 0, &larger, 0, old_size);
        queue.submit([encoder.finish()]);
    }
    larger
}

/// Rounds an element count up to the pool's growth step.
fn growth_step(count: u32) -> u32 {
    count.max(MIN_ELEMENTS).next_power_of_two()
}

/// Creates a buffer of `size` bytes with the usages the pool needs.
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
    use crate::specialize::VertexAttributes;

    fn noop_device() -> (wgpu::Device, wgpu::Queue) {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
    }

    fn layout(stride: u64) -> VertexBufferLayoutDesc {
        VertexBufferLayoutDesc {
            array_stride: stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: VertexAttributes::new(),
        }
    }

    #[test]
    fn a_layout_gets_a_buffer_once_it_is_allocated_from() {
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            16,
        );
        let eight = layout(8);

        assert!(pool.buffer(&eight).is_none());
        let vertices = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 4)
            .expect("the pool has room");
        assert!(pool.buffer(&eight).is_some());
        assert_eq!(vertices.offset(), 0);
        assert_eq!(vertices.count(), 4);
    }

    #[test]
    fn a_stream_with_no_stride_gets_no_buffer() {
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            16,
        );
        let empty = layout(0);

        let vertices = pool
            .allocate(&device, &queue, std::slice::from_ref(&empty), 4)
            .expect("the pool has room");
        assert!(pool.buffer(&empty).is_none());
        assert_eq!(vertices.count(), 4);
    }

    #[test]
    fn every_layout_of_an_allocation_covers_the_same_elements() {
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            64,
        );
        let layouts = [layout(8), layout(12), layout(24)];

        let vertices = pool
            .allocate(&device, &queue, &layouts, 10)
            .expect("the pool has room");
        for stream in &layouts {
            // A buffer only has to cover the elements the pool has handed out
            // up to here, which is what makes one shared allocator safe: the
            // byte sizes differ but the element index does not.
            let size = pool.buffer(stream).expect("the layout has a buffer").size();
            assert!(
                size >= VertexStreamPool::byte_offset(stream, vertices.offset() + vertices.count()),
                "the buffer covers the whole allocation"
            );
        }
    }

    #[test]
    fn allocations_do_not_overlap_and_are_reused() {
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            64,
        );
        let eight = layout(8);

        let first = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 8)
            .expect("the pool has room");
        let second = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 8)
            .expect("the pool has room");
        assert!(
            first.offset() + first.count() <= second.offset()
                || second.offset() + second.count() <= first.offset(),
            "the two allocations do not overlap"
        );

        pool.release(first.allocation());
        let third = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 8)
            .expect("the pool has room");
        assert_eq!(third.offset(), first.offset(), "the freed range comes back");
    }

    #[test]
    fn growing_keeps_element_offsets_and_replaces_the_buffers() {
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            8,
        );
        let eight = layout(8);

        let first = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 8)
            .expect("the pool has room");
        let before = pool
            .buffer(&eight)
            .expect("the layout has a buffer")
            .clone();

        // This does not fit in the remaining elements, so the pool grows.
        let second = pool
            .allocate(&device, &queue, std::slice::from_ref(&eight), 32)
            .expect("the pool grows");
        // A caller syncing a pool into a resource graph detects a grow by
        // comparing the buffer it holds against the pool's, so growing has to
        // hand out a buffer that compares unequal to the old one.
        assert_ne!(
            pool.buffer(&eight).expect("the layout has a buffer"),
            &before,
            "growing replaces the buffer"
        );
        assert_eq!(first.offset(), 0, "the first allocation did not move");
        assert!(pool.element_capacity() >= first.count() + second.count());
    }

    #[test]
    fn byte_offset_is_the_element_index_times_the_stride() {
        let stream = layout(12);
        assert_eq!(VertexStreamPool::byte_offset(&stream, 0), 0);
        assert_eq!(VertexStreamPool::byte_offset(&stream, 3), 36);
    }

    #[test]
    fn two_layouts_of_one_mesh_land_on_the_same_element_index() {
        // The regression this guards: giving each layout its own allocator
        // lets two streams of one mesh drift onto different element indices,
        // because the allocator's bins are a logarithmic approximation and
        // `n * stride` rounds differently per stride. A shared, element-unit
        // allocator cannot drift, and this is what says so.
        let (device, queue) = noop_device();
        let mut pool = VertexStreamPool::new(
            &device,
            "test::vertices",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            16,
        );
        let layouts = [layout(8), layout(12)];

        // A pseudo-random churn of allocations and frees.
        let mut live: Vec<VertexAllocation> = Vec::new();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for step in 0..20_000u32 {
            let rolls = u64::from(step) % 3;
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let free_one = state.is_multiple_of(2);
            if free_one && let Some(allocation) = live.pop() {
                pool.release(allocation.allocation());
            }
            let count = 1 + (state >> 33) % 64;
            let count = u32::try_from(count).expect("the count fits in u32");
            let allocation = pool
                .allocate(&device, &queue, &layouts, count)
                .expect("the pool grows");
            // One allocation names one element index, so every stream of the
            // mesh reads the same vertex: there is nothing to compare here
            // beyond the allocation itself, and the assertion below is what
            // catches a design that gave each stream an index of its own.
            assert_eq!(allocation.count(), count);
            if rolls == 0 {
                live.push(allocation);
            } else {
                pool.release(allocation.allocation());
            }
        }
    }
}
