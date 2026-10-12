//! Host helpers: the sub-world that shares the host's thread.

use crossbeam_channel::{Receiver, Sender, bounded};
use log::warn;
use unlit_ecs::{Entity, World};

use crate::job::{Job, Request};

/// The proxy a host entity carries to reach its sub-world: a command channel
/// into the nested world.
///
/// A sub-world shares the host's thread, so its jobs only run while the host
/// drives it — once per frame, typically, with [`drain_sub_world`]. Clones of
/// a proxy all reach the same world.
#[derive(Clone)]
pub struct SubWorld {
    /// The command channel the nested world drains.
    commands: Sender<Job<()>>,
}

impl SubWorld {
    /// Queue a job to run against the sub-world.
    ///
    /// The queue is bounded, so a host that stops draining applies back
    /// pressure instead of growing without limit; a send only fails once the
    /// sub-world is gone.
    pub fn send(&self, job: Job<()>) {
        if let Err(error) = self.commands.send(job) {
            warn!("the sub-world is gone, dropping a job: {error}");
        }
    }

    /// Queue a job that answers back; poll the returned request for the
    /// result.
    #[must_use = "the request carries the job's result"]
    pub fn request<R: Send + 'static>(&self, job: Job<R>) -> Request<R> {
        let (job, request) = Request::wrap(job);
        self.send(job);
        request
    }

    /// A clone of the command channel, for worlds that queue jobs without
    /// holding the proxy.
    #[must_use]
    pub fn sender(&self) -> Sender<Job<()>> {
        self.commands.clone()
    }
}

/// The sub-world's command inbox, a component of the nested world itself.
///
/// [`drain_sub_world`] empties it.
pub struct SubWorldInbox(Receiver<Job<()>>);

/// Spawn a sub-world that shares the host's thread, nested as a `World`
/// component of the returned entity, with a command inbox of the given
/// capacity.
///
/// Sub-worlds are for work that must stay on the host thread — rendering
/// into a window's swap chain, say — but whose state should not mix with
/// the host's. Other worlds reach it through the [`SubWorld`] proxy on the
/// returned entity: queued jobs run when the host calls [`drain_sub_world`],
/// and the world itself is driven with `World::get_mut` on the returned
/// entity.
#[must_use]
pub fn spawn_sub_world(host: &mut World, command_capacity: usize) -> Entity {
    let (commands, receiver) = bounded(command_capacity);
    let mut nested = World::new();
    nested.spawn((SubWorldInbox(receiver),));
    host.spawn((nested, SubWorld { commands }))
}

/// Run every job queued for the sub-world so far, in queue order.
///
/// Call this from the host thread before driving the world's frame: jobs
/// arrive at any time and must land between frames, on the host's thread.
pub fn drain_sub_world(nested: &mut World) {
    // The jobs take the world while they run, so the inbox borrow must end
    // before the first one starts.
    let jobs = {
        let Some((_, inbox)) = nested.query::<&SubWorldInbox>().next() else {
            return;
        };
        core::iter::from_fn(|| inbox.0.try_recv().ok()).collect::<Vec<_>>()
    };
    for job in jobs {
        job(nested);
    }
}
