//! The presentation path: the window surface, and how a frame reaches it.
//!
//! A scene presents one of two ways, chosen with [`PresentationPath`]. The plain
//! one draws straight into the swap chain: one pass, no copy. The offscreen
//! one draws the scene into a persistent target
//! ([`offscreen::PersistentTarget`](crate::offscreen::PersistentTarget))
//! and adds a blit pass after it that copies the target over the swap
//! chain. The target survives present, so a screenshot reads the last
//! frame back from it, and the swap chain's format or size changes never
//! touch the scene's pipeline -- which is why an MCP-driven scene asks for
//! it.
//!
//! [`build_presenter`] constructs the [`Presenter`] component once the GPU
//! context exists, and [`setup_presenter`] spawns it. A single-world
//! application drives the result with [`present_frames`],
//! [`pump_presented`] and [`resize_presented`]; a host that keeps the
//! render world in a sub-world -- multi-world runtimes -- spawns its own
//! pump and calls [`Presenter::present`] itself.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit_ecs::sync;
use unlit_ecs::{Command, Entity, EntityAccessError};

use crate::attachments::FrameAttachments;
use crate::blit::BlitSource;
use crate::frame::{Frame, FrameBehaviourOrder, FrameSkip, OnFrame, dispatch_frame};
use crate::offscreen::{PersistentTarget, TargetConfig};
use crate::pass::{FramePass, FramePassOrder, Pass};
use crate::renderer::Renderer;
use crate::source::Source;
use crate::winit::builtin::{Resumed, window};
use crate::winit::event::{OnAboutToWait, OnWindowEvent};
use crate::winit::surface::WindowSurface;
use winit::event::WindowEvent;
use winit::window::Window;

/// Why the presentation path could not be built, or a frame could not be
/// presented.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PresenterError {
    /// The host world carries no window to present to.
    #[error("the scene setup found no window")]
    NoWindow,
    /// The window surface could not be built from the display handle.
    #[error("failed to build the presentation surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    /// The render sub-world is not reachable from the host.
    #[error("the scene setup found no render world: {0}")]
    NoRenderWorld(EntityAccessError),
    /// The render world carries no renderer at the named entity.
    #[error("the scene setup found no renderer: {0}")]
    NoRenderer(EntityAccessError),
    /// The offscreen target could not be bound to the scene pass.
    #[error("failed to bind the offscreen target: {0}")]
    Target(EntityAccessError),
    /// A resource the render world was expected to carry is gone at runtime.
    #[error("the presentation path lost a render-world resource: {0}")]
    MissingResource(#[from] EntityAccessError),
}

/// The order a scene's own pass records at, ahead of the copying pass.
///
/// A caller building the render world spawns its pass with this order, so
/// the pass the offscreen path adds lands after it.
pub const SCENE_PASS_ORDER: FramePassOrder = FramePassOrder(0);

/// The order the offscreen path's copying pass records at: after the
/// scene's own, so the target it samples holds this frame's picture.
pub const BLIT_PASS_ORDER: FramePassOrder = FramePassOrder(1);

/// The frame-behaviour order [`present_frames`] runs at: after the default
/// band, so every behaviour that feeds the frame has run, and before
/// behaviours that read its result state an order of their own.
pub const PRESENT_ORDER: i32 = 10;

/// How a scene reaches the window's swap chain.
///
/// This does not choose the final render destination: both paths present to
/// the window. They differ only in whether the scene first draws into a
/// persistent intermediate texture. Pure offscreen and empty-target rendering
/// use [`PersistentTarget`] or an unattached pass directly, without a presenter.
///
/// [`PresentationPath::Surface`] draws straight into the swap chain.
/// [`PresentationPath::Intermediate`] adds a persistent texture and a blit;
/// that texture is readable after present, which makes it suitable for MCP
/// screenshots, post-processing and scaled presentation.
#[derive(Clone, Copy, Debug)]
pub enum PresentationPath {
    /// Draw the scene straight into the swap chain.
    Surface {
        /// The sample count the swap-chain frames resolve with.
        samples: u32,
        /// Whether swap-chain frames carry a depth-stencil attachment.
        with_depth: bool,
    },
    /// Draw the scene into a persistent intermediate texture, then blit it
    /// over the swap chain. The texture remains readable after presentation,
    /// and its size can scale with the window or stay fixed.
    Intermediate(TargetConfig),
}

