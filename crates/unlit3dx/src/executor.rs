//! The async tasks a world runs, and the executor that drives them.
//!
//! Some work a world starts is asynchronous — a GPU request that resolves
//! once a device is ready, for instance — and [`crate::executor::LocalTasks`]
//! is the pool that carries it.
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

use unlit_ecs::bundle;
use unlit_ecs::{Entity, World};

pub use futures_executor::LocalPool;
pub use futures_util::task::LocalSpawnExt;

/// The async tasks a world runs, held as a component of that world.
///
/// Whoever drives the world pumps them, which is what [`LocalTasks::pump`]
/// does, so a task lasts as long as the world rather than as long as the call
/// that started it. A world holds at most one set: installation claims the
/// [`LocalTasks`] singleton key.
pub struct LocalTasks(LocalPool);

impl LocalTasks {
    /// An empty task set.
    #[must_use]
    pub fn new() -> Self {
        Self(LocalPool::new())
    }

    /// The entity holding `world`'s task set, if it has one.
    fn entity(world: &World) -> Option<Entity> {
        world.root_scope().singleton::<LocalTasks>()
    }

    /// Give `world` a task set, unless it already has one.
    ///
    /// Every world the framework builds is given one this way at creation,
    /// before its setup runs, so the setup can spawn a task of its own.
    pub fn install(world: &mut World) {
        if Self::entity(world).is_none() {
            world
                .root_scope_mut()
                .spawn(bundle![Self::new()].singleton::<LocalTasks>());
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
    /// turn with it, exactly as a panicking job or tick would. The other
    /// tasks keep their pool: the unwind drops the guard, and the guard puts
    /// the pool back first.
    pub fn pump(world: &mut World) {
        Self::install(world);
        // The pool runs without the borrow held: a polled task that reaches
        // the world again — through a job it queued, for one — must not meet
        // a claim the pump still holds. `Restore` puts it back whether the run
        // returns or unwinds.
        let Some(entity) = Self::entity(world) else {
            return;
        };
        let pool = {
            let Ok(mut local_tasks) = world.root_scope().try_query_one::<&mut LocalTasks>(entity)
            else {
                return;
            };
            core::mem::take(&mut local_tasks.get().0)
        };
        let mut running = Restore {
            world,
            entity,
            pool: Some(pool),
        };
        if let Some(pool) = running.pool.as_mut() {
            pool.run_until_stalled();
        }
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
        let Some(entity) = Self::entity(world) else {
            return false;
        };
        let Ok(mut local_tasks) = world.root_scope().try_query_one::<&LocalTasks>(entity) else {
            return false;
        };
        local_tasks.get().0.spawner().spawn_local(future).is_ok()
    }
}

/// Puts a running task set back into the world it came from.
///
/// The pump runs the pool outside the world, so the pool is absent while a
/// task is polled. Dropping this guard returns it, which is what makes a
/// panicking task take only its own turn: the unwind walks the guard, and the
/// tasks that were still waiting are in the pool the next pump runs.
struct Restore<'a> {
    /// The world the pool belongs to.
    world: &'a mut World,
    /// The entity holding the task set.
    entity: Entity,
    /// The pool, until the guard puts it back.
    pool: Option<LocalPool>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else {
            return;
        };
        if let Ok(mut local_tasks) = self
            .world
            .root_scope()
            .try_query_one::<&mut LocalTasks>(self.entity)
        {
            local_tasks.get().0 = pool;
        }
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

    /// A task that panics leaves the pool behind, so the tasks it was
    /// running beside still have an owner and the next pump drives them.
    #[test]
    fn a_panicking_task_leaves_the_pool_in_place() {
        let mut world = World::new();
        LocalTasks::pump(&mut world);
        assert!(
            LocalTasks::spawn(&world, async { panic!("the task died") }),
            "the installed pool takes the task"
        );
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            LocalTasks::pump(&mut world);
        }));
        assert!(panicked.is_err(), "the panic reached the caller");
        // The pool came back with the unwind, so a later task still runs.
        let seen: Rc<RefCell<bool>> = Rc::default();
        let slot = Rc::clone(&seen);
        assert!(
            LocalTasks::spawn(&world, async move {
                *slot.borrow_mut() = true;
            }),
            "the pool is still in the world"
        );
        LocalTasks::pump(&mut world);
        assert!(*seen.borrow(), "the next pump ran the later task");
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
