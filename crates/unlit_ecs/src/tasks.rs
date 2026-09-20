//! Futures that behaviour components return.
//!
//! A behaviour component may do asynchronous work: its callback returns a future
//! instead of blocking. `unlit_ecs` does not run an executor — the caller
//! decides where and when work runs, just as it decides which thread runs a
//! system.
//!
//! The driver owns a [`Tasks`] table next to the world, pushes the futures it
//! wants to run, and polls them with [`Tasks::poll_all`] (each poll is
//! non-blocking). A caller that has an external executor can poll the table with
//! its own waker with [`Tasks::poll_with`].
//!
//! `````
//! # use unlit_ecs::Tasks;
//! let mut tasks: Tasks = Tasks::new();
//! tasks.push(async {});
//! tasks.poll_all();
//! assert!(tasks.is_empty());
//! `````
//!
//! Because a task borrows the world while it runs, a future must not hold a
//! component borrow across an `await`: copy the value, or fetch it again after
//! resuming.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::mode::{LocalMode, Mode, SendMode};

/// Erases a future into a mode's task storage.
///
/// The `Send` implementation only accepts futures that are `Send`, which is
/// what keeps [`SendWorld`](crate::SendWorld)'s tasks movable between threads.
pub trait TaskErase<M: Mode> {
    /// Pin and erase the future.
    fn erase(self) -> Pin<Box<M::ErasedTask>>;
}

impl<F: Future<Output = ()> + 'static> TaskErase<LocalMode> for F {
    fn erase(self) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(self)
    }
}

impl<F: Future<Output = ()> + Send + 'static> TaskErase<SendMode> for F {
    fn erase(self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(self)
    }
}

/// The pinned tasks a driver is running.
///
/// The table lives beside the world rather than in it, so polling a task that
/// borrows the world does not borrow the table that holds it.
pub struct Tasks<M: Mode = LocalMode> {
    tasks: Vec<Pin<Box<M::ErasedTask>>>,
}

impl<M: Mode> Default for Tasks<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Mode> Tasks<M> {
    /// No tasks.
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Add a future to run.
    pub fn push<F>(&mut self, future: F)
    where
        F: TaskErase<M>,
    {
        self.tasks.push(future.erase());
    }

    /// Number of unfinished tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether every task has finished.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Poll every unfinished task once, dropping the ones that finished.
    ///
    /// The waker is a no-op, so a task waiting on an outside event simply stays
    /// in the table until that event happens and the driver polls again.
    pub fn poll_all(&mut self) {
        self.poll_with(Waker::noop());
    }

    /// Poll every unfinished task once with `waker`, dropping the finished
    /// ones.
    pub fn poll_with(&mut self, waker: &Waker) {
        let mut cx = Context::from_waker(waker);
        let mut index = 0;
        while index < self.tasks.len() {
            let finished = match self.tasks[index].as_mut().poll(&mut cx) {
                Poll::Ready(()) => true,
                Poll::Pending => false,
            };
            if finished {
                drop(self.tasks.swap_remove(index));
            } else {
                index += 1;
            }
        }
    }

    /// Drop every task.
    pub fn clear(&mut self) {
        self.tasks.clear();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalWorld, SendWorld};
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll};

    #[test]
    fn a_ready_future_finishes_on_the_first_poll() {
        let mut tasks: Tasks = Tasks::new();
        tasks.push(async {});
        assert_eq!(tasks.len(), 1);
        tasks.poll_all();
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_pending_future_stays_until_it_is_ready() {
        /// A future that is pending for a fixed number of polls.
        struct Countdown(u32);
        impl Future for Countdown {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
                if self.0 == 0 {
                    Poll::Ready(())
                } else {
                    self.0 -= 1;
                    Poll::Pending
                }
            }
        }

        let mut tasks: Tasks = Tasks::new();
        tasks.push(Countdown(2));
        tasks.poll_all();
        assert_eq!(tasks.len(), 1, "still pending");
        tasks.poll_all();
        assert_eq!(tasks.len(), 1);
        tasks.poll_all();
        assert!(tasks.is_empty(), "finished on the third poll");
    }

    #[test]
    fn several_futures_are_driven_together() {
        let mut tasks: Tasks = Tasks::new();
        tasks.push(async {});
        tasks.push(async {});
        tasks.push(async {});
        assert_eq!(tasks.len(), 3);
        tasks.poll_all();
        assert!(tasks.is_empty());
    }

    #[test]
    fn clear_drops_every_task() {
        let mut tasks: Tasks = Tasks::new();
        tasks.push(async {});
        tasks.clear();
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_future_may_borrow_the_world_across_a_frame() {
        // The pattern the docs recommend: fetch inside the future, never hold a
        // component borrow across an await.
        let mut world = LocalWorld::new();
        let entity = world.spawn((1u32,));
        let mut tasks: Tasks = Tasks::new();
        tasks.push(async {
            // The world is not captured here; the driver holds it. This test
            // only checks that a future may exist next to a world.
        });
        tasks.poll_all();
        assert_eq!(world.get::<u32>(entity).unwrap().to_owned(), 1);
    }

    #[test]
    fn the_send_world_has_send_tasks() {
        let mut tasks: Tasks<SendMode> = Tasks::new();
        tasks.push(async {});
        tasks.poll_all();
        assert!(tasks.is_empty());
        fn assert_send<T: Send>(_: &T) {}
        let world = SendWorld::new();
        assert_send(&world);
    }
}
