use super::*;
use crate::job::SendErrorKind;
use crate::time::FIXED_FRAME_DELTA;
use std::time::Duration;
use unlit_ecs::bundle;

/// Jobs sent through the handle run, and what one leaves behind is
/// published to the sender's reader.
#[test]
fn sent_jobs_run_and_publish_back() {
    let mut host = World::new();
    let handle = spawn_threaded_world(&mut host, "test", 8, |_, control, _| {
        control.wait_timeout(Duration::from_millis(1));
        TickOutcome::Running
    })
    .expect("the world thread starts");
    handle
        .commands()
        .send(Box::new(|world: &mut World| {
            world.root_scope_mut().spawn(bundle![Count(0)]);
        }))
        .expect("the world accepts jobs");
    let counted = crate::channel::Snapshot::new();
    let reader = counted.reader();
    let publish = counted.clone();
    handle
        .commands()
        .send(Box::new(move |world: &mut World| {
            world
                .root_scope()
                .query::<&mut Count>()
                .iter()
                .next()
                .expect("the counter")
                .0 += 1;
            let mut count_rows = world.root_scope().query::<&Count>();

            let count = count_rows.iter().next().expect("the counter").0;
            publish.publish(count);
        }))
        .expect("the world accepts jobs");
    let mut answer = None;
    for _ in 0..2_000 {
        answer = reader.latest().map(|count| *count);
        if answer.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(answer, Some(1));
}

/// The setup runs before the first job, so a job finds what it built.
#[test]
fn init_runs_before_the_first_job() {
    let mut host = World::new();
    let handle = spawn_threaded_world_with_init(
        &mut host,
        "test",
        8,
        |world: &mut World| {
            world.root_scope_mut().spawn(bundle![Count(0)]);
        },
        |_, control, (), _| {
            control.wait_timeout(Duration::from_millis(1));
            TickOutcome::Running
        },
    )
    .expect("the world thread starts");
    let seeded = crate::channel::Snapshot::new();
    let reader = seeded.reader();
    let publish = seeded.clone();
    handle
        .commands()
        .send(Box::new(move |world: &mut World| {
            publish.publish(world.root_scope().query::<&Count>().iter().next().is_some());
        }))
        .expect("the world accepts jobs");
    let mut answered = None;
    for _ in 0..2_000 {
        answered = reader.latest().map(|found| *found);
        if answered.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(answered, Some(true), "init ran before the job");
}

/// A full queue fails the send instead of blocking the sender.
#[test]
fn a_full_queue_fails_the_send() {
    let mut host = World::new();
    let handle = spawn_stepped_world_with_init(
        &mut host,
        "test",
        1,
        |_| (),
        |_, _, (), _| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    let mut results = Vec::new();
    for _ in 0..64 {
        results.push(handle.commands().send(Box::new(|_| {})));
    }
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Err(error) if error.kind() == SendErrorKind::Full)),
        "a bounded queue must eventually report Full"
    );
}

/// A stepped world runs its jobs and its tick only when stepped.
#[test]
fn a_stepped_world_runs_on_its_owner_s_calls() {
    let mut host = World::new();
    let handle = spawn_stepped_world_with_init(
        &mut host,
        "stepped",
        8,
        |world: &mut World| {
            world.root_scope_mut().spawn(bundle![Count(0)]);
        },
        |world: &mut World, _control: &ThreadControl, (), _: FrameDelta| {
            world
                .root_scope()
                .query::<&mut Count>()
                .iter()
                .next()
                .expect("the counter")
                .0 += 1;
            TickOutcome::Running
        },
    )
    .expect("the stepped world starts");
    let count = |host: &World| {
        with_stepped_world(host, handle.entity(), |world| {
            world
                .root_scope()
                .query::<&Count>()
                .iter()
                .next()
                .expect("the counter")
                .0
        })
    };
    // Nothing has run yet: the host has not stepped the world.
    assert_eq!(count(&host), Ok(0));
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Ok(StepOutcome::Ran)
    );
    assert_eq!(count(&host), Ok(1));
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Ok(StepOutcome::Ran)
    );
    assert_eq!(count(&host), Ok(2));
}

