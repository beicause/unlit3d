//! Integration tests for the multi-world core: sub-worlds on the host
//! thread, threaded worlds on their own threads, and the way jobs and
//! snapshots cross between them.

use std::sync::Arc;
use std::time::Duration;

use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit3dx::prelude::*;

/// A threaded world that idles between ticks, for the tests to drive.
fn idle_host() -> (World, WorldHandle) {
    let mut host = World::new();
    let world = spawn_threaded_world(&mut host, "test", 64, |_, control| {
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    (host, world)
}

/// Waits until `probe` answers `true`, giving up after two seconds.
fn wait_for(mut probe: impl FnMut() -> bool) -> bool {
    for _ in 0..2_000 {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// Sub-world jobs drain in queue order.
#[test]
fn sub_world_jobs_drain_in_order() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    for step in 0..4 {
        let order = Arc::clone(&order);
        let _ = proxy.sender.send(Box::new(move |_: &mut World| {
            order.lock().expect("the order lock").push(step);
        }));
    }
    let last = Arc::new(std::sync::Mutex::new(None));
    let seen = Arc::clone(&last);
    let _ = proxy.sender.send(Box::new(move |_: &mut World| {
        *seen.lock().expect("the order lock") = Some("last");
    }));

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert_eq!(*order.lock().expect("the order lock"), vec![0, 1, 2, 3]);
    assert_eq!(*last.lock().expect("the order lock"), Some("last"));
}

/// A job that panics ends the world it ran in: the drain stops where the
/// panic was, and the world's state says why.
#[test]
fn a_panicking_job_ends_the_sub_world() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let _ = proxy.sender.send(Box::new(|_: &mut World| {
        panic!("this job cannot run");
    }));
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&ran);
    let _ = proxy.sender.send(Box::new(move |_: &mut World| {
        seen.lock().expect("the order lock").push(7u64);
    }));

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert!(
        ran.lock().expect("the order lock").is_empty(),
        "a job queued behind a panic must not run"
    );
    assert!(proxy.is_finished(), "the panic ended the sub-world");
    assert!(
        matches!(
            proxy.state.latest().as_deref(),
            Some(WorldState::Panicked(_))
        ),
        "the panic is the world's last state"
    );
    // The queue closed with the world: a later send is refused instead of
    // landing in a queue nothing will ever drain.
    assert_eq!(
        proxy.sender.send(Box::new(|_: &mut World| {})),
        Err(SendError::Closed)
    );
}

/// A collector is arbitrary code, so a world that has panicked runs none:
/// the panic stays in the world that raised it instead of reaching the host.
#[test]
fn a_panicking_job_does_not_run_the_collectors() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let collected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&collected);
    let reader = proxy
        .sender
        .watch(move |_world: &World| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(1u32)
        })
        .expect("the world accepts a subscription");
    let _ = proxy.sender.send(Box::new(|_: &mut World| {
        panic!("this job cannot run");
    }));

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert!(proxy.is_finished(), "the panic ended the sub-world");
    assert_eq!(
        collected.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a collector must not run after the world panicked"
    );
    assert!(reader.latest().is_none(), "nothing was collected");
}

/// Stopping a sub-world ends it on the next drain, and the queue closes with
/// it: the host is the one that drives it, so nothing else would ever stop
/// accepting jobs.
#[test]
fn stopping_a_sub_world_ends_it_and_refuses_jobs() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);
    proxy.stop();

    let ran = Arc::new(std::sync::Mutex::new(false));
    let seen = Arc::clone(&ran);
    let _ = proxy.sender.send(Box::new(move |_: &mut World| {
        *seen.lock().expect("the order lock") = true;
    }));

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert!(
        !*ran.lock().expect("the order lock"),
        "a stopped sub-world runs nothing"
    );
    assert!(proxy.is_finished(), "the stop ended the sub-world");
    assert_eq!(proxy.state.latest().as_deref(), Some(&WorldState::Stopped));
    assert_eq!(
        proxy.sender.send(Box::new(|_: &mut World| {})),
        Err(SendError::Closed)
    );
}

/// Dropping the entity that carries the sub-world ends it, so an owner that
/// tears its worlds down is not left with one that reports `Running`
/// forever.
#[test]
fn despawning_a_sub_world_ends_it() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    assert!(
        host.root_scope_mut().despawn(proxy.entity).is_ok(),
        "the proxy entity despawns"
    );

    assert!(proxy.is_finished(), "the sub-world ended with its entity");
    assert_eq!(proxy.state.latest().as_deref(), Some(&WorldState::Stopped));
}

/// Dropping the entity that carries a stepped world ends it, the way it ends
/// a sub-world: the state reads `Stopped` and the queue closes, so a sender
/// does not go on posting to a world no step will ever drive.
#[test]
fn despawning_a_stepped_world_ends_it() {
    let mut host = World::new();
    let world = spawn_stepped_world_with_init(&mut host, "stepped", 8, |_| (), |_, _, (), _| true);
    step_stepped_world(&host, world.entity, FRAME_SECONDS);
    assert!(!world.is_finished(), "the world is still running");

    assert!(
        host.root_scope_mut().despawn(world.entity).is_ok(),
        "the proxy entity despawns"
    );

    assert!(
        world.is_finished(),
        "the stepped world ended with its entity"
    );
    assert_eq!(world.state.latest().as_deref(), Some(&WorldState::Stopped));
    assert_eq!(
        world.sender.send(Box::new(|_: &mut World| {})),
        Err(SendError::Closed)
    );
}

