//! Jobs and requests: work handed to another world.
//!
//! A [`crate::job::Job`] is a boxed closure the target world runs on its own
//! thread. A [`crate::job::Request`] pairs a job with a reply slot: the job's
//! result lands in the slot, and the sender reads it synchronously with
//! [`Request::poll`](crate::job::Request::poll) or awaits it as a [`Future`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};

use unlit_ecs::World;

use crate::sync;

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
/// hands out. Sends never block: a full or stopped queue reports
/// [`SendError`] and the job comes back.
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

    /// Queue a job that answers back through the returned request.
    #[must_use = "the request carries the job's result"]
    pub fn request<R: Send + 'static>(&self, job: Job<R>) -> Request<R> {
        let (job, request) = Request::wrap(job);
        // A refused job is dropped here, which marks the request as Dropped.
        let _ = self.send(job);
        request
    }

    /// Whether the world's queue is empty right now.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Whether the world has stopped.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }
}

/// Why a request produced no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The world dropped the job without running it: it stopped, or its
    /// command queue was full when the request was made.
    Dropped,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the world dropped the request without running it")
    }
}

impl std::error::Error for RequestError {}

/// The reply slot a running job writes and the request that awaits it reads.
struct ReplyState<R> {
    inner: Mutex<ReplyInner<R>>,
    ready: Condvar,
}

struct ReplyInner<R> {
    value: Option<R>,
    waker: Option<Waker>,
    dropped: bool,
}

impl<R> ReplyState<R> {
    fn new() -> Self {
        Self {
            inner: Mutex::new(ReplyInner {
                value: None,
                waker: None,
                dropped: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Store the result and wake whoever is waiting.
    fn complete(&self, value: R) {
        let waker = {
            let mut inner = sync::lock(&self.inner);
            inner.value = Some(value);
            inner.waker.take()
        };
        self.ready.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Mark the slot as never going to receive a result, and wake the waiter.
    fn drop_without_result(&self) {
        let waker = {
            let mut inner = sync::lock(&self.inner);
            inner.dropped = true;
            inner.waker.take()
        };
        self.ready.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Marks a reply slot as dropped when its job is dropped without running.
struct ReplyGuard<R> {
    state: Arc<ReplyState<R>>,
    completed: bool,
}

impl<R> Drop for ReplyGuard<R> {
    fn drop(&mut self) {
        if !self.completed {
            self.state.drop_without_result();
        }
    }
}

/// A handle to a job's result.
///
/// The result arrives once the target world runs the job. Poll it with
/// [`poll`](Self::poll) from a loop, block on it with
/// [`recv`](Self::recv), or await it as a [`Future`]. If the world
/// drops the job instead of running it, every one of those reports
/// [`RequestError::Dropped`].
pub struct Request<R> {
    state: Arc<ReplyState<R>>,
}

impl<R> Request<R> {
    /// Wrap a job so its result arrives through the returned request.
    #[must_use = "the request carries the job's result"]
    pub fn wrap(job: Job<R>) -> (Job<()>, Self)
    where
        R: Send + 'static,
    {
        let state = Arc::new(ReplyState::new());
        let guard = ReplyGuard {
            state: Arc::clone(&state),
            completed: false,
        };
        let job = Box::new(move |world: &mut World| {
            let mut guard = guard;
            let value = job(world);
            guard.completed = true;
            guard.state.complete(value);
        });
        (job, Self { state })
    }

    /// Take the result if the job has run.
    #[must_use]
    pub fn poll(&self) -> Option<R> {
        sync::lock(&self.state.inner).value.take()
    }

    /// Whether a result is waiting.
    #[must_use]
    pub fn answered(&self) -> bool {
        sync::lock(&self.state.inner).value.is_some()
    }

    /// Whether the job was dropped without running, so no result will ever
    /// arrive.
    #[must_use]
    pub fn dropped(&self) -> bool {
        let inner = sync::lock(&self.state.inner);
        inner.dropped && inner.value.is_none()
    }

    /// Block until the result arrives.
    pub fn recv(self) -> Result<R, RequestError> {
        let mut inner = sync::lock(&self.state.inner);
        loop {
            if let Some(value) = inner.value.take() {
                return Ok(value);
            }
            if inner.dropped {
                return Err(RequestError::Dropped);
            }
            inner = self
                .state
                .ready
                .wait(inner)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl<R> Future for Request<R> {
    type Output = Result<R, RequestError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = sync::lock(&self.state.inner);
        if let Some(value) = inner.value.take() {
            return Poll::Ready(Ok(value));
        }
        if inner.dropped {
            return Poll::Ready(Err(RequestError::Dropped));
        }
        inner.waker = Some(context.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrapped job's result comes back through the request.
    #[test]
    fn wrap_sends_the_result_through_the_handle() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 2 + 2));
        let mut world = World::new();
        job(&mut world);
        assert_eq!(request.poll(), Some(4));
    }

    /// Polling before the job runs finds nothing.
    #[test]
    fn poll_is_empty_before_the_job_runs() {
        let (_job, request) = Request::wrap(Box::new(|_: &mut World| 1));
        assert_eq!(request.poll(), None);
        assert!(!request.answered());
        assert!(!request.dropped());
    }

    /// The peek keeps the answer: only poll takes it away.
    #[test]
    fn answered_peeks_without_consuming() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 7));
        assert!(!request.answered());
        let mut world = World::new();
        job(&mut world);
        assert!(request.answered());
        assert!(request.answered(), "peeking twice keeps the answer");
        assert_eq!(request.poll(), Some(7));
        assert!(!request.answered(), "polling consumed the answer");
    }

    /// A job dropped without running fails its request instead of hanging.
    #[test]
    fn a_dropped_job_fails_the_request() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 1));
        drop(job);
        assert!(request.dropped());
        assert_eq!(request.recv(), Err(RequestError::Dropped));
    }

    /// A job that panics fails its request instead of hanging.
    #[test]
    fn a_panicking_job_fails_the_request() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| -> u8 {
            panic!("boom");
        }));
        let mut world = World::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut world)));
        assert!(result.is_err());
        assert_eq!(request.recv(), Err(RequestError::Dropped));
    }

    /// A request can be awaited, and its result wakes the awaiter.
    #[test]
    fn a_request_can_be_awaited() {
        let (job, mut request) = Request::wrap(Box::new(|_: &mut World| 9));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(Pin::new(&mut request).poll(&mut context).is_pending());
        let mut world = World::new();
        job(&mut world);
        assert_eq!(
            Pin::new(&mut request).poll(&mut context),
            Poll::Ready(Ok(9))
        );
    }
}
