//! Worlds the host reaches through a proxy component: one on its own
//! thread, or one the host steps itself.
//!
//! Both leave the same proxy component on the host entity, so the host's
//! world carries the same data either way. What differs is who drives the
//! world: a thread of its own, or the host's own frames.

use async_channel::{Receiver, bounded};
use unlit_ecs::{ArchetypeBuilder, Entity, World};

use crate::executor::LocalTasks;
use crate::handle::WorldHandle;
use crate::job::{Job, JobSender, SendError};
use crate::thread::{ThreadControl, ThreadStatus, WorldThread, panic_message, stepped_control};

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
    commands: JobSender,
    /// The liveness a sender checks before it posts.
    status: ThreadStatus,
}

impl WorldSender {
    /// Post a job to the world this component belongs to.
    pub fn send(&self, job: Job<()>) -> Result<(), SendError> {
        self.commands.send(job)
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
type Tick = Box<dyn FnMut(&mut World, &ThreadControl) -> bool>;

/// A world the host steps: the world, its command queue and its tick.
///
/// The world lives here rather than in the caller's own struct, so a host
/// entity's data does not depend on whether the world has a thread.
struct SteppedWorld {
    /// The world itself.
    world: World,
    /// The commands queued for it.
    commands: Receiver<Job<()>>,
    /// The pacing and stop signal its tick reads.
    control: ThreadControl,
    /// The state the handle reports.
    status: ThreadStatus,
    /// The tick, with its one-time state already captured.
    tick: Tick,
    /// The name the world logs under, and the one a bridge reports.
    name: &'static str,
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
    fn spawn<I, S, T>(
        host: &mut World,
        name: &'static str,
        command_capacity: usize,
        init: I,
        tick: T,
    ) -> WorldHandle
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + Send + 'static;
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
    ) -> WorldHandle
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + Send + 'static,
    {
        spawn_remote_world_with_init(host, name, command_capacity, init, tick)
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
    ) -> WorldHandle
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + Send + 'static,
    {
        spawn_stepped_world_with_init(host, name, command_capacity, init, tick)
    }
}

/// Spawn a remote world on a fresh thread, driven by `tick`.
///
/// The thread's loop drains the jobs queued for the world, then runs
/// `tick` once; a tick that returns `false` ends the thread. A job that
/// panics is a tick panic: it is reported and ends the thread. The handle
/// returned carries the handle every later job goes through, and
/// dropping the host entity it names stops the thread.
///
/// `name` labels the world for a bridge that lists the host's worlds; it
/// lands on the proxy entity as a [`WorldName`].
pub fn spawn_remote_world<T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    mut tick: T,
) -> WorldHandle
where
    T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
{
    spawn_remote_world_with_init(
        host,
        name,
        command_capacity,
        |_| (),
        move |world, control, ()| tick(world, control),
    )
}

/// Like [`spawn_remote_world`], with a one-time setup step.
///
/// `init` runs on the world's thread before the first job is drained, so
/// a world can create the components its jobs assume exist - its physics
/// world, say - and hand any state they need to the tick. The state it
/// returns is passed to every tick.
///
/// # Panics
///
/// Panics when `command_capacity` is zero: a queue that can never hold
/// a job would silently block every sender.
pub fn spawn_remote_world_with_init<I, S, T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    init: I,
    mut tick: T,
) -> WorldHandle
where
    I: FnOnce(&mut World) -> S + Send + 'static,
    S: 'static,
    T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + Send + 'static,
{
    let (commands, receiver) = bounded::<Job<()>>(command_capacity);
    let thread = WorldThread::spawn_with(name, init, move |world, control, state| {
        while let Ok(job) = receiver.try_recv() {
            job(world);
        }
        tick(world, control, state)
    });
    let status: ThreadStatus = thread.status();
    let commands = JobSender::new(commands);
    let sender = WorldSender {
        commands: commands.clone(),
        status: status.clone(),
    };
    let entity = spawn_proxy(host, WorldKind::Threaded { _thread: thread }, name, sender);
    WorldHandle::new(entity, commands, Some(status))
}

/// Spawn a world the host steps, with a one-time setup step.
///
/// The world and its command queue live in the proxy component on the host
/// entity, exactly as a remote world's thread does, so the entity carries
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
    T: FnMut(&mut World, &ThreadControl, &mut S) -> bool + 'static,
{
    let (commands, receiver) = bounded::<Job<()>>(command_capacity);
    let mut world = World::new();
    // The world's async tasks are its own from here on: the setup may spawn
    // one, and every step drives them.
    LocalTasks::install(&mut world);
    let mut state = init(&mut world);
    let (control, status) = stepped_control();
    let tick = Box::new(move |world: &mut World, control: &ThreadControl| {
        tick(world, control, &mut state)
    });
    let stepped = SteppedWorld {
        world,
        commands: receiver,
        control,
        status: status.clone(),
        tick,
        name,
    };
    let sender = WorldSender {
        commands: JobSender::new(commands.clone()),
        status: status.clone(),
    };
    let entity = spawn_proxy(host, WorldKind::Stepped(Box::new(stepped)), name, sender);
    WorldHandle::new(entity, JobSender::new(commands), Some(status))
}

