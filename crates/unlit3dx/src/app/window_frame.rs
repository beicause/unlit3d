//! One window frame transaction, shared by both clocks.

use std::cell::RefCell;
use std::rc::Rc;

use unlit_ecs::{Entity, World};
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};
use unlit3d::winit::builtin::ExitRequest;
use unlit3d::winit::presenter::PRESENT_ORDER;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use super::Capture;
use super::{Pacing, RunFrames, Stepping};
use crate::host::drain_sub_world;
use crate::present::present_turn;
use crate::world::{StepOutcome, drain_all_stepped_worlds};

#[derive(Clone, Default)]
pub(super) struct RunFailure(Rc<RefCell<Option<String>>>);

impl RunFailure {
    pub(super) fn report(&self, world: &World, app: Entity, message: String) {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            log::error!("{message}");
            *slot = Some(message);
        }
        exit(world, app);
    }

    pub(super) fn take(&self) -> Option<String> {
        self.0.borrow_mut().take()
    }
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
    /// Return false when the surface is unavailable or the run is done.
    pub(super) fn submit(&mut self, world: &World, delta_time: f32) -> Result<bool, String> {
        if self.pacing.is_spent(self.frames.advanced()) {
            exit(world, self.app);
            return Ok(false);
        }
        if self.failure.0.borrow().is_some() {
            return Err("the windowed run has already failed".to_owned());
        }
        let index = self.frames.advanced();
        let mut frame = Frame { delta_time, index };
        let mut finished = false;
        let result = present_turn(world, self.render, &mut frame, || {
            match self
                .stepper
                .as_mut()
                .map(|stepper| stepper.step(world, f64::from(delta_time)))
            {
                None | Some(StepOutcome::Ran) => Ok(()),
                Some(StepOutcome::Finished | StepOutcome::Stopped) => {
                    finished = true;
                    Err("the scene ended".to_owned())
                }
                Some(StepOutcome::Failed) => Err("a simulation world failed".to_owned()),
            }
        });
        if finished {
            exit(world, self.app);
            return Ok(false);
        }
        let result = result.and_then(|presented| {
            if presented {
                self.frames.advance();
                #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
                if let Some(capture) = &self.capture {
                    super::capture::capture_presented(world, self.render, capture, index)?;
                }
                if self.pacing.is_spent(self.frames.advanced()) {
                    exit(world, self.app);
                }
            } else {
                self.service_idle(world)?;
            }
            Ok(presented)
        });
        if let Err(message) = &result {
            self.failure.report(world, self.app, message.clone());
        }
        result
    }

    fn service_idle(&self, world: &World) -> Result<(), String> {
        let failed = drain_all_stepped_worlds(world);
        if !failed.is_empty() {
            return Err(format!("world jobs failed: {}", failed.join(", ")));
        }
        let mut render = world
            .root_scope()
            .try_query_one::<&mut World>(self.render)
            .map_err(|error| format!("the render world is unavailable: {error}"))?;
        match drain_sub_world(render.get()) {
            StepOutcome::Ran => Ok(()),
            StepOutcome::Finished | StepOutcome::Stopped => {
                exit(world, self.app);
                Ok(())
            }
            StepOutcome::Failed => Err("render world jobs failed".to_owned()),
        }
    }
}

pub(super) fn drive_window() -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER),
        |world, entity, frame| {
            if let Ok(mut row) = world
                .root_scope()
                .try_query_one::<&mut WindowFrames>(entity)
            {
                let state = row.get();
                if state.pacing.is_spent(state.frames.advanced()) {
                    exit(world, state.app);
                } else if state.pacing.client {
                    if let Err(message) = state.service_idle(world) {
                        state.failure.report(world, state.app, message);
                    }
                } else {
                    let _ = state.submit(world, frame.delta_time);
                }
            }
        },
    )
}

fn exit(world: &World, app: Entity) {
    if let Ok(mut request) = world.root_scope().try_query_one::<&mut ExitRequest>(app) {
        request.get().0 = true;
    }
}
