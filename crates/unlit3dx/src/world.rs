//! Worlds the host reaches through a proxy component: one on its own
//! thread, or one the host steps itself.
//!
//! Both leave the same proxy component on the host entity, so the host's
//! world carries the same data either way. What differs is who drives the
//! world: a thread of its own, or the host's own frames.
//!
//! Whoever drives a world commits and publishes its turn: a turn drains the
//! queue, runs the tick, applies the commands the tick queued, and then
//! publishes what the world's collectors returned. [`crate::turn`] owns that
//! order for every driver, so a tick body collects nothing by hand and cannot
//! publish twice.

use async_channel::{Receiver, bounded};
use unlit_ecs::{Bundle, DynamicBundle, Entity, EntityAccessError, World, bundle};
use unlit3d::frame::FrameClock;

use crate::executor::LocalTasks;
use crate::handle::WorldHandle;
use crate::job::{Job, JobSender, SendError, SendErrorKind};
use crate::thread::{ThreadControl, ThreadError, ThreadStatus, WorldThread, stepped_control};
use crate::turn::{Link, close_inbox, drain_jobs, run_turn};

pub use crate::turn::{StepOutcome, WorldError, WorldStage};

/// The proxy component a spawned world leaves on the host.
///
/// It owns the world's state - a thread that drives it, or the world itself
/// when the host steps it - so despawning the entity ends the world. It is
/// an implementation detail: callers reach the world through the
/// [`WorldHandle`] the spawn returns, never through this component.
struct WorldProxy {
    /// What drives the world.
    kind: WorldKind,
}

/// What drives a spawned world.
enum WorldKind {
    /// A world on its own thread.
    Threaded {
        /// The thread, stopped and joined when the proxy drops.
        _thread: WorldThread,
    },
    /// A world the host steps with [`step_stepped_world`].
    Stepped(Box<SteppedWorld>),
}

/// The name a spawned world answers to.
///
/// Every spawn leaves this component on the host entity that carries the
/// world, so a bridge that only walks the host can name the worlds it
/// reports instead of numbering them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldName(pub &'static str);

/// The host-side view of a spawned world's command queue.
///
/// A spawned world is reached through the [`WorldHandle`] its spawn
/// returns, but that handle belongs to the caller. Every spawn also leaves
/// this component on the proxy entity, so a bridge that only has the host
/// world — the MCP transport, say — can reach the world the same way. A
/// job posted here is picked up by whatever drives the world: its own
/// thread, or the host's [`step_stepped_world`] calls.
#[derive(Clone)]
pub struct WorldSender {
    /// The queue the world's jobs are posted through.
    sender: JobSender,
    /// The liveness a sender checks before it posts.
    status: ThreadStatus,
}

impl WorldSender {
    pub(crate) fn new(sender: JobSender, status: ThreadStatus) -> Self {
        Self { sender, status }
    }

    /// Post a job to the world this component belongs to.
    ///
    /// A finished world is rejected before touching the queue, so callers do
    /// not enqueue work that cannot ever run.
    pub fn send(&self, job: Job) -> Result<(), SendError> {
        if self.status.is_finished() {
            return Err(SendError::new(SendErrorKind::Closed, job));
        }
        self.sender.send(job)
    }

    /// Whether the world has stopped.
    ///
    /// A job sent to a stopped world never runs, so a sender checks this
    /// first rather than waiting for an answer that cannot come.
    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }
}

/// The tick a world runs, with its one-time state already captured.
type Tick = Box<dyn FnMut(&mut World, &ThreadControl, f32) -> bool>;

/// A world the host steps: the world, its command queue and its tick.
///
/// The world lives here rather than in the caller's own struct, so a host
/// entity's data does not depend on whether the world has a thread.
struct SteppedWorld {
    /// The world itself.
    world: World,
    /// The jobs queued for it.
    inbox: Receiver<Job>,
    /// The pacing and stop signal its tick reads.
    control: ThreadControl,
    /// The state the handle reports.
    status: ThreadStatus,
    /// The tick, with its one-time state already captured.
    tick: Tick,
    /// The name the world logs under, and the one a bridge reports.
    name: &'static str,
}

