//! A single-threaded executor for awaiting world requests.
//!
//! [`crate::job::Request`] is a future, so a scene build that queues a batch
//! of assets and waits for the answers reads as one `async` block:
//! [`crate::executor::try_join_all`] joins a batch into a single future of
//! the answers in request order, and [`crate::executor::LocalPool`] drives it
//! on the thread that owns the pool — the main thread of a windowed app, or
//! the tick loop of a headless run.
//!
//! Both are the ecosystem's own, re-exported so a caller needs no futures
//! dependency of their own. The pool is the one that fits a frame loop: it
//! polls every task it holds until none can make progress and never blocks,
//! so a fixed-step simulation runs it once per tick and a windowed app runs
//! it from a frame behaviour. A task woken by another thread — a remote
//! world answering a request — is picked up by the next call.
//!
//! A spawned task is `'static`, so what it lands has to be shared with the
//! code that reads it: an `Rc<RefCell<..>>` slot that the tick drains, as
//! the demo's scene build does.

pub use futures_executor::LocalPool;
pub use futures_util::future::try_join_all;
pub use futures_util::task::LocalSpawnExt;

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use unlit_ecs::World;

    use super::*;
    use crate::job::{Request, RequestError};

    /// A spawned future waits for the pool and runs on its next poll.
    #[test]
    fn a_pool_runs_a_spawned_future() {
        let mut pool = LocalPool::new();
        // A task is `'static`, so what it writes has to be shared with the
        // test rather than borrowed from it.
        let seen: Rc<RefCell<bool>> = Rc::default();
        let slot = Rc::clone(&seen);
        pool.spawner()
            .spawn_local(async move { *slot.borrow_mut() = true })
            .expect("a live pool takes its tasks");
        assert!(!*seen.borrow(), "spawning does not poll");
        pool.run_until_stalled();
        assert!(*seen.borrow());
    }

    /// A request answered by another thread resolves through the pool.
    #[test]
    fn a_pool_drives_a_request_to_its_answer() {
        let mut pool = LocalPool::new();
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 5));
        let answer: Rc<RefCell<Option<usize>>> = Rc::default();
        let slot = Rc::clone(&answer);
        pool.spawner()
            .spawn_local(async move {
                *slot.borrow_mut() = Some(request.await.unwrap());
            })
            .expect("a live pool takes its tasks");
        pool.run_until_stalled();
        assert_eq!(*answer.borrow(), None, "the job has not run yet");
        let mut world = World::new();
        job(&mut world);
        pool.run_until_stalled();
        assert_eq!(*answer.borrow(), Some(5));
    }

    /// A waker fired from another thread queues the task for the next poll.
    #[test]
    fn a_wake_from_another_thread_queues_the_task() {
        let mut pool = LocalPool::new();
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 7));
        let answer: Rc<RefCell<Option<usize>>> = Rc::default();
        let slot = Rc::clone(&answer);
        pool.spawner()
            .spawn_local(async move {
                *slot.borrow_mut() = Some(request.await.unwrap());
            })
            .expect("a live pool takes its tasks");
        pool.run_until_stalled();
        assert_eq!(*answer.borrow(), None, "the worker has not run yet");
        let worker = std::thread::spawn(move || {
            let mut world = World::new();
            job(&mut world);
        });
        worker.join().expect("the worker finishes");
        pool.run_until_stalled();
        assert_eq!(*answer.borrow(), Some(7));
    }

    /// `try_join_all` waits for every request and returns the batch in order.
    #[test]
    fn a_joined_batch_comes_back_in_request_order() {
        let mut jobs = Vec::new();
        let mut requests = Vec::new();
        for value in [1, 2, 3] {
            let (job, request) = Request::wrap(Box::new(move |_: &mut World| value));
            jobs.push(job);
            requests.push(request);
        }
        let mut joined = Box::pin(try_join_all(requests));
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        assert!(joined.as_mut().poll(&mut context).is_pending());
        let mut world = World::new();
        for job in jobs {
            job(&mut world);
        }
        assert_eq!(
            joined.as_mut().poll(&mut context),
            std::task::Poll::Ready(Ok(vec![1, 2, 3]))
        );
    }

    /// A joined batch fails as soon as one request's job is dropped.
    #[test]
    fn a_joined_batch_fails_on_a_dropped_request() {
        let mut pool = LocalPool::new();
        let (job, request) = Request::wrap(Box::new(|_: &mut World| 1u8));
        drop(job);
        // The landing slot a world would hand its build future.
        type Landed = Rc<RefCell<Option<Result<Vec<u8>, RequestError>>>>;
        let answer: Landed = Rc::default();
        let slot = Rc::clone(&answer);
        pool.spawner()
            .spawn_local(async move {
                *slot.borrow_mut() = Some(try_join_all([request]).await);
            })
            .expect("a live pool takes its tasks");
        pool.run_until_stalled();
        assert!(matches!(
            &*answer.borrow(),
            Some(Err(RequestError::Dropped))
        ));
    }
}
