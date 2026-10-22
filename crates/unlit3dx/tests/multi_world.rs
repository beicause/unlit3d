//! Integration tests for the multi-world core: sub-worlds on the host
//! thread, remote worlds on their own threads, and the way jobs and
//! snapshots cross between them.

use std::sync::Arc;
use std::time::Duration;

use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit3dx::prelude::*;

/// A remote world that idles between ticks, for the tests to drive.
fn idle_host() -> (World, WorldHandle) {
    let mut host = World::new();
    let world = spawn_remote_world(&mut host, "test", 64, |_, control| {
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
        let _ = proxy.commands.send(Box::new(move |_: &mut World| {
            order.lock().expect("the order lock").push(step);
        }));
    }
    let last = Arc::new(std::sync::Mutex::new(None));
    let seen = Arc::clone(&last);
    let _ = proxy.commands.send(Box::new(move |_: &mut World| {
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

/// A job that panics does not stop the jobs queued behind it: the drain
/// catches the panic and carries on.
#[test]
fn a_panicking_job_does_not_stop_the_drain() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let _ = proxy.commands.send(Box::new(|_: &mut World| {
        panic!("this job cannot run");
    }));
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&ran);
    let _ = proxy.commands.send(Box::new(move |_: &mut World| {
        seen.lock().expect("the order lock").push(7u64);
    }));

    let mut nested = host
        .root_scope()
        .query_one::<&mut World>(proxy.entity)
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert_eq!(*ran.lock().expect("the order lock"), vec![7]);
}

/// A remote world's state persists between jobs: later jobs see what
/// earlier jobs left, which is what makes a world a world.
#[test]
fn remote_world_state_persists_between_jobs() {
    let (_host, remote) = idle_host();

    let _ = remote.commands.send(Box::new(|world: &mut World| {
        world.root_scope_mut().spawn(bundle![Count(0)]);
    }));
    for _ in 0..5 {
        let _ = remote.commands.send(Box::new(|world: &mut World| {
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
    let _ = remote.commands.send(Box::new(move |world: &mut World| {
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
    let world = spawn_remote_world(&mut host, "test", 8, |_, _| false);

    assert!(
        wait_for(|| world.is_finished()),
        "the thread kept running after its tick declined"
    );
    assert!(world.panic_message().is_none());
    // The channel's receiver dies with the thread, so the job is refused
    // instead of being queued for a world that will never run it.
    let refused = world.commands.send(Box::new(|_: &mut World| {
        panic!("a dropped job must not run");
    }));
    assert_eq!(refused, Err(SendError::Closed));
}

/// Dropping the proxy entity stops the world: once the thread has ended,
/// its command channel answers every send with a disconnect.
#[test]
fn despawning_the_proxy_stops_the_world() {
    let mut host = World::new();
    let world = spawn_remote_world(&mut host, "test", 8, |_, control| {
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    let sender = world.commands.clone();
    assert!(
        host.root_scope_mut().despawn(world.entity),
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
/// The collector runs inside the world's own tick, so a world publishes only
/// while the reader that asked for the snapshot is alive.
#[test]
fn subscriptions_flow_from_thread_to_host() {
    let mut host = World::new();
    // The world's own tick is what runs its subscribers: a world that never
    // publishes collects nothing, whatever it carries.
    let world = spawn_remote_world(&mut host, "test", 8, |world, control| {
        publish_subscribed(world);
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    // The collector owns its own counter: it runs inside the world's tick,
    // so it sees a fresh value on every one.
    let mut tick: u32 = 0;
    let reader = world
        .commands
        .subscribe(move |_world: &World| {
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

/// The counter the persistence test shares with its remote world.
struct Count(u32);
