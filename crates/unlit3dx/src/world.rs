//! Remote worlds: a proxy component on the host that routes jobs to a thread.

use async_channel::bounded;
use unlit_ecs::{ArchetypeBuilder, Bundle, World};

use crate::handle::WorldHandle;
use crate::job::{Job, JobSender};
use crate::thread::{ThreadControl, ThreadStatus, WorldThread};

/// The proxy component a remote world leaves on the host: it owns the
/// thread, so despawning the entity requests the thread's stop.
///
/// It is an implementation detail of a remote world. Callers reach the
/// world through the [`WorldHandle`] the spawn returns, never through this
/// component.
struct RemoteWorld {
    _thread: WorldThread,
}

/// Spawn a remote world on a fresh thread, driven by `tick`.
///
/// The thread's loop drains the jobs queued for the world, then runs
/// `tick` once; a tick that returns `false` ends the thread. A job that
/// panics is a tick panic: it is reported and ends the thread. The handle
/// returned carries the handle every later job goes through, and
/// dropping the host entity it names stops the thread.
pub fn spawn_remote_world<T>(
    host: &mut World,
    name: &str,
    command_capacity: usize,
    tick: T,
) -> WorldHandle
where
    T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
{
    spawn_remote_world_with(host, name, command_capacity, (), tick)
}

/// Like [`spawn_remote_world`], with more components on the proxy.
///
/// The components travel with the world they are bound to, which is
/// how a world's reader halves — snapshot inboxes, say — reach the host on
/// the same entity its jobs are addressed by.
pub fn spawn_remote_world_with<C, T>(
    host: &mut World,
    name: &str,
    command_capacity: usize,
    extra: C,
    mut tick: T,
) -> WorldHandle
where
    C: Bundle,
    T: FnMut(&mut World, &ThreadControl) -> bool + Send + 'static,
{
    spawn_remote_world_with_init(
        host,
        name,
        command_capacity,
        extra,
        |_| (),
        move |world, control, ()| tick(world, control),
    )
}

/// Like [`spawn_remote_world_with`], with a one-time setup step.
///
/// `init` runs on the world's thread before the first job is drained, so
/// a world can create the components its jobs assume exist — its physics
/// world, say — and hand any state they need to the tick. The state it
/// returns is passed to every tick.
///
/// # Panics
///
/// Panics when `command_capacity` is zero: a queue that can never hold
/// a job would silently block every sender.
pub fn spawn_remote_world_with_init<C, I, S, T>(
    host: &mut World,
    name: &str,
    command_capacity: usize,
    extra: C,
    init: I,
    mut tick: T,
) -> WorldHandle
where
    C: Bundle,
    I: FnOnce(&mut World) -> S + Send + 'static,
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
    // The proxy and the caller's components go on together as one flat
    // bundle, so a bundle of several inboxes lands as several components
    // rather than as one tuple.
    let mut bundle = ArchetypeBuilder::new();
    bundle.push(RemoteWorld { _thread: thread });
    bundle.push_bundle(extra);
    let entity = host.spawn(bundle);
    WorldHandle::new(entity, commands, Some(status))
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
            (),
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

    /// The counter the tests share with their remote world.
    struct Count(u32);
}