impl Drop for SteppedWorld {
    fn drop(&mut self) {
        // Dropping the proxy entity tears the world down, and a world that is
        // gone says so: the state it leaves reads `Stopped`, and the queue
        // closes so a later send is refused instead of landing in a queue no
        // step will ever drain.
        self.status.finish();
        close_inbox(&self.inbox);
    }
}

/// How a world is driven: on its own thread, or by the host's frames.
///
/// A runner is a zero-sized choice made at the call site, so a bridge can
/// offer both a threaded and a stepped world without a feature or a target
/// check. The stepped runner needs no thread at all, so a target without
/// threads can still build and drive every world.
pub trait WorldRunner: 'static {
    /// Whether the runner drives the world on its own.
    ///
    /// A world whose runner drives itself can open devices and sleep between
    /// ticks; a stepped world does neither, and its owner does both.
    const SELF_DRIVEN: bool;

    /// Spawn a world driven this way.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadError::Spawn`] when the runner needs a thread and the
    /// operating system refuses to create one. A runner that needs no thread
    /// never fails.
    fn spawn<I, S, T>(
        host: &mut World,
        name: &'static str,
        command_capacity: usize,
        init: I,
        tick: T,
    ) -> Result<WorldHandle, ThreadError>
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static;
}

/// A world on its own thread.
pub struct Threaded;

/// A world the host steps, with no thread of its own.
pub struct Stepped;

impl WorldRunner for Threaded {
    const SELF_DRIVEN: bool = true;

    fn spawn<I, S, T>(
        host: &mut World,
        name: &'static str,
        command_capacity: usize,
        init: I,
        tick: T,
    ) -> Result<WorldHandle, ThreadError>
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static,
    {
        spawn_threaded_world_with_init(host, name, command_capacity, init, tick)
    }
}

impl WorldRunner for Stepped {
    const SELF_DRIVEN: bool = false;

    fn spawn<I, S, T>(
        host: &mut World,
        name: &'static str,
        command_capacity: usize,
        init: I,
        tick: T,
    ) -> Result<WorldHandle, ThreadError>
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static,
    {
        Ok(spawn_stepped_world_with_init(
            host,
            name,
            command_capacity,
            init,
            tick,
        ))
    }
}

/// Spawn a threaded world on a fresh thread, driven by `tick`.
///
/// The thread's loop runs a turn per iteration: the jobs queued for the world,
/// then `tick`, then the commit and the publish. A tick that returns `false`
/// ends the thread. A panic in a job, in the tick or in a collector is
/// contained, recorded as the world's state, and ends the thread. The handle
/// returned carries the handle every later job goes through, and dropping the
/// host entity it names stops the thread.
///
/// `tick` takes the whole seconds since the previous tick, so a tick body
/// can advance by the time that really passed rather than assume the
/// rate it asked to be woken at.
///
/// `name` labels the world for a bridge that lists the host's worlds; it
/// lands on the proxy entity as a [`WorldName`].
/// # Errors
///
/// Returns [`ThreadError::Spawn`] when the operating system refuses to create
/// the thread.
pub fn spawn_threaded_world<T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    mut tick: T,
) -> Result<WorldHandle, ThreadError>
where
    T: FnMut(&mut World, &ThreadControl, f32) -> bool + Send + 'static,
{
    spawn_threaded_world_with_init(
        host,
        name,
        command_capacity,
        |_| (),
        move |world, control, (), delta_seconds| tick(world, control, delta_seconds),
    )
}