/// Spawn the proxy entity with the runner's components.
fn spawn_proxy(
    host: &mut World,
    kind: WorldKind,
    name: &'static str,
    sender: WorldSender,
) -> Entity {
    let mut bundle = ArchetypeBuilder::new();
    bundle.push(WorldProxy { kind });
    bundle.push(WorldName(name));
    bundle.push(sender);
    host.spawn(bundle)
}

/// Drain a stepped world's jobs and run its tick once.
///
/// Does nothing when `entity` does not name a world the host steps, or when
/// that world has already stopped. A tick that returns `false` stops the
/// world; a panic is caught, logged and recorded the way a remote world's
/// thread panic is, and ends the world too.
pub fn step_stepped_world(host: &World, entity: Entity) {
    let Some(mut proxy) = host.get_mut::<WorldProxy>(entity) else {
        return;
    };
    let WorldKind::Stepped(stepped) = &mut proxy.kind else {
        return;
    };
    if stepped.status.is_finished() {
        return;
    }
    if stepped.control.stop_requested() {
        stepped.status.finish();
        return;
    }
    loop {
        let Ok(job) = stepped.commands.try_recv() else {
            break;
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut stepped.world)));
        if let Err(payload) = outcome {
            let message = panic_message(payload.as_ref());
            log::error!("the stepped world {} panicked: {message}", stepped.name);
            stepped.status.record_panic(message);
            stepped.status.finish();
            return;
        }
    }
    let SteppedWorld {
        world,
        control,
        tick,
        status,
        name,
        ..
    } = &mut **stepped;
    LocalTasks::pump(world);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(world, control)));
    match outcome {
        Ok(true) => {}
        Ok(false) => status.finish(),
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            log::error!("the stepped world {name} panicked: {message}");
            status.record_panic(message);
            status.finish();
        }
    }
}

/// Run `f` against a stepped world's world, if `entity` names one.
///
/// A host that supplies its own buffers - an offline run filling an audio
/// buffer frame by frame, say - reads the world through this instead of
/// through the world's jobs.
pub fn with_stepped_world<R>(
    host: &World,
    entity: Entity,
    f: impl FnOnce(&World) -> R,
) -> Option<R> {
    let proxy = host.get::<WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &proxy.kind else {
        return None;
    };
    Some(f(&stepped.world))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::SendError;
    use std::time::Duration;

    /// Jobs sent through the handle run, and a request answers back.
    #[test]
    fn sent_jobs_run_and_requests_reply() {
        let mut host = World::new();
        let handle = spawn_remote_world(&mut host, "test", 8, |_, control| {
            control.wait_timeout(Duration::from_millis(1));
            true
        });
        handle
            .send(Box::new(|world: &mut World| {
                world.spawn((Count(0),));
            }))
            .expect("the world accepts jobs");
        let request = handle.request(Box::new(|world: &mut World| {
            world.query::<&mut Count>().next().expect("the counter").1.0 += 1;
            world.query::<&Count>().next().expect("the counter").1.0
        }));
        let mut answer = None;
        for _ in 0..2_000 {
            answer = request.poll();
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
        let handle = spawn_remote_world_with_init(
            &mut host,
            "test",
            8,
            |world: &mut World| {
                world.spawn((Count(0),));
            },
            |_, control, ()| {
                control.wait_timeout(Duration::from_millis(1));
                true
            },
        );
        let request = handle.request(Box::new(|world: &mut World| {
            world.query::<&Count>().next().is_some()
        }));
        let mut answered = None;
        for _ in 0..2_000 {
            answered = request.poll();
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
        let handle = spawn_remote_world(&mut host, "test", 1, |_, _| true);
        let mut results = Vec::new();
        for _ in 0..64 {
            results.push(handle.send(Box::new(|_| {})));
        }
        assert!(
            results.contains(&Err(SendError::Full)),
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
                world.spawn((Count(0),));
            },
            |world: &mut World, _control: &ThreadControl, ()| {
                world.query::<&mut Count>().next().expect("the counter").1.0 += 1;
                true
            },
        );
        // Nothing has run yet: the host has not stepped the world.
        assert_eq!(
            handle
                .request(Box::new(|world: &mut World| {
                    world.query::<&Count>().next().expect("the counter").1.0
                }))
                .poll(),
            None
        );
        step_stepped_world(&host, handle.entity());
        assert_eq!(
            handle
                .request(Box::new(|world: &mut World| {
                    world.query::<&Count>().next().expect("the counter").1.0
                }))
                .poll(),
            None
        );
        step_stepped_world(&host, handle.entity());
        let request = handle.request(Box::new(|world: &mut World| {
            world.query::<&Count>().next().expect("the counter").1.0
        }));
        // A step drains the queue before it ticks, exactly as a remote
        // world's loop does, so this request reads the count of the two
        // ticks that have already run.
        step_stepped_world(&host, handle.entity());
        assert_eq!(request.poll(), Some(2));
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
            |_, _: &ThreadControl, ()| false,
        );
        step_stepped_world(&host, handle.entity());
        assert!(handle.is_finished(), "a false tick ends a stepped world");
    }

    /// Stepping an entity that is not a stepped world does nothing.
    #[test]
    fn stepping_a_foreign_entity_does_nothing() {
        let mut host = World::new();
        let plain = host.spawn((Count(0),));
        step_stepped_world(&host, plain);
    }

    /// The counter the tests share with their worlds.
    struct Count(u32);
}
