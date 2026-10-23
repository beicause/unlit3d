//! The handle every spawn hands back.
//!
//! A spawned world — a sub-world on the host thread, a threaded world on its
//! own — is reached the same way: a bounded command channel that never
//! blocks. [`crate::handle::WorldHandle`] is that uniform handle. The spawn
//! functions return it instead of a bare entity, so a caller queues work
//! without a second lookup for the proxy component on the host.

use unlit_ecs::Entity;

use crate::channel::SnapshotReader;
use crate::job::JobSender;
use crate::thread::{ThreadStatus, WorldState};

/// A cloneable handle to a spawned world.
///
/// It carries the host entity the world's proxy component lives on, a
/// cloneable command sender, and — for a world on its own thread — a reading
/// of that thread's liveness. It can be handed to the world that queues work
/// without the host's `World` in hand. Cloning it does not spawn anything:
/// the world lives on until the host drops the proxy entity.
#[derive(Clone)]
pub struct WorldHandle {
    /// The host entity the world's proxy component lives on.
    pub entity: Entity,
    /// The command queue. Queueing through [`send`](JobSender::send) never
    /// blocks: a full or stopped queue reports
    /// [`SendError`](crate::job::SendError) and hands the job back.
    pub sender: JobSender,
    /// What the world is doing, and how it ended.
    ///
    /// A world publishes its state, so a reader that arrives after the end
    /// still learns how the world ended, and reading never takes the answer
    /// away from another reader.
    pub state: SnapshotReader<WorldState>,
    status: ThreadStatus,
}

impl WorldHandle {
    /// Bind a handle to the entity carrying the world's proxy.
    ///
    /// A world the runtime does not drive passes
    /// [`ThreadStatus::detached`], which keeps it running until its owner
    /// says otherwise; a runner of the caller's own builds its handle this
    /// way.
    #[must_use]
    pub fn new(entity: Entity, sender: JobSender, status: ThreadStatus) -> Self {
        Self {
            entity,
            sender,
            state: status.state(),
            status,
        }
    }

    /// Whether the world has stopped or died.
    ///
    /// A sub-world shares the host's thread, so this is always `false` for
    /// one; the host itself decides when a sub-world is gone.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }

    /// Ask a threaded world's thread to stop, without waiting for it.
    pub fn stop(&self) {
        self.status.stop();
    }
}

#[cfg(test)]
mod tests {
    use unlit_ecs::World;

    /// A handle queues jobs into the world it was spawned for.
    #[test]
    fn a_handle_queues_into_its_world() {
        let mut host = World::new();
        let handle = crate::host::spawn_sub_world(&mut host, "nested", 8);
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&ran);
        handle
            .sender
            .send(Box::new(move |_: &mut World| {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
            .expect("the queue has room");
        let mut nested = host
            .root_scope()
            .query_one::<&mut World>(handle.entity)
            .expect("the nested world");
        crate::host::drain_sub_world(&mut nested);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }
}
