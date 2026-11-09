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

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use unlit_ecs::World;
use unlit_ecs::sync;
use unlit_ecs::{Command, Entity, EntityAccessError};

pub use unlit3d::winit::presenter::{
    BLIT_PASS_ORDER, Presenter, PresenterError, SCENE_PASS_ORDER, resize_presented,
};

use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame, dispatch_frame};
use unlit3d::offscreen::{OFFSCREEN_SAMPLES, TargetConfig};
use unlit3d::winit::builtin::window;
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use unlit3d::winit::presenter::{PRESENT_ORDER, PresentationPath};
use winit::event::WindowEvent;

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
/// # Errors
///
/// Returns an error when the host has no window, the surface cannot be
/// built, or the render sub-world, its renderer or its targets are gone.
pub fn setup_presenter(
    world: &mut World,
    render: Entity,
    renderer: Entity,
    scene_pass: Entity,
    gpu: &Gpu,
    fallback_size: (u32, u32),
    offscreen: bool,
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
    )
    .map(|_| ())
}

/// One frame's dispatch, deferred to the command apply.
///
/// The frame behaviours need a mutable world, and they run between two
/// event callbacks -- so the pump and the redraw request queue this
/// command instead of drawing inline.
#[derive(Clone)]
struct DrawFrame {
    /// The instant the previous frame dispatched, shared by both producers.
    last: Arc<Mutex<Option<web_time::Instant>>>,
    /// The index the next dispatched frame takes.
    index: Arc<AtomicU32>,
    /// Set while a draw is queued but not yet applied, so the pump and a
    /// redraw request in the same turn coalesce into one frame.
    pending: Arc<AtomicBool>,
}

impl Command for DrawFrame {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let now = web_time::Instant::now();
        let delta_time = sync::lock(&self.last)
            .replace(now)
            .map_or(0.0, |last| (now - last).as_secs_f32().min(0.1));
        let size = window(world)
            .map(|window| {
                let size = window.inner_size();
                (size.width, size.height)
            })
            .or_else(|| {
                world
                    .root_scope()
                    .query::<(Entity, &Presenter)>()
                    .iter()
                    .next()
                    .map(|(_, presenter)| presenter.fallback_size)
            })
            .unwrap_or((1, 1));
        let mut frame = Frame {
            delta_time,
            index: self.index.fetch_add(1, Ordering::Relaxed),
            size,
        };
        // The host world's own tasks run before the behaviours that read
        // what they land, so a task spawned by one of them is polled by the
        // next frame.
        LocalTasks::pump(world);
        dispatch_frame(world, &mut frame);
        // A frame has been dispatched, so the next producer may queue one.
        self.pending.store(false, Ordering::Release);
        Ok(())
    }
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
pub fn draw_presented(render: Entity) -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER),
        move |world, _entity, frame| {
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

/// The behaviours that keep frames coming: one per turn of the event
/// loop, plus one per compositor redraw request.
///
/// Redraw requests alone go quiet whenever the window stops being
/// visible to the compositor, and a simulation left waiting on them
/// freezes with it; the pump keeps the scene advancing on its own.
pub fn pump_presented() -> (OnAboutToWait, OnWindowEvent) {
    let last: Arc<Mutex<Option<web_time::Instant>>> = Arc::default();
    let index: Arc<AtomicU32> = Arc::default();
    let pending: Arc<AtomicBool> = Arc::default();
    let pump = {
        let (last, index, pending) = (Arc::clone(&last), Arc::clone(&index), Arc::clone(&pending));
        OnAboutToWait::new(move |world, _entity, ()| {
            queue_draw(world, &last, &index, &pending);
        })
    };
    let redraw = {
        let (last, index, pending) = (Arc::clone(&last), Arc::clone(&index), Arc::clone(&pending));
        OnWindowEvent::new(move |world, _entity, payload| {
            if matches!(payload.event, WindowEvent::RedrawRequested) {
                queue_draw(world, &last, &index, &pending);
            }
        })
    };
    (pump, redraw)
}

/// Queue one frame unless a queued one has not been applied yet.
///
/// The pump and a redraw request can both fire in the same turn of the
/// event loop; without the flag that turn would dispatch two frames and
/// advance the simulation twice.
fn queue_draw(
    world: &World,
    last: &Arc<Mutex<Option<web_time::Instant>>>,
    index: &Arc<AtomicU32>,
    pending: &Arc<AtomicBool>,
) {
    if pending.swap(true, Ordering::AcqRel) {
        return;
    }
    world.root_scope().queue().push(DrawFrame {
        last: Arc::clone(last),
        index: Arc::clone(index),
        pending: Arc::clone(pending),
    });
}
