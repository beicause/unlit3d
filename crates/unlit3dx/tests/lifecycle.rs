//! The lifecycle the three world kinds share: who owns a world, who drives
//! it, and what waiting for its thread reports.
//!
//! Ownership and driving are two questions, so a world is described by both:
//! a world on its own thread is owned by that thread and drives itself, while
//! a sub-world and a stepped world are owned by their host and turned by the
//! caller. Only a world with a thread has a thread to reclaim, and that is
//! what a wait answers.

use std::time::Duration;

use unlit_ecs::World;
use unlit3dx::prelude::*;

/// A tick that idles until it is asked to stop, so a test controls when the
/// thread ends.
fn idle_tick(_: &mut World, control: &ThreadControl, _: FrameDelta) -> TickOutcome {
    control.wait_timeout(Duration::from_millis(1));
    TickOutcome::Running
}

/// Each spawn says who owns the world and who drives it, and the two answers
/// are independent: the thread a world runs on is the only one that drives
/// itself.
#[test]
fn each_spawn_reports_who_owns_and_drives_it() {
    let mut host = World::new();
    let threaded =
        spawn_threaded_world(&mut host, "threaded", 8, idle_tick).expect("the world thread starts");
    let stepped = spawn_stepped_world_with_init(
        &mut host,
        "stepped",
        8,
        |_: &mut World| (),
        |_, _: &ThreadControl, (), _: FrameDelta| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    let nested = spawn_sub_world(&mut host, "nested", 8);

    assert_eq!(
        (threaded.ownership(), threaded.driving()),
        (WorldOwnership::ThreadOwned, WorldDriving::Continuous),
        "a world on its own thread drives itself"
    );
    assert_eq!(
        (stepped.ownership(), stepped.driving()),
        (WorldOwnership::HostOwned, WorldDriving::Stepped),
        "a stepped world is the host's to turn"
    );
    assert_eq!(
        (nested.ownership(), nested.driving()),
        (WorldOwnership::HostOwned, WorldDriving::Stepped),
        "a sub-world is stepped by whoever drains it"
    );

    threaded.stop();
    assert_eq!(
        threaded.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined,
        "the thread ended when it was asked to"
    );
}

/// A world no thread drives has nothing to reclaim, and the wait says so
/// rather than reporting an end it never saw.
#[test]
fn a_world_no_thread_drives_has_nothing_to_join() {
    let mut host = World::new();
    let stepped = spawn_stepped_world_with_init(
        &mut host,
        "stepped",
        8,
        |_: &mut World| (),
        |_, _: &ThreadControl, (), _: FrameDelta| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    let nested = spawn_sub_world(&mut host, "nested", 8);

    assert_eq!(
        stepped.join_timeout(Duration::from_millis(1)),
        JoinOutcome::NoHandle,
        "a stepped world ends through its state, not through a wait"
    );
    assert_eq!(
        nested.join_timeout(Duration::from_millis(1)),
        JoinOutcome::NoHandle,
        "a sub-world ends through its state, not through a wait"
    );
}

/// A wait that ran out keeps the handle, so the next one waits for the same
/// thread instead of reporting a join that never happened; once it is
/// reclaimed, a further wait says the handle is spent.
#[test]
fn a_wait_that_gave_up_can_wait_again() {
    let mut host = World::new();
    let world = spawn_threaded_world(&mut host, "slow", 8, |_, control, _| {
        control.wait_timeout(Duration::from_millis(50));
        TickOutcome::Running
    })
    .expect("the world thread starts");

    assert_eq!(
        world.join_timeout(Duration::from_millis(1)),
        JoinOutcome::TimedOut,
        "a thread still running is not a thread that ended"
    );
    world.stop();
    assert_eq!(
        world.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Joined,
        "the same wait takes the thread it left behind"
    );
    assert_eq!(
        world.join_timeout(Duration::from_secs(2)),
        JoinOutcome::AlreadyJoined,
        "a reclaimed thread is not reclaimed twice"
    );
}

/// Giving the wait up is a decision the endpoint keeps: a later wait reports
/// it rather than blocking on a thread nobody will collect.
#[test]
fn a_detached_world_reports_no_wait() {
    let mut host = World::new();
    let world =
        spawn_threaded_world(&mut host, "detached", 8, idle_tick).expect("the world thread starts");

    world.detach();
    assert_eq!(
        world.join_timeout(Duration::from_secs(2)),
        JoinOutcome::Detached,
        "a given-up wait is not a join"
    );
    world.stop();
}

/// A stepped world whose setup panicked fails the spawn with that cause, and
/// nothing is left on the host to step: a half-built world is not one a
/// caller can drive.
#[test]
fn a_stepped_setup_that_panicked_reports_why_and_spawns_nothing() {
    let mut host = World::new();
    let before = host.root_scope().query::<&WorldName>().iter().count();

    let error = match spawn_stepped_world_with_init(
        &mut host,
        "broken",
        8,
        |_: &mut World| panic!("the setup failed"),
        |_, _: &ThreadControl, (), _: FrameDelta| TickOutcome::Running,
    ) {
        Ok(_) => panic!("a panicking setup fails the spawn"),
        Err(error) => error,
    };

    assert!(
        matches!(error, ThreadError::Setup { name: "broken", .. }),
        "the failure names the world it could not set up: {error}"
    );
    assert_eq!(
        host.root_scope().query::<&WorldName>().iter().count(),
        before,
        "a failed setup leaves no world behind"
    );
}
