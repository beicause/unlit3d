//! The turn every world runs, whichever driver runs it.
//!
//! A threaded world, a stepped world and a sub-world turn the same way, so
//! their order, their failures and what they publish cannot drift apart. A
//! world that has ended is left alone. Otherwise the jobs queued before the
//! turn run, their commands apply, the body runs, and what the body queued
//! applies before the collectors publish. Each stage is contained: a panic or
//! a failed command ends the world and is recorded as its state, so no driver
//! has to guess what a half-run turn left behind.

use std::sync::Arc;

use async_channel::Receiver;
use unlit_ecs::{EntityAccessError, World};

use crate::channel::publish_collected;
use crate::job::Job;
use crate::thread::{ThreadStatus, WorldState, panic_message};
use crate::world::StepError;

/// What one turn of a world did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StepOutcome {
    /// The turn ran and the world carries on.
    Ran,
    /// The world had already ended, or its body ended it.
    Finished,
    /// A stop request ended the world before its body ran.
    Stopped,
    /// A job, the body or the commit panicked or failed, and the world ended
    /// with that as its state.
    Failed(WorldError),
}

impl core::fmt::Display for StepOutcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ran => f.write_str("still running"),
            Self::Finished => f.write_str("finished"),
            Self::Stopped => f.write_str("stopped"),
            Self::Failed(error) => write!(f, "failed: {error}"),
        }
    }
}

/// Which part of a turn failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorldStage {
    /// The world could not be set up on its thread.
    Setup,
    /// A queued job panicked.
    Job,
    /// A queued command panicked or was refused.
    Command,
    /// The tick panicked.
    Body,
    /// A collector panicked.
    Publish,
}

impl core::fmt::Display for WorldStage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Setup => "setup",
            Self::Job => "job",
            Self::Command => "queued command",
            Self::Body => "tick",
            Self::Publish => "collector",
        };
        f.write_str(name)
    }
}

/// Why a world ended in failure.
///
/// Carries the cause rather than only its text, so a driver that reports a
/// failure hands on what went wrong: the command that was refused, the step
/// that could not be taken, or the panic and the stage it came from. Cloning is
/// cheap, because a failure is recorded and then reported by several readers.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorldError {
    /// A job, the tick or a collector panicked.
    #[error("the {stage} panicked: {message}")]
    Panic {
        /// Which stage of the turn panicked.
        stage: WorldStage,
        /// What the panic carried.
        message: Arc<str>,
    },
    /// A queued command could not be applied.
    #[error("a queued command failed: {source}")]
    Command {
        /// What the world reported when it refused the command.
        #[source]
        source: EntityAccessError,
    },
    /// A stepped world could not be driven at all.
    #[error("the world could not be driven: {source}")]
    Step {
        /// Why the world could not be reached or run.
        #[source]
        source: StepError,
    },
}

/// The inbox and state a driver reads each turn.
///
/// Borrowed rather than owned, so a driver can hand the world to the turn
/// mutably in the same call.
#[derive(Clone, Copy)]
pub(crate) struct Link<'a> {
    /// The name the world logs under.
    pub(crate) name: &'a str,
    /// The jobs queued for the world.
    pub(crate) inbox: &'a Receiver<Job>,
    /// The state the world publishes.
    pub(crate) status: &'a ThreadStatus,
}

/// Run one turn: the queued jobs, the body, then the commit.
///
/// The body runs once and returns whether the world carries on. A `false`
/// ends the world after its commit, so the state it leaves is the last one its
/// readers see. A stop asked before the turn ends the world without running
/// anything.
pub(crate) fn run_turn(
    world: &mut World,
    link: Link<'_>,
    body: impl FnOnce(&mut World) -> bool,
) -> StepOutcome {
    if let Some(outcome) = admit(world, link) {
        return outcome;
    }
    if let Err(outcome) = run_jobs(world, link) {
        return outcome;
    }
    let carries_on = match contain(WorldStage::Body, || body(&mut *world)) {
        Ok(carries_on) => carries_on,
        Err(error) => return fail(link, error),
    };
    if let Err(outcome) = commit(world, link) {
        return outcome;
    }
    if carries_on {
        StepOutcome::Ran
    } else {
        end(link);
        StepOutcome::Finished
    }
}

/// Run the jobs queued for a world without a tick, then publish its collectors.
///
/// A sub-world has no tick of its own, but its host still gives it a complete
/// observable turn: jobs apply first and collectors publish afterward. This
/// keeps subscriptions useful for nested render and service worlds.
pub(crate) fn drain_jobs(world: &mut World, link: Link<'_>) -> StepOutcome {
    if let Some(outcome) = admit(world, link) {
        return outcome;
    }
    if let Err(outcome) = run_jobs(world, link) {
        return outcome;
    }
    match publish(world, link) {
        Ok(()) => StepOutcome::Ran,
        Err(outcome) => outcome,
    }
}

/// Whether the world has already ended, or must end before running anything.
fn admit(world: &mut World, link: Link<'_>) -> Option<StepOutcome> {
    match link.status.current().as_deref() {
        Some(WorldState::Failed(error)) => return Some(StepOutcome::Failed(error.clone())),
        Some(WorldState::Stopped) => return Some(StepOutcome::Finished),
        _ => {}
    }
    if !link.status.stop_requested() {
        return None;
    }
    // A stop is not a failure, so the world collects one last time: the state
    // it had reached is the last its readers ever get.
    if let Err(outcome) = publish(world, link) {
        return Some(outcome);
    }
    end(link);
    Some(StepOutcome::Stopped)
}