/// Like [`spawn_threaded_world`], with a one-time setup step.
///
/// `init` runs on the world's thread before the first job is drained, so
/// a world can create the components its jobs assume exist - its physics
/// world, say - and hand any state they need to the tick. The state it
/// returns is passed to every tick.
/// # Errors
///
/// Returns [`ThreadError::Spawn`] when the operating system refuses to create
/// the thread.
pub fn spawn_threaded_world_with_init<I, S, T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    init: I,
    mut tick: T,
) -> Result<WorldHandle, ThreadError>
where
    I: FnOnce(&mut World) -> S + Send + 'static,
    S: 'static,
    T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static,
{
    let (queue, inbox) = bounded::<Job>(command_capacity);
    // The thread's loop runs the turn, so the queue and the status travel with
    // it: the driver owns what happens between two ticks.
    let thread = WorldThread::spawn_with(name, inbox, init, move |world, control, state| {
        tick(world, control, state, control.elapsed_seconds())
    })?;
    let status: ThreadStatus = thread.status();
    let sender = JobSender::new(queue);
    let port = WorldSender {
        sender: sender.clone(),
        status: status.clone(),
    };
    let entity = host.root_scope_mut().spawn(WorldEntity {
        kind: WorldKind::Threaded { _thread: thread },
        name,
        sender: port,
    });
    Ok(WorldHandle::with_status(entity, sender, status))
}

/// Spawn a world the host steps, with a one-time setup step.
///
/// The world and its command queue live in the proxy component on the host
/// entity, exactly as a threaded world's thread does, so the entity carries
/// the same data either way. Nothing here runs on its own: the host calls
/// [`step_stepped_world`] to drain the queue and run the tick once.
///
/// `init` runs immediately, against the fresh world, before the entity is
/// spawned, so a job sent right after the spawn already finds what it built.
///
/// `name` labels the world for a bridge that lists the host's worlds; it
/// lands on the proxy entity as a [`WorldName`].
pub fn spawn_stepped_world_with_init<I, S, T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    init: I,
    mut tick: T,
) -> WorldHandle
where
    I: FnOnce(&mut World) -> S + 'static,
    S: 'static,
    T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + 'static,
{
    let (queue, inbox) = bounded::<Job>(command_capacity);
    let mut world = World::new();
    // A world is born with its frame state, so a behaviour subscribed to the
    // first turn already reads a clock rather than finding none.
    world
        .root_scope_mut()
        .spawn_named("frame_clock", bundle![FrameClock::default()]);
    // The world's async tasks are its own from here on: the setup may spawn
    // one, and every step drives them.
    LocalTasks::install(&mut world);
    let mut state = init(&mut world);
    let (control, status) = stepped_control();
    let tick = Box::new(
        move |world: &mut World, control: &ThreadControl, delta_seconds: f32| {
            tick(world, control, &mut state, delta_seconds)
        },
    );
    let stepped = SteppedWorld {
        world,
        inbox,
        control,
        status: status.clone(),
        tick,
        name,
    };
    let sender = JobSender::new(queue);
    let port = WorldSender {
        sender: sender.clone(),
        status: status.clone(),
    };
    let entity = host.root_scope_mut().spawn(WorldEntity {
        kind: WorldKind::Stepped(Box::new(stepped)),
        name,
        sender: port,
    });
    WorldHandle::with_status(entity, sender, status)
}
/// The entity a spawned world lives on: what drives it, the name it answers
/// to, and the queue its jobs are posted through.
struct WorldEntity {
    /// What drives the world.
    kind: WorldKind,
    /// The name the world answers to.
    name: &'static str,
    /// The host-side view of its command queue.
    sender: WorldSender,
}

impl Bundle for WorldEntity {
    fn name(&self) -> &'static str {
        self.name
    }

    fn into_bundle(self) -> DynamicBundle {
        bundle![
            WorldProxy { kind: self.kind },
            WorldName(self.name),
            self.sender,
        ]
    }
}

/// Why a stepped world could not be driven through the entity that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StepError {
    /// The entity does not name a world proxy the host can reach.
    #[error("cannot reach the world proxy: {0}")]
    NoWorld(#[from] EntityAccessError),
    /// The entity names a threaded world, which drives itself.
    #[error("the world is threaded and drives itself")]
    NotStepped,
    /// The stepped world has stopped or is stopping and cannot be accessed.
    #[error("the stepped world is no longer available")]
    Unavailable,
}