impl Default for PresentationPath {
    /// The plain window: a swap-chain frame at four samples with depth, the
    /// presentation path's long-standing default.
    fn default() -> Self {
        Self::Surface {
            samples: crate::offscreen::OFFSCREEN_SAMPLES,
            with_depth: true,
        }
    }
}

/// Everything needed to render a frame and present it.
///
/// The presenter is a component of its own entity, built by
/// [`build_presenter`]: everything it holds arrives after the GPU request
/// resolves, and a component set fixed at spawn has no room for that.
pub struct Presenter {
    /// The render sub-world the frames are presented from; `None` when the
    /// renderer, the passes and the presenter share the host world itself.
    render: Option<Entity>,
    /// The renderer inside the render world.
    renderer: Entity,
    /// The pass the scene's own sources draw into.
    scene_pass: Entity,
    /// The pass that copies the offscreen target over the surface; `None`
    /// when the scene presents itself.
    blit: Option<Blit>,
    /// The swap chain wrapper.
    surface: WindowSurface,
    /// The presenting pass's attachments to the swap chain: the scene's own
    /// sample count without the offscreen path, one sample and no depth
    /// with it, since the blit covers the target with one triangle.
    attachments: FrameAttachments,
    /// The persistent target the scene draws into, when the scene's frames
    /// land offscreen.
    target: Option<PersistentTarget>,
    /// The size used until the window reports its own.
    pub fallback_size: (u32, u32),
}

/// The pass that copies the offscreen target into the swap chain.
struct Blit {
    /// The pass in the render world, ordered after every other pass.
    pass: Entity,
}

impl Presenter {
    /// The pass that draws into the swap chain.
    ///
    /// The offscreen target sits in front of it when the blit pass exists;
    /// otherwise the scene's own pass draws straight into the surface.
    fn presenting(&self) -> Entity {
        self.blit.as_ref().map_or(self.scene_pass, |blit| blit.pass)
    }

    /// Draw the frame's result into the swap chain and present it.
    ///
    /// The presenting pass parks first and takes the frame's view only
    /// after the swap chain image has been acquired. Between the two, and
    /// after the image has been presented and destroyed, the pass states
    /// no attachment -- so a frame rendered by anything other than this
    /// loop, an MCP tool's screenshot of the scene, skips the pass rather
    /// than recording into a dead swap-chain image.
    ///
    /// A surface released by [`Self::release`] is opened again here, so a host
    /// that suspended only has to keep presenting.
    ///
    /// # Errors
    ///
    /// Returns an error when the render world's renderer or its resources
    /// are gone, or the window can no longer back a surface. A render
    /// sub-world that is gone presents nothing and returns `Ok`.
    pub fn present(&mut self, host: &World) -> Result<(), PresenterError> {
        let mut nested = match self.render {
            Some(render) => match host.root_scope().query_one::<&World>(render) {
                Ok(nested) => Some(nested),
                Err(_) => return Ok(()),
            },
            None => None,
        };
        let world = match nested.as_mut() {
            Some(nested) => nested.get(),
            None => host,
        };
        let presenting = self.presenting();
        if self.surface.is_released() {
            let mut renderer_row = world.root_scope().query_one::<&Renderer>(self.renderer)?;
            self.surface
                .build(world, renderer_row.get())
                .map_err(PresenterError::Surface)?;
        }
        let Some(swap_frame) =
            self.surface
                .acquire_for(world, self.renderer, presenting, Some(&self.attachments))
        else {
            return Ok(());
        };
        let mut renderer_row = world
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)?;
        renderer_row.get().render(world);
        let context = renderer_row.get().context();
        let queue = context.queue(&world.root_scope())?;
        swap_frame.present_and_park(world, &queue, presenting);
        Ok(())
    }

    /// Match the surface, the swap-chain attachments and the offscreen
    /// target to a new size.
    ///
    /// The surface resizes first and reports the size it ended up with; the
    /// swap chain's attachments follow it, and the offscreen target is
    /// rebuilt for that size. The target's texture and view are replaced in
    /// place -- their resource nodes keep their ids -- so the blit reading
    /// the view rebuilds its bind group through its own dependency, without
    /// the presenter touching the source.
    ///
    /// # Errors
    ///
    /// Returns an error when the render world or its resources are gone.
    pub fn resize(&mut self, host: &World, size: (u32, u32)) -> Result<(), PresenterError> {
        let mut nested = match self.render {
            Some(render) => Some(
                host.root_scope()
                    .query_one::<&World>(render)
                    .map_err(PresenterError::MissingResource)?,
            ),
            None => None,
        };
        let world = match nested.as_mut() {
            Some(nested) => nested.get(),
            None => host,
        };
        let size = {
            let mut renderer = world
                .root_scope()
                .query_one::<&mut Renderer>(self.renderer)?;
            self.surface
                .resize(world, &mut *renderer.get(), size.0, size.1);
            self.surface.size()
        };
        self.attachments.resize(world, size);
        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };
        let context = {
            let mut renderer = world.root_scope().query_one::<&Renderer>(self.renderer)?;
            renderer.get().context()
        };
        let device = context.device(&world.root_scope())?;
        target
            .resize(world, context, &device, size)
            .map_err(PresenterError::MissingResource)?;
        Ok(())
    }

    /// Drop the swap chain the platform is about to invalidate, keeping
    /// everything the scene drew from.
    ///
    /// The presenting pass parks first: it still names a view of the swap
    /// chain, and that id is what keeps the view alive -- parking it drops
    /// the view, so the view goes with the surface this release collects.
    /// The scene keeps its state and presents again through a surface built
    /// from the same window.
    ///
    /// # Errors
    ///
    /// Returns an error when the render world or its renderer is gone.
    pub fn release(&mut self, host: &World) -> Result<(), PresenterError> {
        let mut nested = match self.render {
            Some(render) => Some(
                host.root_scope()
                    .query_one::<&World>(render)
                    .map_err(PresenterError::MissingResource)?,
            ),
            None => None,
        };
        let world = match nested.as_mut() {
            Some(nested) => nested.get(),
            None => host,
        };
        let presenting = self.presenting();
        let _ = world
            .root_scope()
            .query_one::<&mut FramePass>(presenting)
            .map(|mut pass| pass.get().park());
        let mut renderer_row = world
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)?;
        self.surface.release(world, &mut *renderer_row.get());
        Ok(())
    }
}

