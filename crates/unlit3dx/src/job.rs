//! Jobs: work handed to another world.
//!
//! A [`Job`](crate::job::Job) is a boxed closure the target world runs on its
//! own thread. The sender never waits for it: a queue that is full or stopped
//! reports [`SendError`](crate::job::SendError) and the job comes back.
//!
//! A job never answers back directly. What one world learns from another is
//! state, and state travels the other way: a subscription publishes it, and the
//! caller reads it on its own tick. A caller that needs the value one job
//! produces asks for it — [`JobSender::ask`](crate::job::JobSender::ask)
//! sends the job and hands back the snapshot its answer lands in — or publishes
//! that value itself, through the snapshot its subscription fills.

use std::fmt;

use unlit_ecs::World;

/// A unit of work for a world: a boxed closure that runs against it.
pub type Job<R = ()> = Box<dyn FnOnce(&mut World) -> R + Send + 'static>;

/// Why a job was not handed to a world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The world's command queue is full; the job was not queued.
    Full,
    /// The world has stopped; the job was not queued.
    Closed,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("the world's command queue is full"),
            Self::Closed => f.write_str("the world has stopped"),
        }
    }
}

impl std::error::Error for SendError {}

impl<T> From<async_channel::TrySendError<T>> for SendError {
    fn from(error: async_channel::TrySendError<T>) -> Self {
        match error {
            async_channel::TrySendError::Full(_) => Self::Full,
            async_channel::TrySendError::Closed(_) => Self::Closed,
        }
    }
}

/// A cloneable handle to a world's command queue.
///
/// This is the sender half a [`WorldHandle`](crate::handle::WorldHandle)
/// carries in its `commands` field. Sends never block: a full or stopped
/// queue reports [`SendError`] and the job is dropped.
#[derive(Clone)]
pub struct JobSender {
    commands: async_channel::Sender<Job<()>>,
}

impl JobSender {
    /// Wrap a world's command channel.
    #[must_use]
    pub fn new(commands: async_channel::Sender<Job<()>>) -> Self {
        Self { commands }
    }

    /// Queue a job. Never blocks.
    pub fn send(&self, job: Job<()>) -> Result<(), SendError> {
        self.commands.try_send(job).map_err(SendError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world that stopped refuses the job and says why.
    #[test]
    fn sending_to_a_stopped_world_reports_closed() {
        let (sender, receiver) = async_channel::unbounded();
        let jobs = JobSender::new(sender);
        drop(receiver);
        assert_eq!(
            jobs.send(Box::new(|_: &mut World| {})),
            Err(SendError::Closed)
        );
    }
}
