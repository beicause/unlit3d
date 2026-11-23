//! The multi-world presentation path.
//!
//! The machinery a windowed scene presents with -- the [`Presenter`]
//! component, the window surface, the offscreen target and the behaviours
//! that pump, resize and present -- lives in
//! [`unlit3d::winit::presenter`]; this module re-exports it and adds the
//! multi-world pacing. The host world owns the window and the presenter;
//! the renderer and the passes live in a render sub-world. Frames are
//! queued as a command rather than drawn inline, so the host's own async
//! tasks are polled between the event callbacks, and the draw behaviour
//! ticks the sub-world before drawing it -- drains its command queue, folds
//! the input it has been fed, runs its tasks, then dispatches the frame --
//! which is what makes the sub-world a server: clients' requests land as
//! jobs here, on the main thread, between one frame's render and the next.

use unlit_ecs::{Entity, EntityAccessError, World};

pub use unlit3d::winit::presenter::{
    BLIT_PASS_ORDER, Presenter, PresenterError, SCENE_PASS_ORDER, resize_presented,
};

use unlit3d::frame::Frame;
use unlit3d::offscreen::{OFFSCREEN_SAMPLES, TargetConfig};
use unlit3d::winit::SurfaceOptions;
use unlit3d::winit::builtin::{Resumed, window};
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use unlit3d::winit::presenter::{PresentationPath, pump_presented_with};

use crate::executor::LocalTasks;
use crate::gpu::Gpu;
use crate::host::tick_sub_world;
use crate::world::StepOutcome;

/// Build the presentation path and spawn its [`Presenter`] in the host
/// world.
///
/// `render`, `renderer` and `scene_pass` name the render sub-world, its
/// renderer and the pass the scene's own sources draw into, all already
/// filled in. The window comes from the host world; when it has not
/// reported a size yet, `fallback_size` stands in.
///
/// `offscreen` chooses the path. With it the scene's frames land in a
/// persistent target that a screenshot reads back, and a pass added after
/// the scene's copies that target over the swap chain; without it the
/// scene's pass draws straight into the swap chain -- one pass and one
/// copy fewer, which is what a plain window wants.
///
/// `surface_options` configures how the swap chain presents.
///
/// # Errors
///
/// Returns an error when the host has no window, the surface cannot be
/// built, or the render sub-world, its renderer or its targets are gone.
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per scene fixture the host already holds"
)]
pub fn setup_presenter(
    world: &mut World,
    render: Entity,
    renderer: Entity,
    scene_pass: Entity,
    gpu: &Gpu,
    fallback_size: (u32, u32),
    offscreen: bool,
    surface_options: SurfaceOptions,
) -> Result<(), PresenterError> {
    let surface = gpu
        .instance
        .create_surface(window(world).ok_or(PresenterError::NoWindow)?)?;
    let path = if offscreen {
        PresentationPath::Intermediate(TargetConfig::default())
    } else {
        PresentationPath::Surface {
            samples: OFFSCREEN_SAMPLES,
            with_depth: true,
        }
    };
    unlit3d::winit::presenter::setup_presenter(
        world,
        Some(render),
        renderer,
        scene_pass,
        surface,
        &gpu.instance,
        &gpu.adapter,
        fallback_size,
        path,
        surface_options,
    )
    .map(|_| ())
}

/// Why a render turn did not draw and present a frame.
///
/// A world that stopped and a render world that stopped are different ends of
/// a run, so they are different variants: the caller decides on the variant,
/// not on a message.
#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    /// The host carries more than one presenter, so there is no one surface to
    /// present to.
    #[error("the host has more than one presenter")]
    MultiplePresenters,
    /// The render sub-world is not reachable from the host.
    #[error("the render world is unavailable: {0}")]
    RenderWorld(#[from] EntityAccessError),
    /// The simulation that the turn advances has stopped.
    #[error("a simulation world stopped ({0:?})")]
    Simulation(StepOutcome),
    /// The render sub-world has stopped, so there is no frame to draw.
    #[error("the render world stopped ({0:?})")]
    Render(StepOutcome),
    /// Jobs the worlds were asked to run failed, by the names of their worlds.
    #[error("world jobs failed: {}", .0.join(", "))]
    Jobs(Vec<&'static str>),
    /// The frame was drawn but could not be presented.
    #[error("failed to present a frame: {0}")]
    Present(#[from] PresenterError),
}

/// Drive a render turn only after acquiring an image. The preparation callback
/// advances a simulation and reports how that went: anything but
/// [`StepOutcome::Ran`] stops the turn. No render-world borrow is held while
/// the callback runs.
pub fn present_turn(
    world: &World,
    render: Entity,
    frame: &mut Frame,
    prepare: impl FnOnce() -> Result<(), StepOutcome>,
) -> Result<bool, TurnError> {
    if world
        .root_scope()
        .query::<&Resumed>()
        .iter()
        .any(|state| !state.0)
    {
        return Ok(false);
    }
    let mut presenters = world.root_scope().query::<&mut Presenter>();
    let mut rows = presenters.iter();
    let Some(presenter) = rows.next() else {
        return Ok(false);
    };
    if rows.next().is_some() {
        return Err(TurnError::MultiplePresenters);
    }
    let mut prepared = Ok(());
    let presented = presenter.present_with(world, || {
        prepared = prepare().map_err(TurnError::Simulation).and_then(|()| {
            let mut nested = world
                .root_scope()
                .try_query_one::<&mut World>(render)
                .map_err(TurnError::RenderWorld)?;
            match tick_sub_world(nested.get(), frame) {
                StepOutcome::Ran => Ok(()),
                outcome => Err(TurnError::Render(outcome)),
            }
        });
        prepared.is_ok()
    });
    prepared?;
    Ok(presented?)
}

/// The behaviours that keep frames coming: one per turn of the event loop,
/// plus one per compositor redraw request.
///
/// The host world's own tasks are polled before each dispatch, so a task one
/// behaviour spawns is drained by the next frame. Redraw requests alone go
/// quiet whenever the window stops being visible to the compositor, and a
/// simulation left waiting on them freezes with it; the pump keeps the scene
/// advancing on its own.
pub fn pump_presented() -> (OnAboutToWait, OnWindowEvent) {
    pump_presented_with(Some(Box::new(LocalTasks::pump)))
}
