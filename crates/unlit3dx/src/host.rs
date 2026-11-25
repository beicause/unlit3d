//! Host helpers: the sub-world that shares the host's thread.

use async_channel::{Receiver, bounded};
use unlit_ecs::{Bundle, DynamicBundle, World, bundle};
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::input::dispatch_input;

use crate::endpoint::{WorldDriving, WorldEndpoint, WorldOwnership};
use crate::executor::LocalTasks;
use crate::job::{Job, JobSender};
use crate::thread::ThreadStatus;
use crate::turn::{Link, StepOutcome, TickOutcome, close_inbox, drain_jobs, run_turn};
use crate::world::{WorldName, WorldSender};

/// The sub-world's own state: the name it logs under, the inbox its jobs arrive
/// in, and the status it publishes.
///
/// It is a component of the nested world, so dropping that world is what ends
/// the sub-world: the state reads `Stopped`, the queue closes, and a reader that
/// arrives afterwards still sees it.
struct SubWorld {
    name: &'static str,
    inbox: Receiver<Job>,
    status: ThreadStatus,
}

impl Drop for SubWorld {
    fn drop(&mut self) {
        self.status.finish();
        close_inbox(&self.inbox);
    }
}

impl Bundle for SubWorld {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self]
    }
}

/// The host entity a nested world hangs on: the world itself, under the name a
/// bridge lists it by.
struct HostWorld {
    world: World,
    name: &'static str,
    sender: WorldSender,
}

impl Bundle for HostWorld {
    fn name(&self) -> &'static str {
        self.name
    }

    fn into_bundle(self) -> DynamicBundle {
        bundle![self.world, WorldName(self.name), self.sender]
    }
}

/// Spawn a sub-world that shares the host's thread, nested as a [`World`]
/// component of the returned entity, with a command inbox of the given
/// capacity.
///
/// `name` labels the world for a bridge that lists the host's worlds; it lands
/// on the host entity as a [`WorldName`].
///
/// Sub-worlds are for work that must stay on the host thread — rendering into a
/// window's swap chain, say — but whose state should not mix with the host's.
/// Queued jobs run when the host calls [`drain_sub_world`] or [`tick_sub_world`],
/// and the world itself is reached with a [`World`] query on the entity the
/// returned endpoint names.
#[must_use]
pub fn spawn_sub_world(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
) -> WorldEndpoint {
    let (queue, inbox) = bounded(command_capacity);
    let mut nested = World::new();
    // The sub-world owns its async tasks, so a job that mounts something
    // asynchronous hands it a task instead of an executor.
    LocalTasks::install(&mut nested);
    let status = ThreadStatus::detached();
    nested.root_scope_mut().spawn(SubWorld {
        name,
        inbox,
        status: status.clone(),
    });
    let sender = JobSender::new(queue);
    let entity = host.root_scope_mut().spawn(HostWorld {
        world: nested,
        name,
        sender: WorldSender::new(sender.clone(), status.clone()),
    });
    WorldEndpoint::new(
        entity,
        sender,
        status,
        WorldOwnership::HostOwned,
        WorldDriving::Stepped,
    )
}

/// The parts of a sub-world a turn reads, cloned out of the nested world so the
/// turn can take that world mutably.
struct SubWorldParts {
    name: &'static str,
    inbox: Receiver<Job>,
    status: ThreadStatus,
}

impl SubWorldParts {
    fn link(&self) -> Link<'_> {
        Link {
            name: self.name,
            inbox: &self.inbox,
            status: &self.status,
        }
    }
}

fn sub_world_parts(nested: &World) -> SubWorldParts {
    let mut rows = nested.root_scope().query::<&SubWorld>();
    let sub_world = rows
        .iter()
        .next()
        .expect("a sub-world carries its own state");
    SubWorldParts {
        name: sub_world.name,
        inbox: sub_world.inbox.clone(),
        status: sub_world.status.clone(),
    }
}

/// Run every job queued for the sub-world so far, in queue order, without
/// running its frame.
///
/// Call this from the host thread before the world's frame is needed: jobs
/// arrive at any time and must land between frames, on the host's thread. The
/// jobs are committed before collectors publish; a drain never dispatches a
/// frame or pumps local tasks.
///
/// A job that panics ends the sub-world, and the returned outcome says so. A
/// sub-world that has already ended runs nothing and reports that it has ended.
pub fn drain_sub_world(nested: &mut World) -> StepOutcome {
    let parts = sub_world_parts(nested);
    drain_jobs(nested, parts.link())
}

/// Drive one frame of a sub-world: run the jobs queued for it, fold the input
/// it has been fed, run its tasks, dispatch the frame, then apply and publish.
///
/// This is the sub-world's turn, so the steps run in the order they depend on:
/// jobs land before the input dispatch looks for the behaviours they spawned,
/// and the frame draws what the tasks landed. A sub-world with no turn of its
/// own would otherwise keep every event it is fed, which is why the input tick
/// belongs here rather than to the caller.
///
/// A sub-world that has ended is left alone, and the outcome says why: its
/// owner stopped it, or a job or the frame panicked and ended it.
pub fn tick_sub_world(nested: &mut World, frame: &mut Frame) -> StepOutcome {
    let parts = sub_world_parts(nested);
    run_turn(nested, parts.link(), |world| {
        dispatch_input(world);
        LocalTasks::pump(world);
        dispatch_frame(world, frame);
        TickOutcome::Running
    })
}
