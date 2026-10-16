//! The async tasks a world runs, and the executor that drives them.
//!
//! [`crate::job::Request`] is a future, so a scene build that queues a batch
//! of assets and waits for the answers reads as one `async` block:
//! [`crate::executor::try_join_all`] joins a batch into a single future of
//! the answers in request order, and the
//! [`crate::executor::LocalTasks`] a world carries drives it.
//!
//! Both are the ecosystem's own, re-exported so a caller needs no futures
//! dependency of their own. The executor is the one that fits a frame loop:
//! it polls every task it holds until none can make progress and never
//! blocks, and whoever drives the world pumps it — a thread's tick loop
//! before the tick, a host's frame before the dispatch. A task woken by
//! another thread, a remote world answering a request, is picked up by the
//! next pump.
//!
//! A spawned task is `'static`, so what it lands has to be shared with the
//! code that reads it: an `Rc<RefCell<..>>` slot that the tick drains, as
//! the demo's scene build does.
//!
//! # A world owns its tasks
//!
//! The tasks live in the world they belong to, as a
//! [`LocalTasks`](crate::executor::LocalTasks) component, rather than in a
//! [`LocalPool`](crate::executor::LocalPool) the caller keeps beside it.
//! A task therefore lasts as long as the world does, whatever call started
//! it, and no caller has to thread an executor through to the code that
//! spawns. Every world the framework builds is given one at creation, before
//! its setup runs; [`LocalTasks::pump`](crate::executor::LocalTasks::pump)
//! gives one to a world that lacks it,
//! which is how the host world, the one the caller made itself, gets its
//! own.

use std::future::Future;

use unlit_ecs::World;

pub use futures_executor::LocalPool;
pub use futures_util::future::try_join_all;
pub use futures_util::task::LocalSpawnExt;

/// The async tasks a world runs, held as a component of that world.
///
/// Whoever drives the world pumps them, which is what [`LocalTasks::pump`]
/// does, so a task lasts as long as the world rather than as long as the call
/// that started it. A world holds at most one set.
pub struct LocalTasks(LocalPool);

impl LocalTasks {
    /// An empty task set.
    #[must_use]
    pub fn new() -> Self {
        Self(LocalPool::new())
    }

    /// Give `world` a task set, unless it already has one.
    ///
    /// Every world the framework builds is given one this way at creation,
    /// before its setup runs, so the setup can spawn a task of its own.
    pub fn install(world: &mut World) {
        if world.query::<&LocalTasks>().next().is_none() {
            world.spawn((Self::new(),));
        }
    }

    /// Drive `world`'s tasks until none can make progress.
    ///
    /// Call this once per turn of whatever drives the world: a thread's tick
    /// loop before the tick, a host's frame before the dispatch. A world the
    /// caller made itself is given its task set here, so its tasks have an
    /// owner from its first pumped turn on.
    ///
    /// A task that panics unwinds out of the pump and takes the caller's
    /// turn with it, exactly as a panicking job or tick would.
    pub fn pump(world: &mut World) {
        Self::install(world);
        let Some((_, mut tasks)) = world.query::<&mut LocalTasks>().next() else {
            return;
        };
        tasks.0.run_until_stalled();
    }

    /// Hand `future` to the tasks `world` runs, answering whether it took it.
    ///
    /// The task is polled from the world's next pump on. A world with no task
    /// set answers `false`: the framework gives one to every world it builds,
    /// and [`LocalTasks::pump`] gives one to a world that lacks it.
    pub fn spawn<F>(world: &World, future: F) -> bool
    where
        F: Future<Output = ()> + 'static,
    {
        let Some((_, tasks)) = world.query::<&LocalTasks>().next() else {
            return false;
        };
        tasks.0.spawner().spawn_local(future).is_ok()
    }
}

impl Default for LocalTasks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use unlit_ecs::World;

    use super::*;
    use crate::job::{Request, RequestError};

    /// A world's first pump gives it a task set, and later ones drive it.
    #[test]
    fn a_worlds_tasks_are_installed_by_the_first_pump() {
        let mut world = World::new();
        let seen: Rc<RefCell<bool>> = Rc::default();
        // A world with no task set refuses the task rather than dropping it.
        let slot = Rc::clone(&seen);
        assert!(!LocalTasks::spawn(&world, async move {
            *slot.borrow_mut() = true;
        }));
        LocalTasks::pump(&mut world);
        let slot = Rc::clone(&seen);
        assert!(LocalTasks::spawn(&world, async move {
            *slot.borrow_mut() = true;
        }));
        assert!(!*seen.borrow(), "spawning does not poll");
        LocalTasks::pump(&mut world);
        assert!(*seen.borrow());
    }

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
