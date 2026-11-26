//! Host operations on worlds that do not drive themselves.

use unlit_ecs::{Entity, EntityAccessError, World};

use super::{StepOutcome, SteppedWorld, WorldError, WorldKind, WorldProxy};
use crate::executor::LocalTasks;
use crate::time::FrameDelta;
use crate::turn::{Link, drain_jobs, run_turn};

/// Why a stepped world could not be driven through the entity that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StepError {
    /// The entity does not name a world proxy the host can reach.
    #[error("cannot reach the world proxy: {0}")]
    NoWorld(#[from] EntityAccessError),
    /// The entity names a threaded world, which drives itself.
    #[error("the world is threaded and drives itself")]
    NotStepped,
    /// The stepped world has stopped or is stopping and cannot be accessed.
    #[error("the stepped world is no longer available")]
    Unavailable,
}

/// Run one turn of a stepped world with `delta`.
///
/// Runs queued jobs, the tick, and then the commit and publication. A tick
/// that returns [`super::TickOutcome::Finished`] publishes once before ending. A
/// pending stop request skips the jobs and tick, publishes the final state,
/// and reports [`StepOutcome::Stopped`]. A panic or failed command ends the
/// world with its cause in [`StepOutcome::Failed`].
///
/// # Errors
///
/// Returns [`StepError::NoWorld`] for an unreachable proxy, [`StepError::NotStepped`]
/// for a threaded world, and [`StepError::Unavailable`] once the world has ended.
pub fn step_stepped_world(
    host: &World,
    entity: Entity,
    delta: FrameDelta,
) -> Result<StepOutcome, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&mut WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &mut proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() {
        return Err(StepError::Unavailable);
    }
    let SteppedWorld {
        world,
        inbox,
        control,
        status,
        tick,
        name,
    } = &mut **stepped;
    let link = Link {
        name,
        inbox,
        status,
    };
    Ok(run_turn(world, link, |world| {
        LocalTasks::pump(world);
        tick(world, control, delta)
    }))
}

/// A world that ended in failure while its host drained it.
///
/// The cause travels with the name, so a driver that gives up on a failed world
/// hands on what went wrong instead of only which world it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldFailure {
    /// The name the world was spawned under.
    pub name: &'static str,
    /// What ended the world.
    pub error: WorldError,
}

impl core::fmt::Display for WorldFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.name, self.error)
    }
}

/// Print the failures of several worlds as one clause.
///
/// A driver that gives up on a set of worlds reports all of them in one
/// message, so each world's name and cause read together.
#[must_use]
pub fn join_failures(failures: &[WorldFailure]) -> String {
    let mut out = String::new();
    for (index, failure) in failures.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("{failure}"));
    }
    out
}

/// Drain the queued jobs of every world the host steps.
///
/// This walks the proxies directly, so a caller that must answer a
/// command before its next frame does not need to gather entities into
/// a separate buffer first.
///
/// Returns the worlds that failed, including any that failed on an earlier
/// turn, with the cause each ended with. Their queues are closed and their
/// collectors no longer run.
pub fn drain_all_stepped_worlds(host: &World) -> Vec<WorldFailure> {
    let mut proxies = host.root_scope().query::<&mut WorldProxy>();
    let mut failed = Vec::new();
    for proxy in proxies.iter() {
        let WorldKind::Stepped(stepped) = &mut proxy.kind else {
            continue;
        };
        let SteppedWorld {
            world,
            inbox,
            status,
            name,
            ..
        } = &mut **stepped;
        let link = Link {
            name,
            inbox,
            status,
        };
        if let StepOutcome::Failed(error) = drain_jobs(world, link) {
            failed.push(WorldFailure { name, error });
        }
    }
    failed
}

/// Run `f` against a stepped world's world.
///
/// A host that supplies its own buffers - an offline run filling an audio
/// buffer frame by frame, say - reads the world through this instead of
/// through the world's jobs.
///
/// # Errors
///
/// Returns [StepError::NoWorld] when `entity` names no reachable world proxy,
/// [StepError::NotStepped] when it names a threaded world, and
/// [StepError::Unavailable] after the stepped world has stopped or received a
/// stop request.
pub fn with_stepped_world<R>(
    host: &World,
    entity: Entity,
    f: impl FnOnce(&World) -> R,
) -> Result<R, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() || stepped.status.stop_requested() {
        return Err(StepError::Unavailable);
    }
    Ok(f(&stepped.world))
}

/// Run `f` against a stepped world's world mutably.
///
/// The mutable twin of [with_stepped_world]: a caller that starts or stops a
/// recording on the world needs the world itself, not only its components.
///
/// # Errors
///
/// Returns [StepError::NoWorld] when `entity` names no reachable world proxy,
/// [StepError::NotStepped] when it names a threaded world, and
/// [StepError::Unavailable] after the stepped world has stopped or received a
/// stop request.
pub fn with_stepped_world_mut<R>(
    host: &World,
    entity: Entity,
    f: impl FnOnce(&mut World) -> R,
) -> Result<R, StepError> {
    let mut proxy = host.root_scope().try_query_one::<&mut WorldProxy>(entity)?;
    let WorldKind::Stepped(stepped) = &mut proxy.get().kind else {
        return Err(StepError::NotStepped);
    };
    if stepped.status.is_finished() || stepped.status.stop_requested() {
        return Err(StepError::Unavailable);
    }
    Ok(f(&mut stepped.world))
}