/// Draining every stepped world in one pass must not hold a shared
/// claim on the proxy that the drain itself needs to write.
#[test]
fn draining_all_stepped_worlds_does_not_conflict_with_its_own_query() {
    let mut host = World::new();
    let first = spawn_stepped_world_with_init(
        &mut host,
        "first",
        8,
        |world: &mut World| {
            world.root_scope_mut().spawn(bundle![Count(0)]);
        },
        |_, _, (), _| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    let second = spawn_stepped_world_with_init(
        &mut host,
        "second",
        8,
        |world: &mut World| {
            world.root_scope_mut().spawn(bundle![Count(0)]);
        },
        |_, _, (), _| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    for handle in [&first, &second] {
        handle
            .commands()
            .send(Box::new(|world: &mut World| {
                world
                    .root_scope()
                    .query::<&mut Count>()
                    .iter()
                    .next()
                    .expect("the counter")
                    .0 += 1;
            }))
            .expect("the world accepts jobs");
    }
    // The single query inside the drain must not trip the borrow ledger.
    drain_all_stepped_worlds(&host);
    for handle in [&first, &second] {
        let count = with_stepped_world(&host, handle.entity(), |world| {
            world
                .root_scope()
                .query::<&Count>()
                .iter()
                .next()
                .expect("the counter")
                .0
        });
        assert_eq!(count, Ok(1), "the queued job ran");
    }
}

/// A world that failed while its host drained it is reported with the
/// cause it ended on, so a driver hands on what went wrong and not only
/// which world it was.
#[test]
fn a_drained_world_reports_why_it_failed() {
    let mut host = World::new();
    let handle = spawn_stepped_world_with_init(
        &mut host,
        "failing",
        8,
        |_| (),
        |_, _: &ThreadControl, (), _: FrameDelta| TickOutcome::Running,
    )
    .expect("the stepped world starts");
    handle
        .commands()
        .send(Box::new(|_: &mut World| panic!("the drained job died")))
        .expect("the world accepts jobs");

    let failed = drain_all_stepped_worlds(&host);
    let expected = WorldError::Panic {
        stage: WorldStage::Job,
        message: "the drained job died".into(),
    };
    assert_eq!(
        failed,
        vec![WorldFailure {
            name: "failing",
            error: expected,
        }]
    );
    assert_eq!(
        join_failures(&failed),
        "failing (the job panicked: the drained job died)"
    );
}

/// Several failures read as one clause, each world with its own cause.
#[test]
fn joined_failures_name_each_world_and_its_cause() {
    let failures = [
        WorldFailure {
            name: "physics",
            error: WorldError::Command {
                source: unlit_ecs::EntityAccessError::NotAlive {
                    entity: unlit_ecs::Entity::from_bits(7),
                },
            },
        },
        WorldFailure {
            name: "audio",
            error: WorldError::Step {
                source: StepError::NotStepped,
            },
        },
    ];
    let joined = join_failures(&failures);
    assert!(
        joined.starts_with("physics (a queued command failed"),
        "{joined}"
    );
    assert!(joined.contains(", audio ("), "{joined}");
}

/// A stepped world's tick can end the world.
#[test]
fn a_stepped_tick_ends_the_world() {
    let mut host = World::new();
    let handle = spawn_stepped_world_with_init(
        &mut host,
        "stepped",
        8,
        |_| (),
        |_, _: &ThreadControl, (), _: FrameDelta| TickOutcome::Finished,
    )
    .expect("the stepped world starts");
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Ok(StepOutcome::Finished)
    );
    assert!(handle.is_finished(), "a false tick ends a stepped world");
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Err(StepError::Unavailable),
        "a stopped world is reported the same way by every entry point",
    );
    assert_eq!(
        with_stepped_world(&host, handle.entity(), |_| panic!("world ended")),
        Err::<(), _>(StepError::Unavailable),
    );
    assert_eq!(
        with_stepped_world_mut(&host, handle.entity(), |_| panic!("world ended")),
        Err::<(), _>(StepError::Unavailable),
    );
}

/// An entity that names no world proxy is refused, not silently skipped.
#[test]
fn stepping_a_foreign_entity_fails_readably() {
    let mut host = World::new();
    let plain = host.root_scope_mut().spawn(bundle![Count(0)]);
    assert!(matches!(
        step_stepped_world(&host, plain, FrameDelta::ZERO),
        Err(StepError::NoWorld(_))
    ));
}

/// A world that drives itself on a thread is refused rather than
/// silently ignored, so a caller cannot mistake one mode for the other.
#[test]
fn stepping_a_threaded_world_is_refused() {
    let mut host = World::new();
    let handle = spawn_threaded_world(&mut host, "threaded", 8, |_, control, _| {
        control.wait_timeout(Duration::from_millis(1));
        TickOutcome::Running
    })
    .expect("the world thread starts");
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Err(StepError::NotStepped)
    );
    handle.stop();
}

/// A command a tick queues through the scope is committed by the driver:
/// the tick only holds a shared world, so spawning through the queue is
/// the way it builds anything, and what it built must be there after.
#[test]
fn a_stepped_tick_commits_its_queued_commands() {
    let mut host = World::new();
    let handle = spawn_stepped_world_with_init(
        &mut host,
        "stepped",
        8,
        |_| (),
        |world: &mut World, _control: &ThreadControl, (), _: FrameDelta| {
            world.root_scope().queue().spawn(bundle![Count(7)]);
            TickOutcome::Running
        },
    )
    .expect("the stepped world starts");
    assert_eq!(
        step_stepped_world(&host, handle.entity(), FIXED_FRAME_DELTA),
        Ok(StepOutcome::Ran)
    );
    let count = with_stepped_world(&host, handle.entity(), |world| {
        world
            .root_scope()
            .query::<&Count>()
            .iter()
            .next()
            .map(|count| count.0)
    });
    assert_eq!(count, Ok(Some(7)), "the tick's queued entity was committed");
}

/// A threaded world commits what its tick queued as well: both drivers
/// run the same turn, so neither leaves the queue for someone else.
#[test]
fn a_threaded_tick_commits_its_queued_commands() {
    let mut host = World::new();
    let mut first = true;
    let handle = spawn_threaded_world(&mut host, "threaded", 8, move |world, control, _| {
        if first {
            first = false;
            world.root_scope().queue().spawn(bundle![Count(3)]);
        }
        control.wait_timeout(Duration::from_millis(1));
        TickOutcome::Running
    })
    .expect("the world thread starts");
    let counted = crate::channel::Snapshot::new();
    let reader = counted.reader();
    let mut answer = None;
    for _ in 0..2_000 {
        let publish = counted.clone();
        handle
            .commands()
            .send(Box::new(move |world: &mut World| {
                let count = world
                    .root_scope()
                    .query::<&Count>()
                    .iter()
                    .next()
                    .map(|count| count.0);
                publish.publish(count);
            }))
            .expect("the world accepts jobs");
        std::thread::sleep(Duration::from_millis(1));
        if let Some(Some(count)) = reader.latest().map(|count| *count) {
            answer = Some(count);
            break;
        }
    }
    handle.stop();
    assert_eq!(answer, Some(3), "the tick's queued entity was committed");
}

/// The counter the tests share with their worlds.
struct Count(u32);
