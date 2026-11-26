//! The endpoint every spawn hands back.
//!
//! A spawned world - a sub-world on the host thread, a threaded world on its
//! own, a world the host steps - is reached the same way: a bounded command
//! channel that never blocks, a published state and a stop signal. A
//! [`WorldEndpoint`](crate::endpoint::WorldEndpoint) carries those together with the host entity the world's
//! proxy component lives on, so the pieces a caller acts on always describe
//! one world: there is no way to hold the queue of one world beside the state
//! of another.

use core::time::Duration;

use unlit_ecs::Entity;

use crate::channel::SnapshotReader;
use crate::job::JobSender;
use crate::thread::{JoinOutcome, ThreadStatus, WorldState};

/// Who drives a world.
///
/// It says where the world's tick runs, not how often: a host-owned world
/// turns when its owner steps it, and a thread-owned world turns on its own
/// loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WorldOwnership {
    /// The world lives in its owner's world, which drives it.
    HostOwned,
    /// The world lives on a thread of its own.
    ThreadOwned,
}

/// How a world is paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WorldDriving {
    /// The world runs on its own, as fast as its body allows.
    Continuous,
    /// The world turns only when its owner steps it.
    Stepped,
}

/// A cloneable handle to a spawned world.
///
/// It carries the host entity the world's proxy component lives on, a
/// cloneable command sender, the state the world publishes about itself, and
/// who owns and drives it. The fields are read-only: a caller reads them and
/// queues work through them, but cannot swap one world's queue for another's.
/// Cloning it does not spawn anything: the world lives on until the host
/// drops the proxy entity.
#[derive(Clone)]
pub struct WorldEndpoint {
    /// The host entity the world's proxy component lives on.
    entity: Entity,
    /// The command queue.
    commands: JobSender,
    /// What the world is doing, and how it ended.
    state: SnapshotReader<WorldState>,
    /// The stop signal and the state behind it.
    status: ThreadStatus,
    /// Who drives the world.
    ownership: WorldOwnership,
    /// How the world is paced.
    driving: WorldDriving,
}

impl WorldEndpoint {
    /// Bind an endpoint to a world the spawn functions drive themselves.
    #[must_use]
    pub(crate) fn new(
        entity: Entity,
        commands: JobSender,
        status: ThreadStatus,
        ownership: WorldOwnership,
        driving: WorldDriving,
    ) -> Self {
        Self {
            entity,
            commands,
            state: status.state(),
            status,
            ownership,
            driving,
        }
    }

    /// The host entity the world's proxy component lives on.
    ///
    /// Despawning it ends the world.
    #[must_use]
    pub fn entity(&self) -> Entity {
        self.entity
    }

    /// The command queue.
    ///
    /// Queueing through [`send`](JobSender::send) never blocks: a full or
    /// stopped queue reports [`SendError`](crate::job::SendError) and hands
    /// the job back.
    #[must_use]
    pub fn commands(&self) -> &JobSender {
        &self.commands
    }

    /// What the world is doing, and how it ended.
    ///
    /// A world publishes its state, so a reader that arrives after the end
    /// still learns how the world ended, and reading never takes the answer
    /// away from another reader.
    #[must_use]
    pub fn state(&self) -> SnapshotReader<WorldState> {
        self.state.clone()
    }

    /// Who drives the world.
    #[must_use]
    pub fn ownership(&self) -> WorldOwnership {
        self.ownership
    }

    /// How the world is paced.
    #[must_use]
    pub fn driving(&self) -> WorldDriving {
        self.driving
    }

    /// Whether the world has stopped or died.
    ///
    /// It reads the same state the world publishes, so a world of any kind
    /// answers it: a threaded world when its thread ends, a stepped world
    /// when its tick ends or its owner despawns it, and a sub-world when its
    /// host stops it or drops it.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }

    /// Ask the world to stop, without waiting for it.
    ///
    /// The world ends on its driver's next turn — the thread's next loop, the
    /// host's next [`step_stepped_world`](crate::world::step_stepped_world) or
    /// [`drain_sub_world`](crate::host::drain_sub_world) — and only then does
    /// its queue close. A job sent in between is accepted and then dropped,
    /// so a caller that must know the job ran reads [`state`](Self::state)
    /// rather than trusting the send.
    pub fn stop(&self) {
        self.status.stop();
    }

    /// Wait up to `timeout` for the thread that drives this world to finish.
    ///
    /// The answer says what became of the thread, not how the world ended:
    /// [`state`](Self::state) holds that. A world no thread ever drove — a
    /// host-owned one — answers [`JoinOutcome::NoHandle`], so a caller that
    /// waits on every endpoint tells "nothing to reclaim" from "already
    /// reclaimed". A timed-out wait keeps the handle, so calling this again
    /// waits for the same thread instead of pretending it finished.
    pub fn join_timeout(&self, timeout: Duration) -> JoinOutcome {
        self.status.join_timeout(timeout)
    }

    /// Give up waiting for the thread for good.
    ///
    /// The thread is not asked to stop, and it is not reclaimed: this only
    /// says the caller will not wait for it, so [`join_timeout`](Self::join_timeout)
    /// answers [`JoinOutcome::Detached`] from here on instead of waiting for a
    /// thread nobody will collect.
    pub fn detach(&self) {
        self.status.detach();
    }
}

#[cfg(test)]
mod tests {
    use unlit_ecs::World;

    use super::*;

    /// An endpoint queues jobs into the world it was spawned for.
    #[test]
    fn an_endpoint_queues_into_its_world() {
        let mut host = World::new();
        let endpoint = crate::host::spawn_sub_world(&mut host, "nested", 8);
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&ran);
        endpoint
            .commands()
            .send(Box::new(move |_: &mut World| {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
            .expect("the queue has room");
        let mut world_row = host.root_scope().query_one::<&mut World>(endpoint.entity());

        let nested = world_row.get();
        crate::host::drain_sub_world(nested);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A sub-world has no thread, so there is nothing for a wait to reclaim.
    #[test]
    fn a_host_owned_world_has_no_thread_to_join() {
        let mut host = World::new();
        let endpoint = crate::host::spawn_sub_world(&mut host, "nested", 8);
        assert_eq!(endpoint.ownership(), WorldOwnership::HostOwned);
        assert_eq!(endpoint.driving(), WorldDriving::Stepped);
        assert_eq!(
            endpoint.join_timeout(Duration::from_millis(1)),
            JoinOutcome::NoHandle
        );
    }
}
