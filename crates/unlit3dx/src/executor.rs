//! The async tasks a world runs, and the executor that drives them.
//!
//! Some work a world starts is asynchronous — a GPU request that resolves
//! once a device is ready, for instance — and [`crate::executor::LocalTasks`]
//! is the pool that carries it. A world owns its tasks, so a task lasts as
//! long as the world does, whatever call started it.
//!
//! The executor is the ecosystem's own, re-exported so a caller needs no
//! futures dependency of their own. It is the one that fits a frame loop: it
//! polls every task it holds until none can make progress and never blocks,
//! and whoever drives the world pumps it — a thread's tick loop before the
//! tick, a host's frame before the dispatch.
//!
//! A spawned task is `'static`, so what it lands has to be shared with the
//! code that reads it: an `Rc<RefCell<..>>` slot that the tick drains, as a
//! GPU request does.
//!
//! # A world owns its tasks
//!
//! The tasks live in the world they belong to, as [a `LocalTasks`](crate::executor::LocalTasks)
//! component, rather than in [a `LocalPool`](crate::executor::LocalPool) the caller keeps
//! beside it.
//! A task therefore lasts as long as the world does, whatever call started
//! it, and no caller has to thread an executor through to the code that
//! spawns. Every world the framework builds is given one at creation, before
//! its setup runs; [`LocalTasks::pump`](crate::executor::LocalTasks::pump)
//! gives one to a world that lacks it,
//! which is how the host world, the one the caller made itself, gets its
//! own.

use std::future::Future;

use unlit_ecs::World;
use unlit_ecs::bundle;

pub use futures_executor::LocalPool;
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
        if world
            .root_scope()
            .query::<&LocalTasks>()
            .iter()
            .next()
            .is_none()
        {
            world.root_scope_mut().spawn(bundle![Self::new()]);
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
        let mut local_tasks_rows = world.root_scope().query::<&mut LocalTasks>();

        let Some(local_tasks) = local_tasks_rows.iter().next() else {
            return;
        };
        local_tasks.0.run_until_stalled();
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
        let mut local_tasks_rows_2 = world.root_scope().query::<&LocalTasks>();

        let Some(local_tasks) = local_tasks_rows_2.iter().next() else {
            return false;
        };
        local_tasks.0.spawner().spawn_local(future).is_ok()
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
}
