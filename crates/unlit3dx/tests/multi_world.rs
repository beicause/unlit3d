//! Integration tests for the multi-world core: sub-worlds on the host
//! thread, remote worlds on their own threads, and the way jobs and
//! snapshots cross between them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use unlit_ecs::World;
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

/// Sub-world jobs drain in queue order, and a request queued behind them
/// is answered by the same drain.
#[test]
fn sub_world_jobs_drain_in_order() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    for step in 0..4 {
        let order = Arc::clone(&order);
        let _ = proxy.send(Box::new(move |_: &mut World| {
            order.lock().expect("the order lock").push(step);
        }));
    }
    let third = proxy.request(Box::new(|_: &mut World| "third"));

    let mut nested = host
        .get_mut::<World>(proxy.entity())
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert_eq!(*order.lock().expect("the order lock"), vec![0, 1, 2, 3]);
    assert_eq!(third.poll(), Some("third"));
}

/// A job that panics fails only its own request: the drain catches the
/// panic, the request reports it, and the jobs queued behind it still run.
#[test]
fn a_panicking_job_fails_only_its_own_request() {
    let mut host = World::new();
    let proxy = spawn_sub_world(&mut host, "nested", 16);

    let doomed = proxy.request(Box::new(|_: &mut World| -> u64 {
        panic!("this job cannot run");
    }));
    let survivor = proxy.request(Box::new(|_: &mut World| 7u64));

    let mut nested = host
        .get_mut::<World>(proxy.entity())
        .expect("the nested world");
    drain_sub_world(&mut nested);

    assert!(doomed.dropped(), "the failed request reports the drop");
    assert_eq!(
        doomed.recv(),
        Err(RequestError::Dropped),
        "a request whose job panicked answers with the failure"
    );
    assert_eq!(survivor.poll(), Some(7), "the queue kept draining");
}

/// A remote world's state persists between jobs: later jobs see what
/// earlier jobs left, which is what makes a world a world.
#[test]
fn remote_world_state_persists_between_jobs() {
    let (_host, remote) = idle_host();

    let _ = remote.send(Box::new(|world: &mut World| {
        world.spawn((Count(0),));
    }));
    for _ in 0..5 {
        let _ = remote.send(Box::new(|world: &mut World| {
            world.query::<&mut Count>().next().expect("the counter").1.0 += 1;
        }));
    }
    let count = remote.request(Box::new(|world: &mut World| {
        world.query::<&Count>().next().expect("the counter").1.0
    }));

    // Polling consumes the answer, so keep the first one around.
    let mut answer = None;
    assert!(
        wait_for(|| {
            answer = count.poll();
            answer.is_some()
        }),
        "the counting request never answered"
    );
    assert_eq!(answer, Some(5));
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
    let refused = world.send(Box::new(|_: &mut World| {
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
    let sender = world.sender();
    assert!(host.despawn(world.entity()), "the proxy entity despawns");

    // The receiver lives inside the thread closure, so it drops only when
    // the thread does; until then the channel has room and accepts jobs.
    assert!(
        wait_for(|| sender.send(Box::new(|_: &mut World| {})).is_err()),
        "the thread kept accepting jobs after its proxy was dropped"
    );
}

/// A world's snapshot mailbox hands its newest state to a reader on the
/// host, which sees every update the thread publishes.
#[test]
fn snapshots_flow_from_thread_to_host() {
    let mut host = World::new();
    let (writer, reader) = snapshot_pair::<u32>();
    let world = spawn_remote_world(&mut host, "test", 8, move |_, control| {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        writer.publish(NEXT.fetch_add(1, Ordering::SeqCst) as u32);
        control.wait_timeout(Duration::from_millis(1));
        true
    });
    assert!(
        wait_for(|| reader.latest().is_some_and(|value| *value >= 3)),
        "the reader never saw the published values"
    );
    assert!(
        host.contains(world.entity()),
        "the proxy lives on a host entity"
    );
}

/// The counter the persistence test shares with its remote world.
struct Count(u32);
