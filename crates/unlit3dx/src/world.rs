//! Worlds the host reaches through a proxy component: one on its own
//! thread, or one the host steps itself.
//!
//! Both leave the same proxy component on the host entity, so the host's
//! world carries the same data either way. What differs is who drives the
//! world: a thread of its own, or the host's own frames.
//!
//! Whoever drives a world publishes its subscriptions: a turn of the loop
//! drains the queue, runs the tick, and then publishes what the world's
//! collectors returned. A tick body therefore collects nothing by hand and
//! cannot publish twice.

use async_channel::{Receiver, bounded};
use unlit_ecs::{Bundle, DynamicBundle, Entity, World, bundle};

use crate::channel::publish_collected;
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
    sender: JobSender,
    /// The liveness a sender checks before it posts.
    status: ThreadStatus,
}

impl WorldSender {
    /// Post a job to the world this component belongs to.
    pub fn send(&self, job: Job) -> Result<(), SendError> {
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
        self.inbox.close();
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
    ) -> WorldHandle
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
    ) -> WorldHandle
    where
        I: FnOnce(&mut World) -> S + Send + 'static,
        S: 'static,
        T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static,
    {
        spawn_stepped_world_with_init(host, name, command_capacity, init, tick)
    }
}

/// Spawn a threaded world on a fresh thread, driven by `tick`.
///
/// The thread's loop drains the jobs queued for the world, then runs
/// `tick` once, then publishes what the world's subscriptions collected;
/// a tick that returns `false` ends the thread. A job that
/// panics is a tick panic: it is reported and ends the thread. The handle
/// returned carries the handle every later job goes through, and
/// dropping the host entity it names stops the thread.
///
/// `tick` takes the whole seconds since the previous tick, so a tick body
/// can advance by the time that really passed rather than assume the
/// rate it asked to be woken at.
///
/// `name` labels the world for a bridge that lists the host's worlds; it
/// lands on the proxy entity as a [`WorldName`].
pub fn spawn_threaded_world<T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    mut tick: T,
) -> WorldHandle
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
///
/// # Panics
///
/// Panics when `command_capacity` is zero: a queue that can never hold
/// a job would silently block every sender.
pub fn spawn_threaded_world_with_init<I, S, T>(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
    init: I,
    mut tick: T,
) -> WorldHandle
where
    I: FnOnce(&mut World) -> S + Send + 'static,
    S: 'static,
    T: FnMut(&mut World, &ThreadControl, &mut S, f32) -> bool + Send + 'static,
{
    let (queue, inbox) = bounded::<Job>(command_capacity);
    let thread = WorldThread::spawn_with(name, init, move |world, control, state| {
        while let Ok(job) = inbox.try_recv() {
            job(world);
        }
        let running = tick(world, control, state, control.elapsed_seconds());
        // The turn is over: the driver publishes what the world's collectors
        // returned, so a tick body collects nothing by hand.
        publish_collected(world);
        running
    });
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
    WorldHandle::with_status(entity, sender, status)
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

/// Drain a stepped world's jobs and run its tick once with `delta_seconds`.
///
/// Does nothing when `entity` does not name a world the host steps, or when
/// that world has already stopped. A tick that returns `false` stops the
/// world; a panic, in a job or in the tick, is caught, logged and published
/// as the world's state, and ends the world too.
///
/// The step publishes what the world's subscriptions collected, the same as
/// a threaded world's loop does, so a tick body never publishes by hand.
pub fn step_stepped_world(host: &World, entity: Entity, delta_seconds: f32) {
    let Ok(mut proxy) = host.root_scope().query_one::<&mut WorldProxy>(entity) else {
        return;
    };
    let WorldKind::Stepped(stepped) = &mut proxy.kind else {
        return;
    };
    if stepped.status.is_finished() {
        return;
    }
    if stepped.control.stop_requested() {
        // A stop is not a panic, so the world collects one last time: the
        // state it had reached is the last its readers ever get.
        publish_collected(&stepped.world);
        stepped.status.finish();
        stepped.inbox.close();
        return;
    }
    if !run_queued_jobs(&mut *stepped) {
        return;
    }
    let SteppedWorld {
        world,
        inbox,
        control,
        tick,
        status,
        name,
    } = &mut **stepped;
    LocalTasks::pump(world);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tick(world, control, delta_seconds)
    }));
    match outcome {
        Ok(true) => {}
        Ok(false) => {
            // A tick that ends the world still publishes: the state it leaves
            // is the last one its readers ever get.
            publish_collected(world);
            status.finish();
            inbox.close();
            return;
        }
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            log::error!("the stepped world {name} panicked: {message}");
            status.record_panic(message);
            // A collector is arbitrary code: running one now would hand its
            // panic to the host, so the world stops collecting here.
            inbox.close();
            return;
        }
    }
    publish_collected(world);
}