/// Run the jobs queued before this call, then apply what they queued.
///
/// Only jobs already in the queue run: one that arrives while the others run
/// waits for the next turn, so a producer that outpaces the world cannot hold
/// the turn open.
fn run_jobs(world: &mut World, link: Link<'_>) -> Result<(), StepOutcome> {
    for _ in 0..link.inbox.len() {
        let Ok(job) = link.inbox.try_recv() else {
            break;
        };
        if let Err(error) = contain(WorldStage::Job, || job(&mut *world)) {
            return Err(fail(link, error));
        }
    }
    apply(world, link)
}

/// Apply the commands queued on the world. A failed command ends the world:
/// the commands after it would run against a state the caller never meant.
fn apply(world: &mut World, link: Link<'_>) -> Result<(), StepOutcome> {
    match contain(WorldStage::Command, || world.apply()) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(fail(link, WorldError::Command { source })),
        Err(error) => Err(fail(link, error)),
    }
}

/// Apply the commands the body queued, then publish what the collectors return.
fn commit(world: &mut World, link: Link<'_>) -> Result<(), StepOutcome> {
    apply(world, link)?;
    publish(world, link)
}

/// Publish what the world's collectors return. A collector is arbitrary code,
/// so a panic in one ends the world rather than reaching the driver.
fn publish(world: &World, link: Link<'_>) -> Result<(), StepOutcome> {
    match contain(WorldStage::Publish, || publish_collected(world)) {
        Ok(_) => Ok(()),
        Err(error) => Err(fail(link, error)),
    }
}

/// Run `f`, turning a panic into the error that names its stage.
fn contain<R>(stage: WorldStage, f: impl FnOnce() -> R) -> Result<R, WorldError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| WorldError::Panic {
        stage,
        message: panic_message(payload.as_ref()).into(),
    })
}

/// End the world with `error` as its state, and close its queue so a later
/// send is refused instead of waiting for a turn that will never come.
fn fail(link: Link<'_>, error: WorldError) -> StepOutcome {
    log::error!("the world {} failed: {error}", link.name);
    link.status.record_failure(error.clone());
    close_inbox(link.inbox);
    StepOutcome::Failed(error)
}

/// Mark the world finished and close its queue.
fn end(link: Link<'_>) {
    link.status.finish();
    close_inbox(link.inbox);
}

/// Drop unrun jobs so callers awaiting their replies can observe cancellation.
pub(crate) fn close_inbox(inbox: &Receiver<Job>) {
    inbox.close();
    while inbox.try_recv().is_ok() {}
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_channel::Sender;

    use super::*;
    use crate::thread::WorldState;

    /// A queue and a status a test drives a turn with.
    fn harness() -> (Sender<Job>, Receiver<Job>, ThreadStatus) {
        let (sender, inbox) = async_channel::bounded(8);
        (sender, inbox, ThreadStatus::detached())
    }

    #[test]
    fn a_turn_runs_its_body_and_carries_on() {
        let (_sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        let ran = AtomicUsize::new(0);
        let outcome = run_turn(&mut world, link, |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(outcome, StepOutcome::Ran);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(!status.is_finished());
    }

    #[test]
    fn a_body_that_declines_ends_the_world_after_its_commit() {
        let (sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        let outcome = run_turn(&mut world, link, |_| false);
        assert_eq!(outcome, StepOutcome::Finished);
        assert!(status.is_finished());
        assert!(sender.is_closed());
    }

    #[test]
    fn a_finished_world_runs_nothing() {
        let (_sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        status.finish();
        let ran = AtomicUsize::new(0);
        let outcome = run_turn(&mut world, link, |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(outcome, StepOutcome::Finished);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_stop_ends_the_world_before_its_body_runs() {
        let (sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        status.stop();
        let ran = AtomicUsize::new(0);
        let outcome = run_turn(&mut world, link, |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(outcome, StepOutcome::Stopped);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(status.is_finished());
        assert!(sender.is_closed());
    }

    #[test]
    fn a_job_that_panics_ends_the_world_before_its_body_runs() {
        let (sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        let job: Job = Box::new(|_: &mut World| panic!("the job died"));
        sender.try_send(job).expect("the queue has room");
        let ran = AtomicUsize::new(0);
        let outcome = run_turn(&mut world, link, |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            true
        });
        let died = WorldError::Panic {
            stage: WorldStage::Job,
            message: "the job died".into(),
        };
        assert_eq!(outcome, StepOutcome::Failed(died.clone()));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(sender.is_closed());
        assert_eq!(
            status.state().latest().as_deref(),
            Some(&WorldState::Failed(died))
        );
    }

    #[test]
    fn a_body_that_panics_ends_the_world_with_its_message() {
        let (_sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        let outcome = run_turn(&mut world, link, |_| panic!("the body died"));
        let died = WorldError::Panic {
            stage: WorldStage::Body,
            message: "the body died".into(),
        };
        assert_eq!(outcome, StepOutcome::Failed(died.clone()));
        assert_eq!(
            status.state().latest().as_deref(),
            Some(&WorldState::Failed(died))
        );
    }

    #[test]
    fn a_job_queued_during_a_turn_waits_for_the_next_turn() {
        let (sender, inbox, status) = harness();
        let link = Link {
            name: "test",
            inbox: &inbox,
            status: &status,
        };
        let mut world = World::new();
        let ran = std::sync::Arc::new(AtomicUsize::new(0));
        let queued_again = sender.clone();
        let seen = std::sync::Arc::clone(&ran);
        let first: Job = Box::new(move |_: &mut World| {
            let later: Job = Box::new(move |_: &mut World| {
                seen.fetch_add(1, Ordering::SeqCst);
            });
            queued_again.try_send(later).expect("the queue has room");
        });
        sender.try_send(first).expect("the queue has room");
        assert_eq!(run_turn(&mut world, link, |_| true), StepOutcome::Ran);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(run_turn(&mut world, link, |_| true), StepOutcome::Ran);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
