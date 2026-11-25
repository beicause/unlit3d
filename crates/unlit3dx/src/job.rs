//! Jobs: work handed to another world.
//!
//! A [`Job`](crate::job::Job) is a boxed closure the target world runs on its
//! own thread. The sender never waits for it: a queue that is full or stopped
//! reports [`SendError`](crate::job::SendError), which carries the job it did
//! not take.
//!
//! A job never answers back directly. What one world learns from another is
//! state, and state travels the other way: a subscription publishes it, and the
//! caller reads it on its own tick. A caller that needs the value one job
//! produces asks for it under a key — [`Pending`](crate::request::Pending)
//! sends the job, remembers the key, and answers on the caller's next tick — or
//! publishes that value itself, through the snapshot its subscription fills.

use unlit_ecs::World;

/// A unit of work for a world: a boxed closure that runs against it.
///
/// A job answers nothing: the queue is one way, and a value the caller
/// wants back travels as state, published by the world that made it and
/// read on the caller's own tick. A [`Pending`](crate::request::Pending) is
/// that round trip: one job per request, and the key its answer comes back
/// under.
pub type Job = Box<dyn FnOnce(&mut World) + Send + 'static>;

/// A job that was not handed to a world, and why.
///
/// A job that was refused is one that never ran: the world it was meant for
/// did not see it, so sending it again is not a repeat of work already done.
/// It is a boxed closure, so it is handed back here rather than dropped, and
/// the caller decides what to do with it — queue it again, keep it for a later
/// tick, or drop it and let the work go.
///
/// Dropping this error drops the job. What that costs depends on what the job
/// carried: work never started never ran, but a job that held state a later
/// tick needs — a frame's texture updates, say — takes it with it, which is
/// why the callers that care keep the job instead of the error.
pub struct SendError {
    kind: SendErrorKind,
    job: Job,
}

impl SendError {
    /// A refusal of `job` for `kind`'s reason.
    #[must_use]
    pub fn new(kind: SendErrorKind, job: Job) -> Self {
        Self { kind, job }
    }

    /// Why the job was refused.
    ///
    /// The reason is a plain value, which is what lets a caller match on it
    /// without the job in hand: whether the queue was full and may have room
    /// later, or the world has stopped and never will.
    #[must_use]
    pub fn kind(&self) -> SendErrorKind {
        self.kind
    }

    /// Take the job back, for sending it again or keeping it.
    pub fn into_job(self) -> Job {
        self.job
    }
}

impl core::fmt::Debug for SendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The job is a closure, which has nothing to print; the reason is
        // what a reader of a log line needs, and the job is what the caller
        // takes back.
        f.debug_struct("SendError")
            .field("kind", &self.kind)
            .finish()
    }
}

impl core::fmt::Display for SendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.kind.fmt(f)
    }
}

impl core::error::Error for SendError {}

/// Why a job was not handed to a world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SendErrorKind {
    /// The world's command queue is full; the job was not queued.
    #[error("the world's command queue is full")]
    Full,
    /// The world has stopped; the job was not queued.
    #[error("the world has stopped")]
    Closed,
}

impl<T> From<async_channel::TrySendError<T>> for SendErrorKind {
    fn from(error: async_channel::TrySendError<T>) -> Self {
        match error {
            async_channel::TrySendError::Full(_) => Self::Full,
            async_channel::TrySendError::Closed(_) => Self::Closed,
        }
    }
}

impl From<async_channel::TrySendError<Job>> for SendError {
    fn from(error: async_channel::TrySendError<Job>) -> Self {
        match error {
            async_channel::TrySendError::Full(job) => Self::new(SendErrorKind::Full, job),
            async_channel::TrySendError::Closed(job) => Self::new(SendErrorKind::Closed, job),
        }
    }
}

/// A cloneable handle to a world's job queue.
///
/// A world is reached either through the [`WorldHandle`](crate::handle::WorldHandle)
/// it was spawned with or through the [`WorldSender`](crate::world::WorldSender)
/// its proxy entity carries; both hold one of these. Sends never block: a full
/// or stopped queue reports [`SendError`] carrying the job.
#[derive(Clone)]
pub struct JobSender {
    queue: async_channel::Sender<Job>,
}

impl JobSender {
    /// Wrap a world's command channel.
    #[must_use]
    pub fn new(queue: async_channel::Sender<Job>) -> Self {
        Self { queue }
    }

    /// Queue a job. Never blocks.
    ///
    /// An `Ok` means the job is in the queue; whether it runs is the world's
    /// business. A world its owner has asked to stop keeps accepting jobs
    /// until its driver's next turn, and drops them when it ends, so a caller
    /// that must know the job ran reads the world's state instead.
    ///
    /// A refused job comes back in the error, unrun: the queue was full, or
    /// the world had stopped. Sending it again is safe because it never
    /// started.
    pub fn send(&self, job: Job) -> Result<(), SendError> {
        self.queue.try_send(job).map_err(SendError::from)
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
        let refused = jobs
            .send(Box::new(|_: &mut World| {}))
            .expect_err("a stopped world takes nothing");
        assert_eq!(refused.kind(), SendErrorKind::Closed);
        let job = refused.into_job();
        job(&mut World::new());
    }
}