/// Run every job queued for a stepped world, without ticking it.
///
/// This is the first half of [`step_stepped_world`]: a host that must answer a
/// command before its next frame drains the queue with it instead of running a
/// whole tick. Returns `false` when a job panicked, which ends the world the
/// same way a panicking tick does.
pub fn drain_stepped_world(host: &World, entity: Entity) -> bool {
    let Ok(mut proxy) = host.root_scope().query_one::<&mut WorldProxy>(entity) else {
        return true;
    };
    let WorldKind::Stepped(stepped) = &mut proxy.kind else {
        return true;
    };
    if stepped.status.is_finished() || stepped.control.stop_requested() {
        return true;
    }
    run_queued_jobs(stepped)
}

/// Run the jobs a stepped world has queued, stopping at the first panic.
fn run_queued_jobs(stepped: &mut SteppedWorld) -> bool {
    loop {
        let Ok(job) = stepped.inbox.try_recv() else {
            return true;
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut stepped.world)));
        if let Err(payload) = outcome {
            let message = panic_message(payload.as_ref());
            log::error!("the stepped world {} panicked: {message}", stepped.name);
            stepped.status.record_panic(message);
            // The world is over. Close the queue so a sender learns it from
            // the send, as a threaded world's departed receiver already
            // taught it.
            stepped.inbox.close();
            return false;
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
    let proxy = host.root_scope().query_one::<&WorldProxy>(entity).ok()?;
    let WorldKind::Stepped(stepped) = &proxy.kind else {
        return None;
    };
    Some(f(&stepped.world))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_SECONDS;
    use crate::job::SendError;
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
        });
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
                    .next()
                    .expect("the counter")
                    .0 += 1;
                let count = world
                    .root_scope()
                    .query::<&Count>()
                    .next()
                    .expect("the counter")
                    .0;
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
        );
        let seeded = crate::channel::Snapshot::new();
        let reader = seeded.reader();
        let publish = seeded.clone();
        handle
            .sender
            .send(Box::new(move |world: &mut World| {
                publish.publish(world.root_scope().query::<&Count>().next().is_some());
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
        let handle = spawn_threaded_world(&mut host, "test", 1, |_, _, _| true);
        let mut results = Vec::new();
        for _ in 0..64 {
            results.push(handle.sender.send(Box::new(|_| {})));
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
                world.root_scope_mut().spawn(bundle![Count(0)]);
            },
            |world: &mut World, _control: &ThreadControl, (), _: f32| {
                world
                    .root_scope()
                    .query::<&mut Count>()
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
                    .next()
                    .expect("the counter")
                    .0
            })
        };
        // Nothing has run yet: the host has not stepped the world.
        assert_eq!(count(&host), Some(0));
        step_stepped_world(&host, handle.entity, FRAME_SECONDS);
        assert_eq!(count(&host), Some(1));
        step_stepped_world(&host, handle.entity, FRAME_SECONDS);
        assert_eq!(count(&host), Some(2));
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
        step_stepped_world(&host, handle.entity, FRAME_SECONDS);
        assert!(handle.is_finished(), "a false tick ends a stepped world");
    }

    /// Stepping an entity that is not a stepped world does nothing.
    #[test]
    fn stepping_a_foreign_entity_does_nothing() {
        let mut host = World::new();
        let plain = host.root_scope_mut().spawn(bundle![Count(0)]);
        step_stepped_world(&host, plain, 0.0);
    }

    /// The counter the tests share with their worlds.
    struct Count(u32);
}
