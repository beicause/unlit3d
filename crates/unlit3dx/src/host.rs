//! Host helpers: the sub-world that shares the host's thread.

use async_channel::{Receiver, bounded};
use unlit_ecs::World;

use crate::executor::LocalTasks;
use crate::handle::WorldHandle;
use crate::job::{Job, JobSender};
use crate::thread::panic_message;
use crate::world::WorldName;
use unlit_ecs::{bundle, query};

/// The sub-world's command inbox, a component of the nested world itself.
///
/// [`drain_sub_world`] empties it. It is an implementation detail of the
/// sub-world: callers reach the world through the [`WorldHandle`] the
/// spawn returns, never through this component.
struct SubWorldInbox(Receiver<Job<()>>);

/// Spawn a sub-world that shares the host's thread, nested as a [`World`]
/// component of the returned entity, with a command inbox of the given
/// capacity.
///
/// `name` labels the world for a bridge that lists the host's worlds; it
/// lands on the host entity as a [`WorldName`].
///
/// Sub-worlds are for work that must stay on the host thread — rendering
/// into a window's swap chain, say — but whose state should not mix with
/// the host's. Queued jobs run when the host calls [`drain_sub_world`], and
/// the world itself is driven with [`World::get_mut`] on the entity the
/// returned handle names.
///
/// # Panics
///
/// Panics when `command_capacity` is zero: a queue that can never hold
/// a job would silently block every sender.
#[must_use]
pub fn spawn_sub_world(
    host: &mut World,
    name: &'static str,
    command_capacity: usize,
) -> WorldHandle {
    let (commands, receiver) = bounded(command_capacity);
    let mut nested = World::new();
    // The sub-world owns its async tasks, so a job that mounts something
    // asynchronous hands it a task instead of an executor.
    LocalTasks::install(&mut nested);
    nested
        .root_scope_mut()
        .spawn(bundle!(SubWorldInbox(receiver),));
    let commands = JobSender::new(commands);
    let entity = host
        .root_scope_mut()
        .spawn(bundle!(nested, WorldName(name)));
    WorldHandle::new(entity, commands, None)
}

/// Run every job queued for the sub-world so far, in queue order.
///
/// Call this from the host thread before driving the world's frame: jobs
/// arrive at any time and must land between frames, on the host's thread.
///
/// A job that panics is reported and the drain continues, so one broken job
/// cannot take the host down with it.
pub fn drain_sub_world(nested: &mut World) {
    // The jobs take the world while they run, so the inbox borrow must end
    // before the first one starts. Cloning the receiver is cheap and lets the
    // drain run without collecting the jobs first.
    let receiver = {
        let Some((_, row)) = nested.root_scope().query(query!(&SubWorldInbox)).next() else {
            return;
        };
        row.read::<SubWorldInbox>().0.clone()
    };
    while let Ok(job) = receiver.try_recv() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(nested)));
        if let Err(payload) = result {
            log::error!(
                "a sub-world job panicked: {}",
                panic_message(payload.as_ref())
            );
        }
    }
}
