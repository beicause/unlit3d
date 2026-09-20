//! The `Send + Sync` world: reading and writing across threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use unlit_ecs::{SendMode, SendWorld, Tasks};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Counter(u64);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Shared;

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn the_send_world_is_send_and_sync() {
    assert_send_sync::<SendWorld>();
    // A task table owns its futures, so it moves between threads; it is not
    // shared, so it needs `Send` but not `Sync`.
    assert_send::<Tasks<SendMode>>();
}

#[test]
fn several_threads_may_write_different_entities() {
    /// How many entities each worker gets.
    const PER_THREAD: u64 = 8;

    let mut world = SendWorld::new();
    // Entities are partitioned between threads: the cells are try-locked, so
    // two threads that touch the same component at once is a borrow conflict,
    // not a wait. Each thread owns one slice.
    let groups: Vec<Vec<unlit_ecs::Entity>> = (0..4)
        .map(|thread| {
            (0..PER_THREAD)
                .map(|offset| world.spawn((Counter(thread * PER_THREAD + offset),)))
                .collect()
        })
        .collect();

    let world_ref = &world;
    let groups_ref = &groups;
    std::thread::scope(|scope| {
        for group in groups_ref {
            scope.spawn(move || {
                for handle in group {
                    world_ref
                        .with_mut::<Counter, _>(*handle, |counter| counter.0 += 1)
                        .unwrap();
                }
            });
        }
    });

    for (thread, group) in groups.iter().enumerate() {
        for (offset, handle) in group.iter().enumerate() {
            let start = thread as u64 * PER_THREAD + offset as u64;
            assert_eq!(world.get::<Counter>(*handle).unwrap().0, start + 1);
        }
    }
}

#[test]
fn each_thread_owns_its_own_entities() {
    // The rule the world relies on: entities are partitioned between threads.
    let mut world = SendWorld::new();
    let per_thread: Vec<Vec<unlit_ecs::Entity>> = (0..4)
        .map(|thread| {
            (0..16u32)
                .map(|value| world.spawn((Counter((thread * 100 + value) as u64),)))
                .collect()
        })
        .collect();

    let world_ref = &world;
    let groups = &per_thread;
    std::thread::scope(|scope| {
        for group in groups {
            scope.spawn(move || {
                for handle in group {
                    world_ref
                        .with_mut::<Counter, _>(*handle, |counter| counter.0 += 1000)
                        .unwrap();
                }
            });
        }
    });

    for (thread, group) in per_thread.iter().enumerate() {
        for (offset, handle) in group.iter().enumerate() {
            let expected = (thread * 100 + offset) as u64 + 1000;
            assert_eq!(world.get::<Counter>(*handle).unwrap().0, expected);
        }
    }
}

#[test]
fn a_world_behind_an_arc_serves_concurrent_reads() {
    let world = Arc::new({
        let mut world = SendWorld::new();
        world.spawn((Counter(1), Shared));
        world.spawn((Counter(2), Shared));
        world
    });

    let total = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let world = Arc::clone(&world);
            let total = Arc::clone(&total);
            scope.spawn(move || {
                let sum: u64 = world
                    .query::<&Counter>()
                    .map(|(_, counter)| counter.0)
                    .sum();
                total.fetch_add(sum as usize, Ordering::Relaxed);
            });
        }
    });

    assert_eq!(total.load(Ordering::Relaxed), 4 * 3);
}

#[test]
fn a_send_world_can_be_moved_to_another_thread() {
    let mut world = SendWorld::new();
    let entity = world.spawn((Counter(5),));

    let handle = std::thread::spawn(move || {
        world
            .with_mut::<Counter, _>(entity, |counter| counter.0 *= 2)
            .unwrap();
        world
    });

    let world = handle.join().unwrap();
    assert_eq!(world.get::<Counter>(entity).unwrap().0, 10);
}

#[test]
fn a_command_queue_can_be_filled_on_another_thread() {
    let mut world = SendWorld::new();

    // Filling the queue from a worker and applying it on the main thread is
    // the split a `!Send` world cannot make: a `Send` command can cross the
    // thread boundary.
    let entity = std::thread::scope(|scope| {
        let world_ref = &world;
        scope
            .spawn(move || world_ref.queue().spawn((Counter(1), Shared)))
            .join()
            .unwrap()
    });

    assert!(!world.contains(entity), "queued, not applied");
    world.apply();
    assert!(world.contains(entity));
    assert_eq!(world.get::<Counter>(entity).unwrap().0, 1);
}
