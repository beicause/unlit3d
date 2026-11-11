use core::num::NonZeroU32;

// Property tests are the host suite's: `proptest`'s generator needs an RNG,
// and `rand`'s `wasm32` backend is a build configuration of its own.
#[cfg(not(target_arch = "wasm32"))]
use proptest::prelude::*;

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
        0, 1, 2, 3, 4, 5, 8, 17, 23, 36, 51, 68, 87, 151, 165, 167, 201, 223, 306, 346, 394, 411,
        806, 969, 1404, 1798, 2236, 4281, 4745, 13989, 21095, 26594, 27146, 29679, 144685, 153878,
        495127, 727999, 1377073, 9440387, 41994490, 68520116,
    ];

    for needed_object_size in TEST_OBJECT_SIZES {
        let allocator_size = min_allocator_size(needed_object_size, NonZeroU32::MIN);
        let mut allocator = Allocator::new(allocator_size);
        assert!(allocator.allocate(needed_object_size).is_some());
    }
}

#[test]
fn aligned_allocations_are_contiguous_and_aligned() {
    let mut allocator =
        Allocator::with_max_nodes_and_alignment(1 << 20, DEFAULT_MAX_NODES, COPY_ALIGNMENT);

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
        0, 1, 2, 3, 4, 5, 8, 17, 23, 36, 51, 68, 87, 151, 165, 167, 201, 223, 306, 346, 394, 411,
        806, 969, 1404, 1798, 2236, 4281, 4745, 13989, 21095, 26594, 27146, 29679, 144685, 153878,
        495127, 727999, 1377073, 9440387, 41994490, 68520116,
    ];

    for needed_object_size in TEST_OBJECT_SIZES {
        let allocator_size = min_allocator_size(needed_object_size, COPY_ALIGNMENT);
        let mut allocator = Allocator::with_max_nodes_and_alignment(
            allocator_size,
            DEFAULT_MAX_NODES,
            COPY_ALIGNMENT,
        );
        let allocation = allocator
            .allocate(needed_object_size)
            .unwrap_or_else(|| panic!("{needed_object_size} units must fit in {allocator_size}"));
        assert_eq!(allocation.offset % COPY_ALIGNMENT.get(), 0);
        assert!(allocator.allocation_size(allocation) >= needed_object_size);
    }
}

#[test]
fn the_last_node_of_the_free_list_is_usable() {
    // Two nodes hold one one-unit allocation plus the free range left over.
    // (The port treated a free list with one node left as exhausted, which
    // wasted it.)
    let mut allocator = Allocator::with_max_nodes_and_alignment(2, 2, NonZeroU32::MIN);
    let allocation = allocator.allocate(1).unwrap();
    assert_eq!(allocation.offset, 0);
    assert!(allocator.allocate(1).is_none());
}

#[test]
fn every_offset_stays_aligned_under_churn() {
    // A small deterministic PRNG, so a failure reproduces exactly.
    let mut seed = 0x9e37_79b9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };

    let alignment = COPY_ALIGNMENT;
    let mut allocator = Allocator::with_max_nodes_and_alignment(1 << 16, 4096, alignment);

    // Live allocations, paired with the size charged for them, so overlap
    // and alignment can be checked against each other.
    let mut live: Vec<(Allocation, u32)> = Vec::new();

    for _ in 0..10_000 {
        // Free roughly a third of the time, so the allocator churns.
        if !live.is_empty() && next() % 3 == 0 {
            let index = (next() as usize) % live.len();
            let (allocation, _) = live.swap_remove(index);
            allocator.free(allocation);
            continue;
        }

        let size = next() % 2000;
        let Some(allocation) = allocator.allocate(size) else {
            continue;
        };
        let charged = allocator.allocation_size(allocation);

        // The offset is aligned, and the charge covers the request.
        assert_eq!(allocation.offset % alignment.get(), 0);
        assert!(charged >= size);
        assert_eq!(charged % alignment.get(), 0);

        // The new range overlaps no live one.
        let end = allocation.offset + charged;
        for &(other, other_charged) in &live {
            let other_end = other.offset + other_charged;
            assert!(
                end <= other.offset || other_end <= allocation.offset,
                "range {}..{} overlaps {}..{}",
                allocation.offset,
                end,
                other.offset,
                other_end
            );
        }

        live.push((allocation, charged));
    }

    // Freeing everything must return the allocator to one whole region.
    for (allocation, _) in live {
        allocator.free(allocation);
    }
    let report = allocator.storage_report();
    assert_eq!(report.total_free_space, 1 << 16);
    assert_eq!(report.largest_free_region, 1 << 16);
}

#[test]
#[should_panic(expected = "already freed")]
fn double_free_panics() {
    let mut allocator = Allocator::new(1024);
    let allocation = allocator.allocate(64).unwrap();
    allocator.free(allocation);
    allocator.free(allocation);
}

#[test]
fn extend_keeps_existing_offsets_and_appends_at_the_end() {
    let mut allocator =
        Allocator::with_max_nodes_and_alignment(1024, DEFAULT_MAX_NODES, COPY_ALIGNMENT);
    assert_eq!(allocator.size(), 1024);

    // Fill the chunk exactly, so the only way to satisfy the allocation
    // below is the appended space.
    let a = allocator.allocate(512).unwrap();
    let b = allocator.allocate(512).unwrap();
    assert_eq!((a.offset, b.offset), (0, 512));

    assert!(allocator.extend(1024));
    assert_eq!(allocator.size(), 2048);

    // The old allocations did not move.
    assert_eq!(allocator.allocation_size(a), 512);
    assert_eq!(allocator.allocation_size(b), 512);

    let c = allocator.allocate(1024).unwrap();
    assert_eq!(
        c.offset, 1024,
        "the new range comes from the appended space"
    );

    // The appended range is contiguous with the old chunk: freeing
    // everything must merge back into one region of the grown size.
    allocator.free(a);
    allocator.free(b);
    allocator.free(c);
    let report = allocator.storage_report();
    assert_eq!(report.total_free_space, 2048);
    assert_eq!(report.largest_free_region, 2048);

    let whole = allocator.allocate(2048).unwrap();
    assert_eq!(whole.offset, 0);
}