/// Run one turn of a stepped world with `delta_seconds`.
///
/// The turn is the one every driver runs: the queued jobs, then the tick, then
/// the commit and the publish. A tick that returns `false` stops the world; a
/// panic, in a job, in the tick or in a collector, is contained, logged and
/// published as the world's state, and ends the world too. A world that has
/// already ended runs nothing, and answers [StepError::Unavailable] the way
/// [with_stepped_world] does: every entry point reports a stopped world the
/// same way, so a caller that switches between them cannot read one as alive.
///
/// # Errors
///
/// Returns [StepError::NoWorld] when `entity` names no reachable world proxy,
/// [StepError::NotStepped] when it names a threaded world, which drives itself
/// and must not be stepped from here, and [StepError::Unavailable] after the
/// stepped world has stopped or received a stop request.
pub fn step_stepped_world(
    host: &World,
    entity: Entity,
    delta_seconds: f32,
) -> Result<StepOutcome, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&mut WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &mut proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() || stepped.status.stop_requested() {
        return Err(StepError::Unavailable);
    }
    let SteppedWorld {
        world,
        inbox,
        control,
        status,
        tick,
        name,
    } = &mut **stepped;
    let link = Link {
        name,
        inbox,
        status,
    };
    Ok(run_turn(world, link, |world| {
        LocalTasks::pump(world);
        tick(world, control, delta_seconds)
    }))
}

/// A world that ended in failure while its host drained it.
///
/// The cause travels with the name, so a driver that gives up on a failed world
/// hands on what went wrong instead of only which world it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldFailure {
    /// The name the world was spawned under.
    pub name: &'static str,
    /// What ended the world.
    pub error: WorldError,
}

impl core::fmt::Display for WorldFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.name, self.error)
    }
}

/// Print the failures of several worlds as one clause.
///
/// A driver that gives up on a set of worlds reports all of them in one
/// message, so each world's name and cause read together.
#[must_use]
pub fn join_failures(failures: &[WorldFailure]) -> String {
    let mut out = String::new();
    for (index, failure) in failures.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("{failure}"));
    }
    out
}

/// Drain the queued jobs of every world the host steps.
///
/// This walks the proxies directly, so a caller that must answer a
/// command before its next frame does not need to gather entities into
/// a separate buffer first.
///
/// Returns the worlds that failed, including any that failed on an earlier
/// turn, with the cause each ended with. Their queues are closed and their
/// collectors no longer run.
pub fn drain_all_stepped_worlds(host: &World) -> Vec<WorldFailure> {
    let mut proxies = host.root_scope().query::<&mut WorldProxy>();
    let mut failed = Vec::new();
    for proxy in proxies.iter() {
        let WorldKind::Stepped(stepped) = &mut proxy.kind else {
            continue;
        };
        let SteppedWorld {
            world,
            inbox,
            status,
            name,
            ..
        } = &mut **stepped;
        let link = Link {
            name,
            inbox,
            status,
        };
        if let StepOutcome::Failed(error) = drain_jobs(world, link) {
            failed.push(WorldFailure { name, error });
        }
    }
    failed
}

/// Run `f` against a stepped world's world.
///
/// A host that supplies its own buffers - an offline run filling an audio
/// buffer frame by frame, say - reads the world through this instead of
/// through the world's jobs.
///
/// # Errors
///
/// Returns [StepError::NoWorld] when `entity` names no reachable world proxy,
/// [StepError::NotStepped] when it names a threaded world, and
/// [StepError::Unavailable] after the stepped world has stopped or received a
/// stop request.
pub fn with_stepped_world<R>(
    host: &World,
    entity: Entity,
    f: impl FnOnce(&World) -> R,
) -> Result<R, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() || stepped.status.stop_requested() {
        return Err(StepError::Unavailable);
    }
    Ok(f(&stepped.world))
}

