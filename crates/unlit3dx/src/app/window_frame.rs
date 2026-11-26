//! One window frame transaction, shared by both clocks.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{Entity, World};
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};
use unlit3d::winit::builtin::ExitRequest;
use unlit3d::winit::presenter::{PRESENT_ORDER, Presenter};

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use super::Capture;
use super::error::RunError;
use super::{Pacing, RunFrames, Stepping};
use crate::host::drain_sub_world;
use crate::present::{TurnError, TurnOutcome, present_turn};
use crate::time::FrameDelta;
use crate::world::{StepOutcome, drain_all_stepped_worlds};

#[derive(Clone, Default)]
pub(super) struct RunFailure(Rc<RefCell<Option<RunError>>>);

impl RunFailure {
    pub(super) fn report(&self, world: &World, app: Entity, error: RunError) {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            log::error!("{error}");
            *slot = Some(error);
        }
        exit(world, app);
    }

    pub(super) fn take(&self) -> Option<RunError> {
        self.0.borrow_mut().take()
    }
}

/// What one window frame came to.
///
/// The ways a frame ends are told apart by variant rather than folded into a
/// `Result<bool, String>`: a spent frame budget and a scene whose worlds have
/// all ended both stop the run without an error, a surface with no image is
/// left to the next turn, and only [`FrameOutcome::Failed`] carries a message
/// for the run to record. A caller therefore decides once per variant what to
/// do, instead of re-deriving the case from a boolean it had to guess at.
#[derive(Debug)]
pub(super) enum FrameOutcome {
    /// The frame was drawn and presented.
    Presented,
    /// The surface had no image to draw into, so nothing was drawn and the
    /// next turn tries again. No world ticked: a frame nobody draws is no
    /// time passing.
    Skipped,
    /// No frame will be drawn again, because the run's frame budget ran out
    /// or the scene ended. The exit has already been requested.
    Ended,
    /// The frame could not be taken, and the run cannot continue. The caller
    /// records the error, which stops the run.
    Failed(RunError),
}

pub(super) struct WindowFrames {
    pub(super) render: Entity,
    pub(super) app: Entity,
    pub(super) stepper: Option<Stepping>,
    pub(super) frames: RunFrames,
    pub(super) pacing: Pacing,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) capture: Option<Capture>,
    pub(super) failure: RunFailure,
}

impl WindowFrames {
    /// Take one frame: step the worlds, draw, and present.
    ///
    /// The worlds are stepped only once a surface image is in hand, so a run
    /// whose window is hidden or whose surface is being rebuilt stands still
    /// rather than banking time it would then have to catch up on.
    pub(super) fn submit(&mut self, world: &World, delta_time: FrameDelta) -> FrameOutcome {
        if self.pacing.is_spent(self.frames.advanced()) {
            exit(world, self.app);
            return FrameOutcome::Ended;
        }
        if self.failure.0.borrow().is_some() {
            return FrameOutcome::Failed(RunError::AlreadyFailed);
        }
        let index = self.frames.advanced();
        let mut frame = Frame { delta_time, index };
        let result = present_turn(world, self.render, &mut frame, || {
            let outcome = self
                .stepper
                .as_mut()
                .map_or(StepOutcome::Ran, |stepper| stepper.step(world, delta_time));
            match outcome {
                StepOutcome::Ran => Ok(()),
                outcome => Err(outcome),
            }
        });
        match result {
            Ok(TurnOutcome::Presented) => {
                self.frames.advance();
                #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
                if let Some(capture) = &self.capture
                    && let Err(error) =
                        super::capture::capture_presented(world, self.render, capture, index)
                {
                    return FrameOutcome::Failed(error.into());
                }
                if self.pacing.is_spent(self.frames.advanced()) {
                    exit(world, self.app);
                    return FrameOutcome::Ended;
                }
                FrameOutcome::Presented
            }
            // The host is suspended: it draws again when it is resumed, and no
            // world ticks while it is away.
            Ok(TurnOutcome::Suspended) => FrameOutcome::Skipped,
            // Nothing can be presented yet: the render world's clients are
            // still waiting on its setup, so nothing is drained either.
            Ok(TurnOutcome::Unprepared) => FrameOutcome::Skipped,
            Ok(TurnOutcome::Skipped) => match self.service_idle(world) {
                Ok(()) => FrameOutcome::Skipped,
                Err(error) => FrameOutcome::Failed(error.into()),
            },
            // A simulation that ended is the run ending, unless it failed.
            Err(TurnError::Simulation(StepOutcome::Failed(error))) => {
                exit(world, self.app);
                FrameOutcome::Failed(RunError::Scene(error))
            }
            Err(error @ TurnError::Simulation(_)) => {
                exit(world, self.app);
                let _ = error;
                FrameOutcome::Ended
            }
            Err(error) => FrameOutcome::Failed(error.into()),
        }
    }

    fn service_idle(&self, world: &World) -> Result<(), TurnError> {
        // Until the presentation path exists the render world's setup has not
        // finished, and the jobs queued for it assume it has.
        if world
            .root_scope()
            .query::<&Presenter>()
            .iter()
            .next()
            .is_none()
        {
            return Ok(());
        }
        let failed = drain_all_stepped_worlds(world);
        if !failed.is_empty() {
            return Err(TurnError::Jobs(failed));
        }
        let mut render = world
            .root_scope()
            .try_query_one::<&mut World>(self.render)
            .map_err(TurnError::RenderWorld)?;
        match drain_sub_world(render.get()) {
            StepOutcome::Ran => Ok(()),
            StepOutcome::Finished | StepOutcome::Stopped => {
                exit(world, self.app);
                Ok(())
            }
            outcome => Err(TurnError::Render(outcome)),
        }
    }
}

pub(super) fn drive_window() -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER),
        |world, entity, frame| {
            let Ok(mut row) = world
                .root_scope()
                .try_query_one::<&mut WindowFrames>(entity)
            else {
                return;
            };
            let state = row.get();
            if state.pacing.is_spent(state.frames.advanced()) {
                exit(world, state.app);
            } else if state.pacing.client {
                if let Err(error) = state.service_idle(world) {
                    state.failure.report(world, state.app, error.into());
                }
            } else if let FrameOutcome::Failed(error) = state.submit(world, frame.delta_time) {
                state.failure.report(world, state.app, error);
            }
        },
    )
}

fn exit(world: &World, app: Entity) {
    if let Ok(mut request) = world.root_scope().try_query_one::<&mut ExitRequest>(app) {
        request.get().0 = true;
    }
}