/// Build the presentation path for a live render world, as a
/// [`Presenter`] component value.
///
/// `render` names the sub-world the renderer and the passes live in, or
/// `None` when they share `world` itself; `renderer` and `scene_pass` name
/// the renderer and the pass the scene's own sources draw into, all already
/// filled in. The window comes from `world`; when it has not reported a
/// size yet, `fallback_size` stands in.
///
/// `path` chooses direct presentation or an intermediate texture plus blit.
///
/// # Errors
///
/// Returns an error when `world` has no window, the surface cannot be
/// built, or the render world, its renderer or its targets are gone.
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per scene fixture the host already holds"
)]
pub fn build_presenter(
    world: &mut World,
    render: Option<Entity>,
    renderer: Entity,
    scene_pass: Entity,
    surface: wgpu::Surface<'static>,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    fallback_size: (u32, u32),
    path: PresentationPath,
) -> Result<Presenter, PresenterError> {
    let window = window(world).ok_or(PresenterError::NoWindow)?;
    let raw_size = window.inner_size();
    let size = if raw_size.width == 0 || raw_size.height == 0 {
        fallback_size
    } else {
        (raw_size.width, raw_size.height)
    };
    let (blit, surface, attachments, target) = match render {
        Some(render) => {
            let mut nested = world
                .root_scope()
                .query_one::<&mut World>(render)
                .map_err(PresenterError::NoRenderWorld)?;
            build_in(
                nested.get(),
                renderer,
                scene_pass,
                instance,
                adapter,
                window,
                surface,
                size,
                path,
            )?
        }
        None => build_in(
            world, renderer, scene_pass, instance, adapter, window, surface, size, path,
        )?,
    };
    Ok(Presenter {
        render,
        renderer,
        scene_pass,
        blit,
        surface,
        attachments,
        target,
        fallback_size,
    })
}

/// Build the presentation path inside the render world itself.
///
/// Returns the blit pass when the scene draws offscreen, the opened window
/// surface, the presenting pass's swap-chain attachments, and the bound
/// offscreen target when there is one.
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per scene fixture the host already holds"
)]
fn build_in(
    world: &mut World,
    renderer: Entity,
    scene_pass: Entity,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    size: (u32, u32),
    path: PresentationPath,
) -> Result<
    (
        Option<Blit>,
        WindowSurface,
        FrameAttachments,
        Option<PersistentTarget>,
    ),
    PresenterError,
