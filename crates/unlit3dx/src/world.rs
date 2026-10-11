//! Remote worlds: a proxy component on the host that routes jobs to a thread.

use crossbeam_channel::{Sender, bounded};
use unlit_ecs::{Entity, World};

use crate::job::{Job, Request};
use crate::thread::{ThreadControl, WorldThread};

/// A world running on its own thread, reached through the entity that
/// carries this component.
///
/// The entity is only a route key: jobs are moved to the thread, and the
/// thread's world never crosses back. Sending never runs the job — the
/// thread drains its command channel between ticks — and a full channel
/// makes [`send`](Self::send) wait for room, while a stopped one makes it
/// log and drop.
pub struct RemoteWorld {
    /// The command channel the thread drains between ticks.
    commands: Sender<Job<()>>,
    /// The thread the world lives on.
    thread: WorldThread,
}

impl RemoteWorld {
    /// Hand a job to the world.
    pub fn send(&self, job: Job<()>) {
        if let Err(error) = self.commands.send(job) {
            log::warn!("a job was dropped: the remote world has stopped ({error})");
        }
    }

    /// Hand a request to the world and return the handle its reply is
    /// polled through.
    pub fn request<R: Send + 'static>(&self, job: Job<R>) -> Request<R> {
        let (job, request) = Request::wrap(job);
        self.send(job);
        request
    }

    /// A sender to the world's command channel, for threads that hand jobs
    /// over without going through the host.
    pub fn sender(&self) -> Sender<Job<()>> {
        self.commands.clone()
    }

    /// The thread the world lives on.
    pub fn thread(&self) -> &WorldThread {
        &self.thread
    }
}

/// Spawn a remote world on a fresh thread, driven by `tick`.
///
/// The thread's loop drains the jobs queued for the world, then runs
/// `tick` once; a tick that returns `false` ends the thread. A job that
/// panics is a tick panic: it is reported and ends the thread. The host
/// entity returned carries the [`RemoteWorld`] every later job goes
/// through, and dropping it stops the thread.
pub fn spawn_remote_world<T>(
    host: &mut World,
    name: &str,
    command_capacity: usize,
    tick: T,
) -> Entity
where
    T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
{
    spawn_remote_world_with(host, name, command_capacity, (), tick)
}

/// Like [`spawn_remote_world`], with one more component on the proxy.
///
/// The component travels with the [`RemoteWorld`] it is bound to, which is
/// how a world's reader half — a snapshot inbox, say — reaches the host on
/// the same entity its jobs are addressed by.
pub fn spawn_remote_world_with<C, T>(
    host: &mut World,
    name: &str,
    command_capacity: usize,
    extra: C,
    mut tick: T,
) -> Entity
where
    C: 'static,
    T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
{
    let (commands, receiver) = bounded::<Job<()>>(command_capacity);
    let thread = WorldThread::spawn(name, move |world, control| {
        while let Ok(job) = receiver.try_recv() {
            job(world);
        }
        tick(world, control)
    });
    host.spawn((RemoteWorld { commands, thread }, extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A job sent to a remote world runs on its thread, and a request's
    /// reply comes back to the sender.
    #[test]
    fn sent_jobs_run_and_requests_reply() {
        let mut host = World::new();
        let world = spawn_remote_world(&mut host, "test", 8, |_, control| {
            control.wait_timeout(Duration::from_millis(1));
            true
        });
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let remote = host.get::<RemoteWorld>(world).expect("the proxy exists");
        let flag = std::sync::Arc::clone(&done);
        remote.send(Box::new(move |_: &mut World| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        let reply = remote.request(Box::new(|_: &mut World| 7));
        let reply = loop {
            if let Some(reply) = reply.poll() {
                break reply;
            }
            assert!(
                !remote.thread().is_finished(),
                "the remote world stopped before replying"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(reply, 7);
        // The plain job ran too, on the same thread the request did.
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }
}