#[test]
fn extend_merges_into_a_free_tail() {
    let mut allocator =
        Allocator::with_max_nodes_and_alignment(1024, DEFAULT_MAX_NODES, COPY_ALIGNMENT);

    // Leave the end of the chunk free, then grow it: the appended space is
    // contiguous with that free tail, so the two must end up as one range
    // rather than as neighbours only a later `free` could merge.
    let a = allocator.allocate(256).unwrap();
    assert!(allocator.extend(1024));

    let report = allocator.storage_report();
    assert_eq!(report.total_free_space, 2048 - 256);
    assert_eq!(report.largest_free_region, 2048 - 256);

    let tail = allocator.allocate(2048 - 256).unwrap();
    assert_eq!(tail.offset, 256);
    allocator.free(a);
    allocator.free(tail);

    let whole = allocator.allocate(2048).unwrap();
    assert_eq!(whole.offset, 0);
}

#[test]
fn extend_appends_after_an_allocated_tail() {
    let mut allocator = Allocator::with_max_nodes_and_alignment(2, 3, NonZeroU32::MIN);

    // The chunk ends in an allocation, so the appended space becomes a new
    // free range.
    let a = allocator.allocate(2).unwrap();
    assert_eq!(a.offset, 0);
    assert!(allocator.extend(64));
    assert_eq!(allocator.size(), 66);

    // The chunk now ends in free space, so a second extension grows that
    // node in place rather than appending a neighbour to it.
    assert!(allocator.extend(64));
    assert_eq!(allocator.size(), 130);

    // The whole appended space is one range: an allocation may take all of
    // it, which a pair of adjacent ranges could not offer.
    let b = allocator.allocate(128).unwrap();
    assert_eq!(b.offset, 2);

    allocator.free(a);
    allocator.free(b);
    assert_eq!(allocator.storage_report().total_free_space, 130);

    // The two allocations merged back across the extension boundary. Had
    // they stayed separate, the free space would be the same 2 + 128 units
    // but the 128-unit range would sit at offset 2; taking it from offset 0
    // is only possible because `free` merged across the boundary.
    let whole = allocator.allocate(128).unwrap();
    assert_eq!(whole.offset, 0);
}

#[cfg(not(target_arch = "wasm32"))]
proptest::proptest! {
    /// Whatever mix of allocations and frees it is given, the allocator keeps
    /// its invariants: every offset is a multiple of the alignment, every
    /// charge covers the request, no range overlaps a live one, and handing
    /// everything back returns the chunk as one region.
    #[test]
    fn churn_keeps_every_allocation_aligned_and_disjoint(
        operations in proptest::collection::vec((any::<bool>(), 1u32..4096), 0..128),
    ) {
        let alignment = COPY_ALIGNMENT;
        let chunk = 1u32 << 16;
        let mut allocator = Allocator::with_max_nodes_and_alignment(chunk, 4096, alignment);
        let mut live: Vec<(Allocation, u32)> = Vec::new();

        for (free, requested) in operations {
            if free && !live.is_empty() {
                let index = requested as usize % live.len();
                let (allocation, _) = live.swap_remove(index);
                allocator.free(allocation);
                continue;
            }

            let Some(allocation) = allocator.allocate(requested) else {
                // Space, not nodes, is what runs out here: the node limit is
                // far above the number of live ranges this test can reach.
                prop_assert!(
                    allocator.storage_report().largest_free_region < requested,
                    "no allocation of {} units although {} were free in one region",
                    requested,
                    allocator.storage_report().largest_free_region,
                );
                continue;
            };

            let charged = allocator.allocation_size(allocation);
            prop_assert_eq!(allocation.offset % alignment.get(), 0);
            prop_assert_eq!(charged % alignment.get(), 0);
            prop_assert!(charged >= requested, "{} charged for {}", charged, requested);

            let end = allocation.offset + charged;
            for &(other, other_charged) in &live {
                let other_end = other.offset + other_charged;
                prop_assert!(
                    end <= other.offset || other_end <= allocation.offset,
                    "range {}..{} overlaps {}..{}",
                    allocation.offset,
                    end,
                    other.offset,
                    other_end,
                );
            }
            live.push((allocation, charged));
        }

        for (allocation, _) in live {
            allocator.free(allocation);
        }
        let report = allocator.storage_report();
        prop_assert_eq!(report.total_free_space, chunk);
        prop_assert_eq!(report.largest_free_region, chunk);
    }

    /// A request no larger than the largest free region is always granted: the
    /// bin search never misses a range big enough for the rounded request.
    #[test]
    fn a_request_that_fits_the_largest_free_region_is_granted(
        requested in 1u32..4096,
        occupied in proptest::collection::vec(1u32..4096, 0..32),
    ) {
        let mut allocator = Allocator::with_max_nodes_and_alignment(1 << 18, 4096, COPY_ALIGNMENT);
        let mut live = Vec::new();
        for size in occupied {
            match allocator.allocate(size) {
                Some(allocation) => live.push(allocation),
                None => break,
            }
        }

        let largest = allocator.storage_report().largest_free_region;
        if largest >= requested {
            prop_assert!(
                allocator.allocate(requested).is_some(),
                "no allocation of {} units although {} were free in one region",
                requested,
                largest,
            );
        }

        for allocation in live {
            allocator.free(allocation);
        }
    }
}