> {
    let context = world
        .root_scope()
        .query_one::<&Renderer>(renderer)
        .map_err(PresenterError::NoRenderer)
        .map(|mut row| row.get().context())?;
    // The presenting pass's swap-chain attachments: without the offscreen
    // path it is the scene's own pass, with the scene's sample count and
    // depth; with it, the blit covers the target with one triangle -- one
    // sample and no depth.
    let (samples, with_depth) = match path {
        PresentationPath::Surface {
            samples,
            with_depth,
        } => (samples, with_depth),
        PresentationPath::Intermediate(_) => (1, false),
    };
    let target = match path {
        PresentationPath::Surface { .. } => None,
        PresentationPath::Intermediate(config) => {
            let device = context.device(&world.root_scope())?;
            Some(
                PersistentTarget::bind(
                    world,
                    context,
                    scene_pass,
                    &device,
                    config.size.resolve(size),
                    config,
                )
                .map_err(PresenterError::Target)?,
            )
        }
    };
    // The blit pass reads the target's view. A resize replaces that view in
    // place, so the source is never patched: the bind group rebuilds
    // through its own dependency on the view.
    let blit = match target.as_ref() {
        None => None,
        Some(target) => {
            let blit_pass = world.root_scope_mut().spawn(Pass::new(BLIT_PASS_ORDER));
            world
                .scope_mut(blit_pass)?
                .spawn(bundle![Source::new(BlitSource::new(target.view.clone()))]);
            Some(Blit { pass: blit_pass })
        }
    };
    let (surface, attachments) = {
        let mut renderer_row = world
            .root_scope()
            .query_one::<&mut Renderer>(renderer)
            .map_err(PresenterError::NoRenderer)?;
        let renderer = renderer_row.get();
        {
            let surface = WindowSurface::new(world, renderer, instance, adapter, window, surface);
            let attachments = FrameAttachments::new(
                world,
                renderer.context(),
                surface.color_format(),
                surface.size(),
                samples,
                with_depth,
            );
            (surface, attachments)
        }
    };
    Ok((blit, surface, attachments, target))
}

/// Build the presentation path and spawn its [`Presenter`], answering the
/// entity it landed on.
///
/// See [`build_presenter`] for the arguments.
///
/// # Errors
///
/// Returns the same errors as [`build_presenter`].
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per scene fixture the host already holds"
)]
pub fn setup_presenter(
    world: &mut World,
    render: Option<Entity>,
    renderer: Entity,
    scene_pass: Entity,
    surface: wgpu::Surface<'static>,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    fallback_size: (u32, u32),
    path: PresentationPath,
) -> Result<Entity, PresenterError> {
    let presenter = build_presenter(
        world,
        render,
        renderer,
        scene_pass,
        surface,
        instance,
        adapter,
        fallback_size,
        path,
    )?;
    let entity = world.root_scope_mut().spawn(bundle![presenter]);
    Ok(entity)
}

/// One frame's dispatch, deferred to the command apply.
///
/// The frame behaviours need a mutable world, and they run between two
/// event callbacks -- so the pump and the redraw request queue this command
/// instead of drawing inline.
#[derive(Clone)]
struct DrawFrame {
    /// The instant the previous frame dispatched, shared by both producers.
    last: Arc<Mutex<Option<web_time::Instant>>>,
    /// The index the next dispatched frame takes.
    index: Arc<AtomicU32>,
    /// Set while a draw is queued but not yet applied, so the pump and a
    /// redraw request in the same turn coalesce into one frame.
    pending: Arc<AtomicBool>,
    /// Work the host does right before the dispatch, when it has any.
    pre_dispatch: Option<SharedHostPump>,
}

/// Work the host does before a frame dispatches, for the world being dispatched.
pub type HostPump = Box<dyn FnMut(&mut World)>;

/// The same work, shared by the two producers that queue a frame.
type SharedHostPump = Rc<RefCell<HostPump>>;

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
        if let Some(pre_dispatch) = &self.pre_dispatch {
            (pre_dispatch.borrow_mut())(world);
        }
        dispatch_frame(world, &mut frame);
        // A frame has been dispatched, so the next producer may queue one.
        self.pending.store(false, Ordering::Release);
        Ok(())
    }
}

