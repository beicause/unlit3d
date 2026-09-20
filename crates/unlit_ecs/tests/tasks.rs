//! Asynchronous behaviour: futures a driver polls beside the world.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use unlit_ecs::{LocalWorld, Tasks};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Progress(u32);

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

#[test]
fn a_ready_future_finishes_on_the_first_poll() {
    let mut tasks: Tasks = Tasks::new();
    tasks.push(async {});

    assert_eq!(tasks.len(), 1);
    assert!(!tasks.is_empty());

    tasks.poll_all();
    assert!(tasks.is_empty());
}

#[test]
fn a_pending_future_stays_until_it_is_ready() {
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
    // Countdown(n) is ready on poll n + 1.
    let mut tasks: Tasks = Tasks::new();
    tasks.push(Countdown(1));
    tasks.push(async {});
    tasks.push(Countdown(2));

    assert_eq!(tasks.len(), 3);
    tasks.poll_all();
    assert_eq!(tasks.len(), 2, "the ready future left");
    tasks.poll_all();
    assert_eq!(tasks.len(), 1, "the one-poll future left");
    tasks.poll_all();
    assert!(tasks.is_empty(), "the last one left");
}

#[test]
fn clear_drops_every_unfinished_task() {
    let finished = Rc::new(Cell::new(false));
    let watched = finished.clone();

    let mut tasks: Tasks = Tasks::new();
    tasks.push(async move {
        watched.set(true);
    });
    tasks.clear();

    assert!(tasks.is_empty());
    tasks.poll_all();
    assert!(!finished.get(), "the dropped future never ran");
}

#[test]
fn a_finished_task_already_in_the_table_is_not_polled_again() {
    let mut inner_tasks: Tasks = Tasks::new();
    inner_tasks.push(async {});
    inner_tasks.poll_all();
    assert!(inner_tasks.is_empty());

    // Polling an empty table is harmless.
    inner_tasks.poll_all();
    assert!(inner_tasks.is_empty());
}

#[test]
fn a_future_can_read_the_world_between_polls() {
    // The pattern the docs recommend: fetch, copy, and never hold a component
    // borrow across a suspension point.
    let mut world = LocalWorld::new();
    let entity = world.spawn((Progress(0),));
    let mut tasks: Tasks = Tasks::new();

    let seen = Rc::new(Cell::new(0));
    let observed = seen.clone();
    tasks.push(async move {
        observed.set(1);
    });

    // The driver advances the world, then polls the tasks.
    let _ = world.with_mut::<Progress, _>(entity, |progress| progress.0 += 1);
    tasks.poll_all();

    assert_eq!(seen.get(), 1);
    assert_eq!(world.get::<Progress>(entity).unwrap().0, 1);
}

#[test]
fn a_driver_can_keep_polling_until_the_work_is_done() {
    let mut world = LocalWorld::new();
    let entity = world.spawn((Progress(0),));
    let mut tasks: Tasks = Tasks::new();
    tasks.push(Countdown(4));

    let mut frames = 0;
    while !tasks.is_empty() {
        let _ = world.with_mut::<Progress, _>(entity, |progress| progress.0 += 1);
        tasks.poll_all();
        frames += 1;
        assert!(frames < 100, "the driver must terminate");
    }

    assert_eq!(frames, 5, "four pending polls and the ready one");
    assert_eq!(world.get::<Progress>(entity).unwrap().0, 5);
}