/// Stopping a sub-world is not a panic, so the drain still collects once:
/// whatever the world's subscriptions had at the stop is the last state their
/// readers ever get.
#[test]
fn stopping_a_sub_world_publishes_its_last_state() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);
    let reader = proxy
        .sender
        .watch(|world: &World| {
            world
                .root_scope()
                .query::<&Count>()
                .next()
                .map(|count| count.0)
        })
        .expect("the world accepts a subscription");
    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    // The first drain installs the collector; the counter arrives afterwards,
    // so the last publish is the only one that can see it.
    drain_sub_world(&mut nested);
    assert!(reader.latest().is_none(), "nothing was there to collect");
    nested.root_scope_mut().spawn(bundle![Count(7)]);

    proxy.stop();
    drain_sub_world(&mut nested);

    assert!(proxy.is_finished(), "the stop ended the sub-world");
    assert_eq!(
        reader.latest().map(|count| *count),
        Some(7),
        "a stopped world publishes one last time"
    );
}

/// A stepped world that has ended closes its queue, so a send after the end
/// is refused rather than queued for a step that will never come.
#[test]
fn a_finished_stepped_world_refuses_jobs() {
    let mut host = World::new();
    let world = spawn_stepped_world_with_init(&mut host, "stepped", 8, |_| (), |_, _, (), _| false);
    step_stepped_world(&host, world.entity, FRAME_SECONDS);

    assert!(world.is_finished(), "the tick ended the world");
    assert_eq!(
        world.sender.send(Box::new(|_: &mut World| {})),
        Err(SendError::Closed)
    );
}

/// A threaded world's state persists between jobs: later jobs see what
/// earlier jobs left, which is what makes a world a world.
#[test]
fn threaded_world_state_persists_between_jobs() {
    let (_host, threaded) = idle_host();

    let _ = threaded.sender.send(Box::new(|world: &mut World| {
        world.root_scope_mut().spawn(bundle![Count(0)]);
    }));
    for _ in 0..5 {
        let _ = threaded.sender.send(Box::new(|world: &mut World| {
            world
                .root_scope()
                .query::<&mut Count>()
                .next()
                .expect("the counter")
                .0 += 1;
        }));
    }
    let counted = Snapshot::new();
    let reader = counted.reader();
    let publish = counted.clone();
    let _ = threaded.sender.send(Box::new(move |world: &mut World| {
        publish.publish(
            world
                .root_scope()
                .query::<&Count>()
                .next()
                .expect("the counter")
                .0,
        );
    }));

    // The newest snapshot is kept, so the answer stays readable.
    assert!(
        wait_for(|| reader.latest().is_some()),
        "the counting job never published"
    );
    assert_eq!(reader.latest().map(|count| *count), Some(5));
}

/// A world's tick returning `false` stops the thread, and jobs sent
/// afterwards are dropped quietly instead of panicking the sender.
#[test]
fn a_finished_world_drops_jobs_quietly() {
    let mut host = World::new();
    let world = spawn_threaded_world(&mut host, "test", 8, |_, _| false);

    assert!(
        wait_for(|| world.is_finished()),
        "the thread kept running after its tick declined"
    );
    assert_eq!(world.state.latest().as_deref(), Some(&WorldState::Stopped));
    // The channel's receiver dies with the thread, so the job is refused
    // instead of being queued for a world that will never run it.
    let refused = world.sender.send(Box::new(|_: &mut World| {
        panic!("a dropped job must not run");
    }));
    assert_eq!(refused, Err(SendError::Closed));
}

/// Dropping the proxy entity stops the world: once the thread has ended,
/// its command channel answers every send with a disconnect.
#[test]
fn despawning_the_proxy_stops_the_world() {
    let mut host = World::new();
    let world = spawn_threaded_world(&mut host, "test", 8, |_, control| {
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    let sender = world.sender.clone();
    assert!(
        host.root_scope_mut().despawn(world.entity).is_ok(),
        "the proxy entity despawns"
    );

    // The receiver lives inside the thread closure, so it drops only when
    // the thread does; until then the channel has room and accepts jobs.
    assert!(
        wait_for(|| sender.send(Box::new(|_: &mut World| {})).is_err()),
        "the thread kept accepting jobs after its proxy was dropped"
    );
}

/// A subscription hands a world's newest state to a reader on the host,
/// which sees the updates the thread collects.
///
/// The collector runs inside the world's own loop — the runner publishes at
/// the end of every turn — so a world publishes only while the reader that
/// asked for the snapshot is alive.
#[test]
fn subscriptions_flow_from_thread_to_host() {
    let mut host = World::new();
    // The world's own tick is what runs its subscribers: a world that never
    // publishes collects nothing, whatever it carries.
    let world = spawn_threaded_world(&mut host, "test", 8, |_world, control| {
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    // The collector owns its own counter: it runs inside the world's tick,
    // so it sees a fresh value on every one.
    let mut tick: u32 = 0;
    let reader = world
        .sender
        .watch(move |_world: &World| {
            tick += 1;
            Some(tick)
        })
        .expect("the world accepts a subscription");
    assert!(
        wait_for(|| reader.latest().is_some_and(|value| *value >= 3)),
        "the reader never saw the collected values"
    );
    assert!(
        host.root_scope().contains(world.entity),
        "the proxy lives on a host entity"
    );
}

/// The counter the persistence test shares with its threaded world.
struct Count(u32);