/// The frame behaviour that presents every dispatched frame: draw the
/// frame's result into the swap chain.
///
/// A single-world application spawns this once; the frame loop that
/// dispatches frames is [`pump_presented`]. The renderer walks every pass
/// of the world here, so the scene's own pass -- and the offscreen path's
/// copying pass, when there is one -- both record on this one call.
///
/// A frame the host skipped -- because its scene or its presentation path is
/// not up yet -- is not presented either, so the behaviour that decides
/// states an earlier order. Neither is a frame the winit host reports the
/// app suspended for; a world with no winit host counts as resumed. When a
/// world carries more than one presenter, the first one the query yields
/// presents.
pub fn present_frames() -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(PRESENT_ORDER),
        move |world, _entity, _frame| {
            // A suspended winit host has no surface worth presenting into.
            let resumed = world
                .root_scope()
                .query::<&Resumed>()
                .iter()
                .next()
                .is_none_or(|state| state.0);
            if !resumed || FrameSkip::is_skipped(world) {
                return;
            }
            let mut rows = world.root_scope().query::<(Entity, &mut Presenter)>();
            let Some((_, presenter)) = rows.iter().next() else {
                return;
            };
            if let Err(error) = presenter.present(world) {
                log::error!("failed to present a frame: {error}");
            }
        },
    )
}

/// The behaviours that keep frames coming: one per turn of the event loop,
/// plus one per compositor redraw request.
///
/// Redraw requests alone go quiet whenever the window stops being visible
/// to the compositor, and a simulation left waiting on them freezes with
/// it; the pump keeps the scene advancing on its own.
pub fn pump_presented() -> (OnAboutToWait, OnWindowEvent) {
    pump_presented_with(None)
}

/// `pump_presented`, with host work that runs before every frame dispatch.
///
/// A runtime that keeps its scene in a sub-world polls that world's own tasks
/// and drains its command queue first; `pre_dispatch` receives the host world
/// so it can do that on the same turn, before the behaviours that read what
/// the work landed are dispatched.
pub fn pump_presented_with(pre_dispatch: Option<HostPump>) -> (OnAboutToWait, OnWindowEvent) {
    let pre_dispatch: Option<SharedHostPump> = pre_dispatch.map(|pump| Rc::new(RefCell::new(pump)));
    let last: Arc<Mutex<Option<web_time::Instant>>> = Arc::default();
    let index: Arc<AtomicU32> = Arc::default();
    let pending: Arc<AtomicBool> = Arc::default();
    let pump = {
        let (last, index, pending) = (Arc::clone(&last), Arc::clone(&index), Arc::clone(&pending));
        let pre_dispatch = pre_dispatch.clone();
        OnAboutToWait::new(move |world, _entity, ()| {
            queue_draw(world, &last, &index, &pending, &pre_dispatch);
        })
    };
    let redraw = {
        let (last, index, pending) = (Arc::clone(&last), Arc::clone(&index), Arc::clone(&pending));
        let pre_dispatch = pre_dispatch.clone();
        OnWindowEvent::new(move |world, _entity, payload| {
            if matches!(payload.event, WindowEvent::RedrawRequested) {
                queue_draw(world, &last, &index, &pending, &pre_dispatch);
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
    pre_dispatch: &Option<SharedHostPump>,
) {
    if pending.swap(true, Ordering::AcqRel) {
        return;
    }
    world.root_scope().queue().push(DrawFrame {
        last: Arc::clone(last),
        index: Arc::clone(index),
        pending: Arc::clone(pending),
        pre_dispatch: pre_dispatch.clone(),
    });
}

/// The resize behaviour: match the presenter to the window's new size,
/// skipping the degenerate zero size windows report on minimize.
pub fn resize_presented() -> OnWindowEvent {
    OnWindowEvent::new(move |world, _entity, payload| {
        let WindowEvent::Resized(size) = &payload.event else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let mut rows = world.root_scope().query::<(Entity, &mut Presenter)>();
        let Some((_, presenter)) = rows.iter().next() else {
            return;
        };
        if let Err(error) = presenter.resize(world, (size.width, size.height)) {
            log::error!("failed to resize the presentation path: {error}");
        }
    })
}