/// Run `f` against a stepped world's world mutably.
///
/// The mutable twin of [with_stepped_world]: a caller that starts or stops a
/// recording on the world needs the world itself, not only its components.
///
/// # Errors
///
/// Returns [StepError::NoWorld] when `entity` names no reachable world proxy,
/// [StepError::NotStepped] when it names a threaded world, and
/// [StepError::Unavailable] after the stepped world has stopped or received a
/// stop request.
pub fn with_stepped_world_mut<R>(
    host: &World,
    entity: Entity,
    f: impl FnOnce(&mut World) -> R,
) -> Result<R, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&mut WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &mut proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() || stepped.status.stop_requested() {
        return Err(StepError::Unavailable);
    }
    Ok(f(&mut stepped.world))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_SECONDS;
    use crate::job::SendErrorKind;
    use std::time::Duration;
    use unlit_ecs::bundle;

    /// Jobs sent through the handle run, and what one leaves behind is
    /// published to the sender's reader.
    #[test]
    fn sent_jobs_run_and_publish_back() {
        let mut host = World::new();
        let handle = spawn_threaded_world(&mut host, "test", 8, |_, control, _| {
            control.wait_timeout(Duration::from_millis(1));
            true
        })
        .expect("the world thread starts");
        handle
            .sender
            .send(Box::new(|world: &mut World| {
                world.root_scope_mut().spawn(bundle![Count(0)]);
            }))
            .expect("the world accepts jobs");
        let counted = crate::channel::Snapshot::new();
        let reader = counted.reader();
        let publish = counted.clone();
        handle
            .sender
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
                true
            },
        )
        .expect("the world thread starts");
        let seeded = crate::channel::Snapshot::new();
        let reader = seeded.reader();
        let publish = seeded.clone();
        handle
            .sender
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
        let handle =
            spawn_stepped_world_with_init(&mut host, "test", 1, |_| (), |_, _, (), _| true);
        let mut results = Vec::new();
        for _ in 0..64 {
            results.push(handle.sender.send(Box::new(|_| {})));
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
            |world: &mut World, _control: &ThreadControl, (), _: f32| {
                world
                    .root_scope()
                    .query::<&mut Count>()
                    .iter()
                    .next()
                    .expect("the counter")
                    .0 += 1;
                true
            },
        );
        let count = |host: &World| {
            with_stepped_world(host, handle.entity, |world| {
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
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
            Ok(StepOutcome::Ran)
        );
        assert_eq!(count(&host), Ok(1));
        assert_eq!(
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
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
            |_, _, (), _| true,
        );
        let second = spawn_stepped_world_with_init(
            &mut host,
            "second",
            8,
            |world: &mut World| {
                world.root_scope_mut().spawn(bundle![Count(0)]);
            },
            |_, _, (), _| true,
        );
        for handle in [&first, &second] {
            handle
                .sender
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
            let count = with_stepped_world(&host, handle.entity, |world| {
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
            |_, _: &ThreadControl, (), _: f32| true,
        );
        handle
            .sender
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
            |_, _: &ThreadControl, (), _: f32| false,
        );
        assert_eq!(
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
            Ok(StepOutcome::Finished)
        );
        assert!(handle.is_finished(), "a false tick ends a stepped world");
        assert_eq!(
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
            Err(StepError::Unavailable),
            "a stopped world is reported the same way by every entry point",
        );
        assert_eq!(
            with_stepped_world(&host, handle.entity, |_| panic!("world ended")),
            Err::<(), _>(StepError::Unavailable),
        );
        assert_eq!(
            with_stepped_world_mut(&host, handle.entity, |_| panic!("world ended")),
            Err::<(), _>(StepError::Unavailable),
        );
    }

    /// An entity that names no world proxy is refused, not silently skipped.
    #[test]
    fn stepping_a_foreign_entity_fails_readably() {
        let mut host = World::new();
        let plain = host.root_scope_mut().spawn(bundle![Count(0)]);
        assert!(matches!(
            step_stepped_world(&host, plain, 0.0),
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
            true
        })
        .expect("the world thread starts");
        assert_eq!(
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
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
            |world: &mut World, _control: &ThreadControl, (), _: f32| {
                world.root_scope().queue().spawn(bundle![Count(7)]);
                true
            },
        );
        assert_eq!(
            step_stepped_world(&host, handle.entity, FRAME_SECONDS),
            Ok(StepOutcome::Ran)
        );
        let count = with_stepped_world(&host, handle.entity, |world| {
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
            true
        })
        .expect("the world thread starts");
        let counted = crate::channel::Snapshot::new();
        let reader = counted.reader();
        let mut answer = None;
        for _ in 0..2_000 {
            let publish = counted.clone();
            handle
                .sender
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
}
