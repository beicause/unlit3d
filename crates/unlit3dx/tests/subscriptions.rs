//! Integration tests for snapshot subscriptions: a world collects state
//! only while a reader asked for it, and each subscription is its own.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit3dx::prelude::*;

/// A stepped world with no tick of its own.
///
/// A stepped world is the deterministic one: nothing runs until the test
/// steps it, so the order of a subscription and the tick that serves it is
/// the test's to fix. The step publishes the subscriptions itself, so a
/// world whose tick does nothing still answers them.
fn subscribed_world(host: &mut World, capacity: usize) -> WorldHandle {
    spawn_stepped_world_with_init(
        host,
        "subscribed",
        capacity,
        |world: &mut World| {
            world.root_scope_mut().spawn(bundle![Marker(0)]);
        },
        |_world: &mut World, _control: &ThreadControl, (), _: f32| true,
    )
}

/// The component the subscriptions read.
struct Marker(u32);

/// Each subscription collects for its own reader: two subscribers on one
/// world see two different snapshots, and neither overwrites the other.
#[test]
fn several_subscribers_each_get_their_own_snapshot() {
    let mut host = World::new();
    let world = subscribed_world(&mut host, 8);
    let sender = world.sender.clone();
    let mut first = 0u32;
    let one = sender
        .watch(move |_| {
            first += 1;
            Some(first)
        })
        .expect("the world accepts a subscription");
    let mut second = 0u32;
    let two = sender
        .watch(move |_| {
            second += 10;
            Some(second)
        })
        .expect("the world accepts a subscription");

    step_stepped_world(&host, world.entity, FRAME_SECONDS);

    assert_eq!(one.latest().map(|value| *value), Some(1));
    assert_eq!(two.latest().map(|value| *value), Some(10));
}

/// A subscription made after the world has been running sees the state of
/// the next tick, not a replay of what it missed.
#[test]
fn a_late_subscriber_sees_the_state_of_the_next_tick() {
    let mut host = World::new();
    let world = subscribed_world(&mut host, 8);
    step_stepped_world(&host, world.entity, FRAME_SECONDS);

    let reader = world
        .sender
        .clone()
        .watch(|world: &World| {
            world
                .root_scope()
                .query::<&Marker>()
                .next()
                .map(|marker| marker.0)
        })
        .expect("the world accepts a subscription");
    assert_eq!(
        reader.latest(),
        None,
        "nothing was collected before it asked"
    );

    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    assert_eq!(reader.latest().map(|value| *value), Some(0));
}

/// Dropping the reader stops the world collecting for it: the subscription
/// is pruned on the next tick, and the collector never runs again.
#[test]
fn dropping_the_reader_prunes_the_subscription() {
    let mut host = World::new();
    let world = subscribed_world(&mut host, 8);
    let ran = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&ran);
    let reader = world
        .sender
        .clone()
        .watch(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(1u32)
        })
        .expect("the world accepts a subscription");
    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    assert_eq!(ran.load(Ordering::SeqCst), 1, "the collector ran once");

    drop(reader);
    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "a dropped reader must stop its collector"
    );

    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    let live =
        with_stepped_world(&host, world.entity, publish_collected).expect("the world is stepped");
    assert_eq!(live, 0, "the world carries no live subscription");
}

/// A collector that answers nothing leaves the newest snapshot in place, so
/// a world whose state has not changed does not wake its reader.
#[test]
fn a_collector_that_returns_nothing_leaves_the_newest_snapshot() {
    let mut host = World::new();
    let world = subscribed_world(&mut host, 8);
    let mut first = true;
    let reader = world
        .sender
        .clone()
        .watch(move |_| {
            let value = if first { Some(1u32) } else { None };
            first = false;
            value
        })
        .expect("the world accepts a subscription");

    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    step_stepped_world(&host, world.entity, FRAME_SECONDS);

    assert_eq!(reader.latest().map(|value| *value), Some(1));
}

/// A sub-world publishes what it collects when its host drains it, so a
/// world with no tick of its own still answers the subscriptions made of it.
#[test]
fn draining_a_sub_world_publishes_its_subscriptions() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 8);
    let reader = proxy
        .sender
        .clone()
        .watch(|world: &World| {
            world
                .root_scope()
                .query::<&Marker>()
                .next()
                .map(|marker| marker.0)
        })
        .expect("the sub-world accepts a subscription");
    assert_eq!(reader.latest(), None, "nothing ran before the drain");

    // The sub-world gets its marker from a job: a nested world runs nothing
    // until its host drains it.
    proxy
        .sender
        .send(Box::new(|world: &mut World| {
            world.root_scope_mut().spawn(bundle![Marker(0)]);
        }))
        .expect("the sub-world accepts a job");

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);
    assert_eq!(
        reader.latest().map(|value| *value),
        Some(0),
        "the drain published what the collector collected"
    );
}

/// A full command queue refuses the subscription instead of blocking, and
/// the reader it would have filled never sees a snapshot.
#[test]
fn a_full_queue_refuses_a_subscription() {
    let mut host = World::new();
    let world = subscribed_world(&mut host, 1);
    world
        .sender
        .send(Box::new(|_: &mut World| {}))
        .expect("the queue has room for one job");

    let refused = world.sender.clone().watch(|_| Some(1u32));

    assert_eq!(refused.err(), Some(SendError::Full));
}
