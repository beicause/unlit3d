/*
 * A port of Sebastian Aaltonen's `OffsetAllocator`, taken from
 * <https://github.com/pcwalton/offset-allocator>.
 *
 * Copyright (c) 2023 Sebastian Aaltonen, Patrick Walton
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! A fast, hard-real-time sub-allocator over one contiguous range.
//!
//! An [`Allocator`] owns a single block of `size` units — units being whatever
//! the caller needs, such as bytes, vertices or indices — and hands out
//! contiguous sub-ranges of it. It knows nothing about the resource behind the
//! range, which is what makes it usable for packing many small ranges into one
//! GPU buffer: allocate the range, then write the data at `offset * unit_size`.
//!
//! The algorithm is the two-level segregated fit used by Sebastian Aaltonen's
//! `OffsetAllocator`: free ranges ("nodes") are filed into 256 bins whose
//! boundaries follow an 8-bit float distribution (a 3-bit mantissa and a 5-bit
//! exponent), so that the relative overhead of rounding to a bin stays under
//! 12.5% for every size class. Two bitfields — one per top-level bin, one per
//! leaf bin — make the search for the next fitting bin two `trailing_zeros`
//! instructions, so [`Allocator::allocate`] and [`Allocator::free`] are O(1)
//! and never allocate.
//!
//! Every allocation starts at a multiple of the alignment fixed at
//! construction, which is what a GPU buffer sub-allocation needs: the size
//! charged for an allocation is the requested size rounded up to that
//! alignment.
//!
//! # Known limitation
//!
//! A free range is filed under the bin of its *rounded-down* size, while an
//! allocation searches from the bin of its *rounded-up* size. A range whose
//! size is not exactly representable by a bin is therefore never found by an
//! allocation of exactly that size, even though it would fit — so
//! `Allocator::new(size).allocate(size)` fails for most `size` values. Size the
//! allocator with [`min_allocator_size`] (and, for aligned allocators, pass the
//! same alignment) to make the initial free range representable:
//!
//! ```
//! use wgpu_unlit_render::offset_allocator::{Allocator, min_allocator_size};
//! use core::num::NonZeroU32;
//!
//! let alignment = NonZeroU32::new(4).unwrap();
//! let mut allocator = Allocator::with_alignment(min_allocator_size(1000, alignment), alignment);
//! assert!(allocator.allocate(1000).is_some());
//! ```

use core::fmt::{Debug, Formatter, Result as FmtResult};
use core::num::NonZeroU32;

/// The number of top-level bins.
const NUM_TOP_BINS: usize = 32;

/// The number of leaf bins per top-level bin.
const BINS_PER_LEAF: usize = 8;

/// The number of leaf bins, i.e. of distinct size classes.
const NUM_LEAF_BINS: usize = NUM_TOP_BINS * BINS_PER_LEAF;

/// The bit position of a top-level bin index within a leaf bin index.
const TOP_BINS_INDEX_SHIFT: u32 = 3;

/// The mask of the leaf bin index within a leaf bin index.
const LEAF_BINS_INDEX_MASK: u32 = 7;

/// The number of nodes an allocator tracks unless the caller says otherwise.
///
/// This matches the default of the original C++ `OffsetAllocator`; it costs
/// roughly 3.5 MB of metadata, so an allocator that knows it holds fewer
/// ranges should use [`Allocator::with_max_nodes`].
const DEFAULT_MAX_NODES: u32 = 128 * 1024;

/// The smallest allocator size that can hold an allocation of `size` units at
/// the given `alignment`.
///
/// See the [module documentation](self#known-limitation) for why an allocator
/// must be sized this way rather than with `size` itself.
///
/// # Panics
///
/// Panics if `size` cannot be rounded up to `alignment` within a `u32`.
pub fn min_allocator_size(size: u32, alignment: NonZeroU32) -> u32 {
    let aligned = align_up(size, alignment.get()).expect("the aligned size does not fit in a u32");
    small_float::float_to_uint(small_float::uint_to_float_round_up(aligned))
}

/// Rounds `size` up to the next multiple of `alignment`.
///
/// Returns `None` if the rounded value does not fit in a `u32`.
fn align_up(size: u32, alignment: u32) -> Option<u32> {
    let rounded = size.checked_add(alignment - 1)? / alignment * alignment;
    Some(rounded)
}

/// An allocator that manages a single contiguous chunk of space and hands out
/// portions of it as requested.
///
/// Allocations never move, and every offset it returns is a multiple of the
/// alignment it was constructed with.
pub struct Allocator {
    /// The total size of the managed chunk, in units.
    size: u32,
    /// The alignment, in units, that every allocation starts at.
    alignment: u32,
    /// The maximum number of ranges — allocated or free — the allocator tracks.
    ///
    /// Every allocation needs at least one node, and the free space that
    /// remains after it needs one more, so this bounds the number of live
    /// allocations to `max_nodes - 1`.
    max_nodes: u32,
    /// The total size of the free ranges, in units.
    ///
    /// Fragmentation and rounding mean this is not always allocatable at once,
    /// but while it is non-zero a one-unit allocation is always possible.
    free_storage: u32,

    /// A bit per top-level bin, set when that bin holds at least one range.
    used_bins_top: u32,
    /// A bit per leaf bin of each top-level bin, set when that leaf bin holds
    /// at least one range.
    used_bins: [u8; NUM_TOP_BINS],
    /// The first node of each leaf bin's linked list of free ranges.
    bin_indices: [Option<NodeIndex>; NUM_LEAF_BINS],

    /// Every range, allocated or free, indexed by [`NodeIndex`].
    nodes: Vec<Node>,
    /// The indexes of the nodes that no range uses, as a stack.
    free_nodes: Vec<NodeIndex>,
    /// How many entries of `free_nodes` are part of the stack.
    num_free_nodes: u32,
}

/// A single allocation, handed out by [`Allocator::allocate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation {
    /// The location of this allocation within the managed chunk, in units.
    ///
    /// This is a multiple of the allocator's alignment.
    pub offset: u32,
    /// The node that tracks this allocation; only meaningful to the allocator.
    metadata: NodeIndex,
}

/// Provides a summary of the state of the allocator, including space remaining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageReport {
    /// The amount of free space left.
    pub total_free_space: u32,
    /// The maximum potential size of a single contiguous allocation.
    pub largest_free_region: u32,
}

/// A range of the managed chunk: either an allocation or the free space around
/// one.
#[derive(Clone, Copy, Default)]
struct Node {
    /// Where the range starts, in units.
    data_offset: u32,
    /// How large the range is, in units.
    data_size: u32,
    /// The previous node of the size bin's list, when the range is free.
    bin_list_prev: Option<NodeIndex>,
    /// The next node of the size bin's list, when the range is free.
    bin_list_next: Option<NodeIndex>,
    /// The range that precedes this one in the chunk, allocated or free.
    neighbor_prev: Option<NodeIndex>,
    /// The range that follows this one in the chunk, allocated or free.
    neighbor_next: Option<NodeIndex>,
    /// Whether this range is handed out.
    used: bool,
}

/// The index of a [`Node`].
///
/// The index is stored biased by one inside a [`NonZeroU32`], so that
/// `Option<NodeIndex>` stays four bytes wide without depending on `nonmax`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct NodeIndex(NonZeroU32);

impl NodeIndex {
    /// Wraps the zero-based `index` of a node.
    ///
    /// # Panics
    ///
    /// Panics if `index` is `u32::MAX`, which [`Allocator::with_max_nodes`]
    /// rules out.
    fn new(index: u32) -> Self {
        match NonZeroU32::new(index + 1) {
            Some(index) => Self(index),
            None => unreachable!("node indices are bounded by `Allocator::with_max_nodes`"),
        }
    }

    /// The zero-based index of the node.
    fn get(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

impl Debug for NodeIndex {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        self.get().fmt(f)
    }
}

impl Allocator {
    /// Creates an allocator over `size` units, with unit alignment and the
    /// default node limit.
    pub fn new(size: u32) -> Self {
        Self::with_max_nodes(size, DEFAULT_MAX_NODES)
    }

    /// Creates an allocator over `size` units where every allocation starts at
    /// a multiple of `alignment`, with the default node limit.
    pub fn with_alignment(size: u32, alignment: NonZeroU32) -> Self {
        Self::with_max_nodes_and_alignment(size, DEFAULT_MAX_NODES, alignment)
    }

    /// Creates an allocator over `size` units with unit alignment, tracking at
    /// most `max_nodes` ranges.
    ///
    /// # Panics
    ///
    /// Panics if `max_nodes` is zero or leaves no room for the node index bias.
    pub fn with_max_nodes(size: u32, max_nodes: u32) -> Self {
        Self::with_max_nodes_and_alignment(size, max_nodes, NonZeroU32::MIN)
    }

    /// Creates an allocator over `size` units where every allocation starts at
    /// a multiple of `alignment`, tracking at most `max_nodes` ranges.
    ///
    /// # Panics
    ///
    /// Panics if `max_nodes` is zero or leaves no room for the node index bias.
    pub fn with_max_nodes_and_alignment(size: u32, max_nodes: u32, alignment: NonZeroU32) -> Self {
        assert!(max_nodes > 0, "an allocator needs at least one node");
        assert!(
            max_nodes < u32::MAX,
            "an allocator cannot track u32::MAX nodes"
        );

        let mut this = Self {
            size,
            alignment: alignment.get(),
            max_nodes,
            free_storage: 0,
            used_bins_top: 0,
            used_bins: [0; NUM_TOP_BINS],
            bin_indices: [None; NUM_LEAF_BINS],
            nodes: Vec::new(),
            free_nodes: Vec::new(),
            num_free_nodes: 0,
        };
        this.reset();
        this
    }

    /// Clears out all allocations, returning the allocator to its initial state.
    pub fn reset(&mut self) {
        self.free_storage = 0;
        self.used_bins_top = 0;
        self.used_bins = [0; NUM_TOP_BINS];
        self.bin_indices = [None; NUM_LEAF_BINS];
        self.nodes = vec![Node::default(); self.max_nodes as usize];

        // The free list is a stack. Nodes go in in reverse order so that index
        // zero pops first.
        self.free_nodes = (0..self.max_nodes)
            .map(|i| NodeIndex::new(self.max_nodes - i - 1))
            .collect();
        self.num_free_nodes = self.max_nodes;

        // Start with the whole chunk as one free range; the algorithm splits
        // remainders off it as it allocates.
        self.insert_node_into_bin(self.size, 0);
    }

    /// Allocates a range of `size` units and returns its allocation.
    ///
    /// The range starts at a multiple of the allocator's alignment, and the
    /// size charged for it is `size` rounded up to that alignment; see
    /// [`Allocator::allocation_size`]. Returns `None` if no free range is large
    /// enough, or if the allocator has no node left to describe the allocation.
    pub fn allocate(&mut self, size: u32) -> Option<Allocation> {
        // Out of nodes?
        if self.num_free_nodes == 0 {
            return None;
        }

        // Every range starts at a multiple of the alignment, so an allocation
        // may need up to `alignment - 1` units of padding in front of it.
        let size = align_up(size, self.alignment)?;

        // Round up to the bin index that is guaranteed to fit the allocation:
        // the lowest bin whose ranges are all at least `size` units.
        let min_bin_index = small_float::uint_to_float_round_up(size);

        let min_top_bin_index = min_bin_index >> TOP_BINS_INDEX_SHIFT;
        let min_leaf_bin_index = min_bin_index & LEAF_BINS_INDEX_MASK;

        let mut top_bin_index = min_top_bin_index;
        let mut leaf_bin_index = None;

        // If the top-level bin exists, scan its leaf bins. This can fail (no
        // space).
        if (self.used_bins_top & (1 << top_bin_index)) != 0 {
            leaf_bin_index = find_lowest_bit_set_after(
                u32::from(self.used_bins[top_bin_index as usize]),
                min_leaf_bin_index,
            );
        }

        // If we didn't find space in the top-level bin, search the next occupied
        // top-level bin. Every leaf bin there fits the allocation, since the
        // top-level bin index was rounded up, so start the leaf search at bit 0.
        let leaf_bin_index = match leaf_bin_index {
            Some(leaf_bin_index) => leaf_bin_index,
            None => {
                top_bin_index =
                    find_lowest_bit_set_after(self.used_bins_top, min_top_bin_index + 1)?;

                // This search can't fail: the top-level bit is set, so at least
                // one leaf bit of that bin is set too.
                self.used_bins[top_bin_index as usize].trailing_zeros()
            }
        };

        let bin_index = (top_bin_index << TOP_BINS_INDEX_SHIFT) | leaf_bin_index;

        // Pop the first node of the bin. The bin's list head becomes the next
        // node.
        let node_index = self.bin_indices[bin_index as usize].unwrap();
        let node = &mut self.nodes[node_index.get()];
        let node_total_size = node.data_size;
        node.data_size = size;
        node.used = true;
        self.bin_indices[bin_index as usize] = node.bin_list_next;
        if let Some(bin_list_next) = node.bin_list_next {
            self.nodes[bin_list_next.get()].bin_list_prev = None;
        }
        self.free_storage -= node_total_size;

        // Bin empty?
        if self.bin_indices[bin_index as usize].is_none() {
            // Remove a leaf bin mask bit.
            self.used_bins[top_bin_index as usize] &= !(1 << leaf_bin_index);

            // All leaf bins empty?
            if self.used_bins[top_bin_index as usize] == 0 {
                // Remove a top-level bin mask bit.
                self.used_bins_top &= !(1 << top_bin_index);
            }
        }

        // Push the remainder back into a lower bin.
        let remainder_size = node_total_size - size;
        if remainder_size > 0 {
            let Node {
                data_offset,
                neighbor_next,
                ..
            } = self.nodes[node_index.get()];

            let new_node_index = self.insert_node_into_bin(remainder_size, data_offset + size);

            // Link the nodes next to each other so that we can merge them later
            // if both are free, and point the old next neighbor at the node in
            // the middle.
            let node = &mut self.nodes[node_index.get()];
            if let Some(neighbor_next) = node.neighbor_next {
                self.nodes[neighbor_next.get()].neighbor_prev = Some(new_node_index);
            }
            self.nodes[new_node_index.get()].neighbor_prev = Some(node_index);
            self.nodes[new_node_index.get()].neighbor_next = neighbor_next;
            self.nodes[node_index.get()].neighbor_next = Some(new_node_index);
        }

        let node = &self.nodes[node_index.get()];
        Some(Allocation {
            offset: node.data_offset,
            metadata: node_index,
        })
    }

    /// Frees an allocation, returning the data to the allocator.
    ///
    /// # Panics
    ///
    /// Panics if the allocation was already freed.
    pub fn free(&mut self, allocation: Allocation) {
        let node_index = allocation.metadata;

        // Merge with the neighbors, reading them before they are removed from
        // their bin.
        let Node {
            data_offset: mut offset,
            data_size: mut size,
            used,
            ..
        } = self.nodes[node_index.get()];

        // Double free check.
        assert!(used, "the allocation was already freed");

        if let Some(neighbor_prev) = self.nodes[node_index.get()].neighbor_prev {
            let prev_node = self.nodes[neighbor_prev.get()];
            if !prev_node.used {
                // Previous (contiguous) free node: change the offset to the
                // previous node's offset and sum the sizes.
                offset = prev_node.data_offset;
                size += prev_node.data_size;

                debug_assert_eq!(prev_node.neighbor_next, Some(node_index));
                self.nodes[node_index.get()].neighbor_prev = prev_node.neighbor_prev;

                // Remove the node from the bin's list and put it in the free
                // list.
                self.remove_node_from_bin(neighbor_prev);
            }
        }

        if let Some(neighbor_next) = self.nodes[node_index.get()].neighbor_next {
            let next_node = self.nodes[neighbor_next.get()];
            if !next_node.used {
                // Next (contiguous) free node: the offset stays the same and
                // the sizes are summed.
                size += next_node.data_size;

                debug_assert_eq!(next_node.neighbor_prev, Some(node_index));
                self.nodes[node_index.get()].neighbor_next = next_node.neighbor_next;

                // Remove the node from the bin's list and put it in the free
                // list.
                self.remove_node_from_bin(neighbor_next);
            }
        }

        let Node {
            neighbor_next,
            neighbor_prev,
            ..
        } = self.nodes[node_index.get()];

        // Put the node back into the free list. It is the one that
        // `insert_node_into_bin` pops below, so the combined free range reuses
        // its index.
        self.free_nodes[self.num_free_nodes as usize] = node_index;
        self.num_free_nodes += 1;

        // Insert the combined free range into a bin.
        let combined_node_index = self.insert_node_into_bin(size, offset);

        // Connect the neighbors to the new combined node.
        if let Some(neighbor_next) = neighbor_next {
            self.nodes[combined_node_index.get()].neighbor_next = Some(neighbor_next);
            self.nodes[neighbor_next.get()].neighbor_prev = Some(combined_node_index);
        }
        if let Some(neighbor_prev) = neighbor_prev {
            self.nodes[combined_node_index.get()].neighbor_prev = Some(neighbor_prev);
            self.nodes[neighbor_prev.get()].neighbor_next = Some(combined_node_index);
        }
    }

    /// Creates a free node and inserts it at the head of the bin for its size.
    ///
    /// The caller is responsible for linking the node into the neighbor list.
    fn insert_node_into_bin(&mut self, size: u32, data_offset: u32) -> NodeIndex {
        // Round down to a bin index so that every range in a bin can hold any
        // allocation that bin is searched for.
        let bin_index = small_float::uint_to_float_round_down(size);

        let top_bin_index = bin_index >> TOP_BINS_INDEX_SHIFT;
        let leaf_bin_index = bin_index & LEAF_BINS_INDEX_MASK;

        // Was the bin empty before?
        if self.bin_indices[bin_index as usize].is_none() {
            // Set the bin mask bits.
            self.used_bins[top_bin_index as usize] |= 1 << leaf_bin_index;
            self.used_bins_top |= 1 << top_bin_index;
        }

        // Take a node from the free list and insert it at the head of the bin's
        // list (its next node is the old head).
        let top_node_index = self.bin_indices[bin_index as usize];
        self.num_free_nodes -= 1;
        let node_index = self.free_nodes[self.num_free_nodes as usize];
        self.nodes[node_index.get()] = Node {
            data_offset,
            data_size: size,
            bin_list_next: top_node_index,
            ..Node::default()
        };
        if let Some(top_node_index) = top_node_index {
            self.nodes[top_node_index.get()].bin_list_prev = Some(node_index);
        }
        self.bin_indices[bin_index as usize] = Some(node_index);

        self.free_storage += size;
        node_index
    }

    /// Removes a node from its bin and puts it back into the free list.
    ///
    /// The caller is responsible for fixing up the neighbor list, and must read
    /// anything it still needs from the node *before* calling this: the node
    /// may be handed out again by the next [`Allocator::insert_node_into_bin`].
    fn remove_node_from_bin(&mut self, node_index: NodeIndex) {
        // Copy the node to work around the borrow checker.
        let node = self.nodes[node_index.get()];

        match node.bin_list_prev {
            Some(bin_list_prev) => {
                // Easy case: the node has a predecessor, so unlink it from the
                // middle of the list.
                self.nodes[bin_list_prev.get()].bin_list_next = node.bin_list_next;
                if let Some(bin_list_next) = node.bin_list_next {
                    self.nodes[bin_list_next.get()].bin_list_prev = node.bin_list_prev;
                }
            }
            None => {
                // Hard case: the node is the head of a bin, so find the bin.
                //
                // Round down to a bin index to stay consistent with
                // `insert_node_into_bin`.
                let bin_index = small_float::uint_to_float_round_down(node.data_size);

                let top_bin_index = bin_index >> TOP_BINS_INDEX_SHIFT;
                let leaf_bin_index = bin_index & LEAF_BINS_INDEX_MASK;

                self.bin_indices[bin_index as usize] = node.bin_list_next;
                if let Some(bin_list_next) = node.bin_list_next {
                    self.nodes[bin_list_next.get()].bin_list_prev = None;
                }

                // Bin empty?
                if self.bin_indices[bin_index as usize].is_none() {
                    // Remove a leaf bin mask bit.
                    self.used_bins[top_bin_index as usize] &= !(1 << leaf_bin_index);

                    // All leaf bins empty?
                    if self.used_bins[top_bin_index as usize] == 0 {
                        // Remove a top-level bin mask bit.
                        self.used_bins_top &= !(1 << top_bin_index);
                    }
                }
            }
        }

        // Put the node back into the free list.
        self.free_nodes[self.num_free_nodes as usize] = node_index;
        self.num_free_nodes += 1;

        self.free_storage -= node.data_size;
    }

    /// Returns the size an allocation reserved, in units.
    ///
    /// This is the size requested at allocation time rounded up to the
    /// allocator's alignment, which may be larger than what the caller asked
    /// for.
    pub fn allocation_size(&self, allocation: Allocation) -> u32 {
        self.nodes[allocation.metadata.get()].data_size
    }

    /// Returns a summary of the free space remaining, and of the largest
    /// allocation that can be made in one piece.
    pub fn storage_report(&self) -> StorageReport {
        let mut largest_free_region = 0;
        let mut free_storage = 0;

        // Out of nodes? Then no free space can be handed out.
        if self.num_free_nodes > 0 {
            free_storage = self.free_storage;
            if self.used_bins_top > 0 {
                let top_bin_index = 31 - self.used_bins_top.leading_zeros();
                let leaf_bin_index =
                    31 - u32::from(self.used_bins[top_bin_index as usize]).leading_zeros();
                largest_free_region = small_float::float_to_uint(
                    (top_bin_index << TOP_BINS_INDEX_SHIFT) | leaf_bin_index,
                );
                debug_assert!(free_storage >= largest_free_region);
            }
        }

        StorageReport {
            total_free_space: free_storage,
            largest_free_region,
        }
    }
}

impl Debug for Allocator {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let report = self.storage_report();
        f.debug_struct("Allocator")
            .field("size", &self.size)
            .field("alignment", &self.alignment)
            .field("max_nodes", &self.max_nodes)
            .field("total_free_space", &report.total_free_space)
            .field("largest_free_region", &report.largest_free_region)
            .field("free_nodes", &self.num_free_nodes)
            .finish()
    }
}

/// Returns the index of the lowest set bit of `bit_mask` at or after
/// `start_bit_index`, or `None` if there is none.
fn find_lowest_bit_set_after(bit_mask: u32, start_bit_index: u32) -> Option<u32> {
    debug_assert!(start_bit_index < 32);
    let mask_before_start_index = (1 << start_bit_index) - 1;
    let bits_after = bit_mask & !mask_before_start_index;
    if bits_after == 0 {
        None
    } else {
        Some(bits_after.trailing_zeros())
    }
}

/// The 8-bit float size distribution the bins follow.
mod small_float {
    /// The number of bits of the mantissa.
    pub const MANTISSA_BITS: u32 = 3;
    /// The value of the hidden high mantissa bit.
    pub const MANTISSA_VALUE: u32 = 1 << MANTISSA_BITS;
    /// The mask of the mantissa.
    pub const MANTISSA_MASK: u32 = MANTISSA_VALUE - 1;

    /// Returns the bin index of the lowest bin that can hold `size` units.
    ///
    /// Bin sizes follow a floating point (exponent + mantissa) distribution —
    /// a piecewise linear approximation of a logarithm — which keeps the
    /// average overhead of rounding to a bin the same for every size class.
    pub fn uint_to_float_round_up(size: u32) -> u32 {
        let mut exp = 0;
        let mut mantissa;

        if size < MANTISSA_VALUE {
            // Denorm: 0..(MANTISSA_VALUE-1)
            mantissa = size
        } else {
            // Normalized: the hidden high bit is always 1 and is not stored,
            // just like a float.
            let highest_set_bit = 31 - size.leading_zeros();
            let mantissa_start_bit = highest_set_bit - MANTISSA_BITS;
            exp = mantissa_start_bit + 1;
            mantissa = (size >> mantissa_start_bit) & MANTISSA_MASK;

            // Round up.
            let low_bits_mask = (1 << mantissa_start_bit) - 1;
            if (size & low_bits_mask) != 0 {
                mantissa += 1;
            }
        }

        // `+` rather than `|` lets a rounded-up mantissa overflow into the
        // exponent.
        (exp << MANTISSA_BITS) + mantissa
    }

    /// Returns the bin index of the highest bin that can hold `size` units.
    pub fn uint_to_float_round_down(size: u32) -> u32 {
        let mut exp = 0;
        let mantissa;

        if size < MANTISSA_VALUE {
            // Denorm: 0..(MANTISSA_VALUE-1)
            mantissa = size
        } else {
            // Normalized: the hidden high bit is always 1 and is not stored,
            // just like a float.
            let highest_set_bit = 31 - size.leading_zeros();
            let mantissa_start_bit = highest_set_bit - MANTISSA_BITS;
            exp = mantissa_start_bit + 1;
            mantissa = (size >> mantissa_start_bit) & MANTISSA_MASK;
        }

        (exp << MANTISSA_BITS) | mantissa
    }

    /// Returns the size that the bin with the given index represents.
    pub fn float_to_uint(float_value: u32) -> u32 {
        let exponent = float_value >> MANTISSA_BITS;
        let mantissa = float_value & MANTISSA_MASK;
        if exponent == 0 {
            mantissa
        } else {
            (mantissa | MANTISSA_VALUE) << (exponent - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use super::*;

    /// The alignment every allocation of a buffer sub-allocator needs.
    const COPY_ALIGNMENT: NonZeroU32 = NonZeroU32::new(wgpu::COPY_BUFFER_ALIGNMENT as u32).unwrap();

    #[test]
    fn small_float_uint_to_float() {
        // Denorms, exp=1 and exp=2 + mantissa = 0 are all precise.
        // NOTE: Assuming 8 values (3 bit) mantissa.
        // If this test fails, please change this assumption!
        let precise_number_count = 17;
        for i in 0..precise_number_count {
            let round_up = small_float::uint_to_float_round_up(i);
            let round_down = small_float::uint_to_float_round_down(i);
            assert_eq!(i, round_up);
            assert_eq!(i, round_down);
        }

        // Test some random picked numbers.
        struct NumberFloatUpDown {
            number: u32,
            up: u32,
            down: u32,
        }

        let test_data = [
            NumberFloatUpDown {
                number: 17,
                up: 17,
                down: 16,
            },
            NumberFloatUpDown {
                number: 118,
                up: 39,
                down: 38,
            },
            NumberFloatUpDown {
                number: 1024,
                up: 64,
                down: 64,
            },
            NumberFloatUpDown {
                number: 65536,
                up: 112,
                down: 112,
            },
            NumberFloatUpDown {
                number: 529445,
                up: 137,
                down: 136,
            },
            NumberFloatUpDown {
                number: 1048575,
                up: 144,
                down: 143,
            },
        ];

        for v in test_data {
            let round_up = small_float::uint_to_float_round_up(v.number);
            let round_down = small_float::uint_to_float_round_down(v.number);
            assert_eq!(round_up, v.up);
            assert_eq!(round_down, v.down);
        }
    }

    #[test]
    fn small_float_float_to_uint() {
        // Denorms, exp=1 and exp=2 + mantissa = 0 are all precise.
        // NOTE: Assuming 8 values (3 bit) mantissa.
        // If this test fails, please change this assumption!
        let precise_number_count = 17;
        for i in 0..precise_number_count {
            let v = small_float::float_to_uint(i);
            assert_eq!(i, v);
        }

        // Test that float->uint->float conversion is precise for all numbers.
        // NOTE: Test values < 240. 240->4G = overflows 32 bit integer.
        for i in 0..240 {
            let v = small_float::float_to_uint(i);
            let round_up = small_float::uint_to_float_round_up(v);
            let round_down = small_float::uint_to_float_round_down(v);
            assert_eq!(i, round_up);
            assert_eq!(i, round_down);
        }
    }

    #[test]
    fn basic_offset_allocator() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);
        let a = allocator.allocate(1337).unwrap();
        let offset: u32 = a.offset;
        assert_eq!(offset, 0);
        allocator.free(a);
    }

    #[test]
    fn allocate_offset_allocator_simple() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);

        // Free merges neighbor empty nodes. Next allocation should also have
        // offset = 0.
        let a = allocator.allocate(0).unwrap();
        assert_eq!(a.offset, 0);

        let b = allocator.allocate(1).unwrap();
        assert_eq!(b.offset, 0);

        let c = allocator.allocate(123).unwrap();
        assert_eq!(c.offset, 1);

        let d = allocator.allocate(1234).unwrap();
        assert_eq!(d.offset, 124);

        allocator.free(a);
        allocator.free(b);
        allocator.free(c);
        allocator.free(d);

        // End: Validate that allocator has no fragmentation left. Should be
        // 100% clean.
        let validate_all = allocator.allocate(1024 * 1024 * 256).unwrap();
        assert_eq!(validate_all.offset, 0);
        allocator.free(validate_all);
    }

    #[test]
    fn allocate_offset_allocator_merge_trivial() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);

        // Free merges neighbor empty nodes. Next allocation should also have
        // offset = 0.
        let a = allocator.allocate(1337).unwrap();
        assert_eq!(a.offset, 0);
        allocator.free(a);

        let b = allocator.allocate(1337).unwrap();
        assert_eq!(b.offset, 0);
        allocator.free(b);

        // End: Validate that allocator has no fragmentation left. Should be
        // 100% clean.
        let validate_all = allocator.allocate(1024 * 1024 * 256).unwrap();
        assert_eq!(validate_all.offset, 0);
        allocator.free(validate_all);
    }

    #[test]
    fn allocate_offset_allocator_reuse_trivial() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);

        // The allocator should reuse the node freed by A, since the allocation
        // C fits in the same bin (using a pow2 size to be sure).
        let a = allocator.allocate(1024).unwrap();
        assert_eq!(a.offset, 0);

        let b = allocator.allocate(3456).unwrap();
        assert_eq!(b.offset, 1024);

        allocator.free(a);

        let c = allocator.allocate(1024).unwrap();
        assert_eq!(c.offset, 0);

        allocator.free(c);
        allocator.free(b);

        // End: Validate that allocator has no fragmentation left. Should be
        // 100% clean.
        let validate_all = allocator.allocate(1024 * 1024 * 256).unwrap();
        assert_eq!(validate_all.offset, 0);
        allocator.free(validate_all);
    }

    #[test]
    fn allocate_offset_allocator_reuse_complex() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);

        // The allocator should not reuse the node freed by A, since the
        // allocation C doesn't fit in the same bin. However nodes D and E fit
        // there and should reuse the node from A.
        let a = allocator.allocate(1024).unwrap();
        assert_eq!(a.offset, 0);

        let b = allocator.allocate(3456).unwrap();
        assert_eq!(b.offset, 1024);

        allocator.free(a);

        let c = allocator.allocate(2345).unwrap();
        assert_eq!(c.offset, 1024 + 3456);

        let d = allocator.allocate(456).unwrap();
        assert_eq!(d.offset, 0);

        let e = allocator.allocate(512).unwrap();
        assert_eq!(e.offset, 456);

        let report = allocator.storage_report();
        assert_eq!(
            report.total_free_space,
            1024 * 1024 * 256 - 3456 - 2345 - 456 - 512
        );
        assert_ne!(report.largest_free_region, report.total_free_space);

        allocator.free(c);
        allocator.free(d);
        allocator.free(b);
        allocator.free(e);

        // End: Validate that allocator has no fragmentation left. Should be
        // 100% clean.
        let validate_all = allocator.allocate(1024 * 1024 * 256).unwrap();
        assert_eq!(validate_all.offset, 0);
        allocator.free(validate_all);
    }

    #[test]
    fn allocate_offset_allocator_zero_fragmentation() {
        let mut allocator = Allocator::new(1024 * 1024 * 256);

        // Allocate 256x 1MB. Should fit. Then free four random slots and
        // reallocate four slots. Plus free four contiguous slots and allocate a
        // 4x larger slot. All must be zero fragmentation!
        let mut allocations: [_; 256] = core::array::from_fn(|i| {
            let allocation = allocator.allocate(1024 * 1024).unwrap();
            assert_eq!(allocation.offset, i as u32 * 1024 * 1024);
            allocation
        });

        let report = allocator.storage_report();
        assert_eq!(report.total_free_space, 0);
        assert_eq!(report.largest_free_region, 0);

        // Free four random slots.
        allocator.free(allocations[243]);
        allocator.free(allocations[5]);
        allocator.free(allocations[123]);
        allocator.free(allocations[95]);

        // Free four contiguous slots (the allocator must merge).
        allocator.free(allocations[151]);
        allocator.free(allocations[152]);
        allocator.free(allocations[153]);
        allocator.free(allocations[154]);

        allocations[243] = allocator.allocate(1024 * 1024).unwrap();
        allocations[5] = allocator.allocate(1024 * 1024).unwrap();
        allocations[123] = allocator.allocate(1024 * 1024).unwrap();
        allocations[95] = allocator.allocate(1024 * 1024).unwrap();
        allocations[151] = allocator.allocate(1024 * 1024 * 4).unwrap(); // 4x larger

        for (i, allocation) in allocations.iter().enumerate() {
            if !(152..155).contains(&i) {
                allocator.free(*allocation);
            }
        }

        let report2 = allocator.storage_report();
        assert_eq!(report2.total_free_space, 1024 * 1024 * 256);
        assert_eq!(report2.largest_free_region, 1024 * 1024 * 256);

        // End: Validate that allocator has no fragmentation left. Should be
        // 100% clean.
        let validate_all = allocator.allocate(1024 * 1024 * 256).unwrap();
        assert_eq!(validate_all.offset, 0);
        allocator.free(validate_all);
    }

    #[test]
    fn ext_min_allocator_size() {
        // Randomly generated integers on a log distribution, σ = 10.
        static TEST_OBJECT_SIZES: [u32; 42] = [
            0, 1, 2, 3, 4, 5, 8, 17, 23, 36, 51, 68, 87, 151, 165, 167, 201, 223, 306, 346, 394,
            411, 806, 969, 1404, 1798, 2236, 4281, 4745, 13989, 21095, 26594, 27146, 29679, 144685,
            153878, 495127, 727999, 1377073, 9440387, 41994490, 68520116,
        ];

        for needed_object_size in TEST_OBJECT_SIZES {
            let allocator_size = min_allocator_size(needed_object_size, NonZeroU32::MIN);
            let mut allocator = Allocator::new(allocator_size);
            assert!(allocator.allocate(needed_object_size).is_some());
        }
    }

    #[test]
    fn aligned_allocations_are_contiguous_and_aligned() {
        let mut allocator = Allocator::with_alignment(1 << 20, COPY_ALIGNMENT);

        // Three units round up to four, so the second allocation starts right
        // after the padded first one, without a gap.
        let a = allocator.allocate(3).unwrap();
        assert_eq!(a.offset, 0);
        assert_eq!(allocator.allocation_size(a), 4);

        let b = allocator.allocate(5).unwrap();
        assert_eq!(b.offset, 4);
        assert_eq!(allocator.allocation_size(b), 8);

        let c = allocator.allocate(4).unwrap();
        assert_eq!(c.offset, 12);
        assert_eq!(allocator.allocation_size(c), 4);

        for allocation in [a, b, c] {
            assert_eq!(allocation.offset % COPY_ALIGNMENT.get(), 0);
        }

        // Freeing in a different order still merges everything back into one
        // aligned range.
        allocator.free(b);
        allocator.free(a);
        allocator.free(c);

        let report = allocator.storage_report();
        assert_eq!(report.total_free_space, 1 << 20);
        assert_eq!(report.largest_free_region, 1 << 20);
    }

    #[test]
    fn min_allocator_size_holds_aligned_objects() {
        // Randomly generated integers on a log distribution, σ = 10.
        static TEST_OBJECT_SIZES: [u32; 42] = [
            0, 1, 2, 3, 4, 5, 8, 17, 23, 36, 51, 68, 87, 151, 165, 167, 201, 223, 306, 346, 394,
            411, 806, 969, 1404, 1798, 2236, 4281, 4745, 13989, 21095, 26594, 27146, 29679, 144685,
            153878, 495127, 727999, 1377073, 9440387, 41994490, 68520116,
        ];

        for needed_object_size in TEST_OBJECT_SIZES {
            let allocator_size = min_allocator_size(needed_object_size, COPY_ALIGNMENT);
            let mut allocator = Allocator::with_alignment(allocator_size, COPY_ALIGNMENT);
            let allocation = allocator.allocate(needed_object_size).unwrap_or_else(|| {
                panic!("{needed_object_size} units must fit in {allocator_size}")
            });
            assert_eq!(allocation.offset % COPY_ALIGNMENT.get(), 0);
            assert!(allocator.allocation_size(allocation) >= needed_object_size);
        }
    }

    #[test]
    fn the_last_node_of_the_free_list_is_usable() {
        // Two nodes hold one one-unit allocation plus the free range left over.
        // (The port treated a free list with one node left as exhausted, which
        // wasted it.)
        let mut allocator = Allocator::with_max_nodes(2, 2);
        let allocation = allocator.allocate(1).unwrap();
        assert_eq!(allocation.offset, 0);
        assert!(allocator.allocate(1).is_none());
    }

    #[test]
    #[should_panic(expected = "already freed")]
    fn double_free_panics() {
        let mut allocator = Allocator::new(1024);
        let allocation = allocator.allocate(64).unwrap();
        allocator.free(allocation);
        allocator.free(allocation);
    }
}
