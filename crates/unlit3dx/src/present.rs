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
//! drains the sub-world's command queue before dispatching the frame into
//! it -- which is what makes the sub-world a server: clients' requests
//! land as jobs here, on the main thread, between one frame's render and
//! the next.

use unlit_ecs::{Entity, World};

pub use unlit3d::winit::presenter::{
    BLIT_PASS_ORDER, Presenter, PresenterError, SCENE_PASS_ORDER, resize_presented,
};

use unlit3d::frame::{FrameBehaviourOrder, OnFrame, dispatch_frame};
use unlit3d::offscreen::{OFFSCREEN_SAMPLES, TargetConfig};
use unlit3d::winit::SurfaceOptions;
use unlit3d::winit::builtin::{Resumed, window};
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use unlit3d::winit::presenter::{PRESENT_ORDER, PresentationPath, pump_presented_with};

use crate::executor::LocalTasks;
use crate::gpu::Gpu;
use crate::host::drain_sub_world;

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

/// The draw behaviour: drain the render sub-world's command queue, then
/// dispatch the frame into it and present the result.
///
/// `render` names the render sub-world the presenter presents from.
/// Draining is what makes the sub-world a server: clients' requests land
/// as jobs here, on the main thread, between one frame's render and the
/// next.
///
/// The renderer walks every pass of the sub-world here, so the scene's own
/// pass -- and the offscreen path's copying pass, when there is one -- both
/// record on this one call.
///
/// The behaviour holds while the winit host reports the app suspended; a
/// host with no winit loop counts as resumed. When the host carries more
/// than one presenter, the first one the query yields presents.
pub fn draw_presented(render: Entity) -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER),
        move |world, _entity, frame| {
            // A suspended host has no display worth drawing into.
            let resumed = world
                .root_scope()
                .query::<&Resumed>()
                .iter()
                .next()
                .is_none_or(|state| state.0);
            if !resumed {
                return;
            }
            let mut presenter_rows = world.root_scope().query::<(Entity, &mut Presenter)>();

            let Some(presenter) = presenter_rows.iter().next().map(|(_, p)| p) else {
                return;
            };
            let Ok(mut nested) = world.root_scope().query_one::<&mut World>(render) else {
                return;
            };
            drain_sub_world(&mut *nested.get());
            // The render world's tasks run between the jobs that queued them
            // and the dispatch that draws what they landed.
            LocalTasks::pump(&mut *nested.get());
            dispatch_frame(&mut *nested.get(), frame);
            drop(nested);
            if let Err(error) = presenter.present(world) {
                log::error!("failed to present a frame: {error}");
            }
        },
    )
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
