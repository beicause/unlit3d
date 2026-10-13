//! A single-threaded executor for awaiting world requests.
//!
//! [`job::Request`] is a [`Future`], so a scene build that
//! queues a batch of assets and waits for the answers reads as one `async`
//! block. [`executor::JoinAll`] joins a batch of requests into a single future, and
//! [`executor::LocalPool`] drives a set of futures on the thread that owns the pool:
//! the main thread of a windowed app, or the tick loop of a headless run.
//!
//! The pool is deliberately small. It polls only the tasks that were woken
//! since the last call, on the thread that calls [`executor::LocalPool::run_ready`],
//! and it never blocks. A fixed-step simulation polls it once per tick; a
//! windowed app polls it from a frame behaviour.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use crate::job::{Request, RequestError};

/// The wake queue every task waker points at.
#[derive(Default)]
struct WakeQueue {
    woken: Mutex<VecDeque<usize>>,
}

impl WakeQueue {
    fn wake(&self, id: usize) {
        self.woken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(id);
    }
}

/// The waker one task holds; waking it queues the task again.
struct TaskWaker {
    id: usize,
    queue: Arc<WakeQueue>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.queue.wake(self.id);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.queue.wake(self.id);
    }
}

/// One task slot: a boxed, pinned future the pool drives to completion.
type TaskSlot = Pin<Box<dyn Future<Output = ()>>>;

/// A queue of futures driven by the thread that owns it.
///
/// Spawn a future with [`spawn`](Self::spawn) and poll the pool with
/// [`run_ready`](Self::run_ready) whenever the frame or tick advances. A
/// task woken from another thread is polled on the next call, so a request
/// answered by a remote world resolves without blocking the caller.
#[derive(Default)]
pub struct LocalPool {
    slots: RefCell<Vec<Option<TaskSlot>>>,
    queue: Arc<WakeQueue>,
}

impl LocalPool {
    /// A pool with no tasks.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: RefCell::new(Vec::new()),
            queue: Arc::new(WakeQueue {
                woken: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// Queue a future. It is polled on the next [`run_ready`](Self::run_ready).
    pub fn spawn<F: Future<Output = ()> + 'static>(&self, future: F) {
        let id = {
            let mut slots = self.slots.borrow_mut();
            let id = slots.len();
            slots.push(Some(Box::pin(future)));
            id
        };
        self.queue.wake(id);
    }

    /// Poll every task that was woken since the last call.
    ///
    /// Waking a task from inside a poll queues it again, so this runs until
    /// no task is ready.
    pub fn run_ready(&self) {
        loop {
            let id = {
                let mut woken = self
                    .queue
                    .woken
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let Some(id) = woken.pop_front() else {
                    break;
                };
                id
            };
            let Some(mut future) = self.slots.borrow_mut().get_mut(id).and_then(Option::take)
            else {
                continue;
            };
            let waker = Waker::from(Arc::new(TaskWaker {
                id,
                queue: Arc::clone(&self.queue),
            }));
            let mut context = Context::from_waker(&waker);
            if future.as_mut().poll(&mut context).is_pending()
                && let Some(slot) = self.slots.borrow_mut().get_mut(id)
            {
                *slot = Some(future);
            }
        }
    }
}

/// A future that resolves once every request in the batch has answered.
///
/// The results come back in request order, and the future fails as soon as
/// any request's world drops its job. This is the batch counterpart of
/// [`poll_all`](crate::job::poll_all) for use inside an `async` build.
pub struct JoinAll<R> {
    requests: Vec<Request<R>>,
    results: Vec<Option<R>>,
}

impl<R> JoinAll<R> {
    /// Wrap a batch of requests.
    #[must_use]
    pub fn new(requests: Vec<Request<R>>) -> Self {
        let results = (0..requests.len()).map(|_| None).collect();
        Self { requests, results }
    }
}

impl<R: Send + Unpin + 'static> Future for JoinAll<R> {
    type Output = Result<Vec<R>, RequestError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut pending = false;
        for (index, request) in this.requests.iter_mut().enumerate() {
            if this.results[index].is_some() {
                continue;
            }
            match Pin::new(request).poll(context) {
                Poll::Ready(Ok(value)) => this.results[index] = Some(value),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => pending = true,
            }
        }
        if pending {
            return Poll::Pending;
        }
        Poll::Ready(Ok(this
            .results
            .iter_mut()
            .map(|slot| slot.take().expect("every request answered"))
            .collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use unlit_ecs::World;

    /// A spawned future runs on the next poll.
    #[test]
    fn a_pool_runs_a_spawned_future() {
        let pool = LocalPool::new();
        let flag = Arc::new(AtomicU32::new(0));
        let writer = Arc::clone(&flag);
        pool.spawn(async move {
            writer.store(1, Ordering::SeqCst);
        });
        assert_eq!(flag.load(Ordering::SeqCst), 0, "spawning does not poll");
        pool.run_ready();
        assert_eq!(flag.load(Ordering::SeqCst), 1);
    }

    /// A pool resolves a request once its job has run elsewhere.
    #[test]
    fn a_pool_drives_a_request_to_its_answer() {
        let pool = LocalPool::new();
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 5));
        let answer: Arc<Mutex<Option<i32>>> = Arc::default();
        let writer = Arc::clone(&answer);
        pool.spawn(async move {
            *writer.lock().unwrap() = Some(request.await.unwrap());
        });
        pool.run_ready();
        assert_eq!(*answer.lock().unwrap(), None, "the job has not run yet");
        let mut world = World::new();
        job(&mut world);
        pool.run_ready();
        assert_eq!(*answer.lock().unwrap(), Some(5));
    }

    /// A waker fired from another thread queues the task for the next poll.
    #[test]
    fn a_wake_from_another_thread_queues_the_task() {
        let pool = LocalPool::new();
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 7));
        let answer: Arc<Mutex<Option<i32>>> = Arc::default();
        let writer = Arc::clone(&answer);
        pool.spawn(async move {
            *writer.lock().unwrap() = Some(request.await.unwrap());
        });
        pool.run_ready();
        let worker = std::thread::spawn(move || {
            let mut world = World::new();
            job(&mut world);
        });
        worker.join().expect("the worker finishes");
        pool.run_ready();
        assert_eq!(*answer.lock().unwrap(), Some(7));
    }

    /// JoinAll waits for every request, then returns the results in order.
    #[test]
    fn join_all_returns_the_batch_in_order() {
        let (job_one, one) = Request::wrap(Box::new(|_: &mut World| 1));
        let (job_two, two) = Request::wrap(Box::new(|_: &mut World| 2));
        let mut all = Box::pin(JoinAll::new(vec![one, two]));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(all.as_mut().poll(&mut context).is_pending());
        let mut world = World::new();
        job_one(&mut world);
        assert!(all.as_mut().poll(&mut context).is_pending());
        job_two(&mut world);
        assert_eq!(all.as_mut().poll(&mut context), Poll::Ready(Ok(vec![1, 2])));
    }

    /// JoinAll fails as soon as one request's job is dropped.
    #[test]
    fn join_all_fails_when_a_job_is_dropped() {
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 1));
        drop(job);
        let mut all = Box::pin(JoinAll::new(vec![request]));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert_eq!(
            all.as_mut().poll(&mut context),
            Poll::Ready(Err(RequestError::Dropped))
        );
    }
}
