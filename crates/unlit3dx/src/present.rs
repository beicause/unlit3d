//! The presentation path: the window surface, the frame loop that drives
//! it, and the offscreen target an image tool can read.
//!
//! By default the scene draws straight into the swap chain: one renderer,
//! one world, one pass, no copy. Turning the offscreen path on with
//! [`setup_presenter`]'s last argument adds a second pass to that same
//! world: the scene's own pass draws every frame into a persistent
//! offscreen color texture, and the pass added after it copies that
//! texture over the swap chain. The texture survives present, so a
//! screenshot can read the last frame back from it, and the swap chain's
//! format or size changes never touch the scene's pipeline — which is why
//! an MCP-driven scene asks for it.
//!
//! [`setup_presenter`] builds all of that once the GPU context exists;
//! [`draw_presented`], [`pump_presented`] and [`resize_presented`]
//! are the behaviours a windowed app spawns to drive it.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use unlit_ecs::bundle;
use unlit_ecs::{Command, Entity, EntityAccessError, World};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResHandle, ResourceGraph, TextureExt, TextureView};
use unlit3d::attachments::FrameAttachments;
use unlit3d::blit::BlitSource;
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame, dispatch_frame};
use unlit3d::pass::{FramePass, FramePassOrder, Pass};

use crate::executor::LocalTasks;
use unlit3d::renderer::Renderer;
use unlit3d::source::{RenderContext, Source};
use unlit3d::winit::WindowSurface;
use unlit3d::winit::builtin::window;
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use winit::event::WindowEvent;
use winit::window::Window;

use crate::gpu::Gpu;
use crate::host::drain_sub_world;

/// The color format the offscreen target uses; the blit converts into the
/// surface's own format.
pub(crate) const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The sample count a windowed frame resolves with, whether it lands in
/// the swap chain or in an offscreen target.
pub(crate) const FRAME_SAMPLES: u32 = 4;

/// The order a scene's own pass records at, ahead of the copying pass.
///
/// A caller building the render world spawns its pass with this order, so
/// the pass [`setup_presenter`] adds lands after it.
pub const SCENE_PASS_ORDER: FramePassOrder = FramePassOrder(0);

/// The order the offscreen path's copying pass records at: after the
/// scene's own, so the target it samples holds this frame's picture.
pub const BLIT_PASS_ORDER: FramePassOrder = FramePassOrder(1);

/// The frame-behaviour order the draw runs at: after the default band, so
/// every behaviour that feeds the frame has run, and before behaviours that
/// read its result state an order of their own.
pub(crate) const DRAW_ORDER: i32 = 10;

/// Everything needed to render a frame and present it.
///
/// The presenter is a component of its own entity, spawned by
/// [`setup_presenter`]: everything it holds arrives after the GPU
/// request resolves, and a component set fixed at spawn has no room for
/// that.
pub struct Presenter {
    /// The render sub-world the frames are dispatched into.
    render: Entity,
    /// The renderer inside the render sub-world.
    renderer: Entity,
    /// The pass the scene's own sources draw into.
    scene_pass: Entity,
    /// The pass that copies the offscreen target over the surface;
    /// `None` when the scene presents itself.
    blit: Option<Blit>,
    /// The GPU context the offscreen target is rebuilt with on resize.
    gpu: Gpu,
    /// The swap chain wrapper, held by the render sub-world.
    surface: WindowSurface,
    /// The presenting pass's attachments to the swap chain: the scene's
    /// own sample count without the offscreen path, one sample and no
    /// depth with it, since the blit covers the target with one triangle.
    attachments: FrameAttachments,
    /// The size used until the window reports its own.
    fallback_size: (u32, u32),
}

/// The pass that copies the offscreen target into the swap chain.
struct Blit {
    /// The pass in the render sub-world, ordered after every other pass.
    pass: Entity,
    /// The source under that pass, sampling the offscreen target.
    source: Entity,
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
    /// The rendering pass binds the swap chain's image for this frame, so
    /// the pass that draws into the surface takes the frame's view just
    /// before the renderer walks the passes in order.
    fn present(&mut self, host: &World) {
        let Ok(nested) = host.root_scope().query_one::<&World>(self.render) else {
            return;
        };
        let presenting = self.presenting();
        let _ = nested
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| {
                let Some(swap_frame) = self.surface.acquire(&nested, &mut renderer) else {
                    return;
                };
                let _ = nested
                    .root_scope()
                    .query_one::<&mut FramePass>(presenting)
                    .map(|mut pass| {
                        pass.set_attachments(&self.attachments, swap_frame.color_view().clone());
                    });
                renderer.render(&nested);
                let queue = nested
                    .root_scope()
                    .query_one::<&wgpu::Queue>(renderer.context().queue)
                    .expect("the render world carries its queue resource");
                swap_frame.present(&queue);
            });
    }

    /// Match the surface, the attachments and the offscreen target to a
    /// new size.
    ///
    /// The surface resizes first and reports the size it ended up with;
    /// the swap chain's attachments follow it, and the offscreen target is
    /// rebuilt for that size — its new view becomes the blit source's
    /// input, which makes the source rebuild its bind group on the next
    /// frame.
    fn resize(&mut self, host: &World, size: (u32, u32)) {
        let Ok(nested) = host.root_scope().query_one::<&World>(self.render) else {
            return;
        };
        let Some(size) = nested
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| {
                self.surface.resize(&nested, &mut renderer, size.0, size.1);
                self.surface.size()
            })
            .ok()
            .filter(|size| size.0 > 0 && size.1 > 0)
        else {
            return;
        };
        self.attachments.resize(&nested, size);
        let Some(blit) = self.blit.as_ref() else {
            return;
        };
        let Ok(context) = nested
            .root_scope()
            .query_one::<&Renderer>(self.renderer)
            .map(|renderer| renderer.context())
        else {
            return;
        };
        let Ok(offscreen) =
            bind_offscreen(&nested, context, self.scene_pass, &self.gpu.device, size)
        else {
            return;
        };
        let _ = nested
            .root_scope()
            .query_one::<&mut Source>(blit.source)
            .map(|mut source| {
                source
                    .as_mut::<BlitSource>()
                    .expect("the blit pass's source is a blit")
                    .view = offscreen.view;
            });
    }
}

/// Where one offscreen target's frames land.
///
/// The texture is the one a screenshot copies out of; the view is the
/// handle a pass draws into and a blit reads, and it stays the same
/// resource node across a resize only until the target is rebuilt.
pub struct OffscreenTarget {
    /// The color texture the frames land in.
    pub texture: wgpu::Texture,
    /// The view of that texture, registered in the world's resource graph.
    pub view: ResHandle<TextureView>,
}

/// Bind an offscreen target of `size` pixels to `pass`, and answer where
/// its frames land.
///
/// The attachments go into the render world's resource graph exactly as a
/// window surface's do, so the renderer specializes its pipelines on them
/// the same way — and the color texture is the one a screenshot copies
/// out of.
pub fn bind_offscreen(
    world: &World,
    context: RenderContext,
    pass: Entity,
    device: &wgpu::Device,
    size: (u32, u32),
) -> Result<OffscreenTarget, EntityAccessError> {
    let color = create_color_target(device, OFFSCREEN_FORMAT, size.0, size.1);
    // The texture is registered as a resource in its own right, and the
    // view depends on it, so whoever holds the view keeps the texture
    // alive — and a screenshot can resolve the texture by handle and read
    // it back.
    let view = {
        let mut graph = world
            .root_scope()
            .query_one::<&mut ResourceGraph>(context.graph)?;
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments =
        FrameAttachments::new(world, context, OFFSCREEN_FORMAT, size, FRAME_SAMPLES, true);
    world
        .root_scope()
        .query_one::<&mut FramePass>(pass)
        .map(|mut pass| {
            pass.set_attachments(&attachments, view.clone());
        })
        .expect("the bound pass exists");
    Ok(OffscreenTarget {
        texture: color,
        view,
    })
}

/// Open the window surface in `world`, through its `renderer`, and build
/// the attachments its frames draw with.
///
/// The surface is configured for the world that presents it, which is the
/// scene's own unless an offscreen target is in use.
fn open_surface(
    world: &World,
    renderer: Entity,
    gpu: &Gpu,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    samples: u32,
    with_depth: bool,
) -> Result<(WindowSurface, FrameAttachments), EntityAccessError> {
    world
        .root_scope()
        .query_one::<&mut Renderer>(renderer)
        .map(|mut renderer| {
            let surface = WindowSurface::new(
                world,
                &mut renderer,
                &gpu.instance,
                &gpu.adapter,
                Arc::clone(&window),
                surface,
            );
            let attachments = FrameAttachments::new(
                world,
                renderer.context(),
                surface.color_format(),
                surface.size(),
                samples,
                with_depth,
            );
            (surface, attachments)
        })
}

/// Build the presentation path for a live render sub-world and spawn its
/// [`Presenter`] component.
///
/// `render`, `renderer` and `scene_pass` name the sub-world, its renderer
/// and the pass the scene's own sources draw into, all already filled in.
/// The window comes from the host world; when it has not reported a size
/// yet, `fallback_size` stands in.
///
/// `offscreen` chooses the path. With it the scene's frames land in a
/// persistent target that a screenshot reads back, and a pass added after
/// the scene's copies that target over the swap chain; without it the
/// scene's pass draws straight into the swap chain — one pass and one copy
/// fewer, which is what a plain window wants.
pub fn setup_presenter(
    world: &mut World,
    render: Entity,
    renderer: Entity,
    scene_pass: Entity,
    gpu: &Gpu,
    fallback_size: (u32, u32),
    offscreen: bool,
) -> Result<(), String> {
    let window = window(world).ok_or("the scene setup found no window")?;
    let surface = gpu
        .instance
        .create_surface(window.clone())
        .map_err(|error| format!("failed to build the presentation surface: {error}"))?;
    let raw_size = window.inner_size();
    let size = if raw_size.width == 0 || raw_size.height == 0 {
        fallback_size
    } else {
        (raw_size.width, raw_size.height)
    };

    let (blit, surface, attachments) = {
        let mut nested = world
            .root_scope()
            .query_one::<&mut World>(render)
            .map_err(|_| "the scene setup found no render world")?;
        let context = nested
            .root_scope()
            .query_one::<&Renderer>(renderer)
            .map_err(|_| "the scene setup found no renderer")
            .map(|renderer| renderer.context())?;
        // The offscreen path keeps the scene away from the swap chain: the
        // scene's pass draws into a target of its own, and a pass added
        // after it copies that target over the surface.
        if offscreen {
            let target = bind_offscreen(&nested, context, scene_pass, &gpu.device, size)
                .map_err(|_| "failed to bind the offscreen target: the resource graph was gone")?;
            let blit_pass = nested.root_scope_mut().spawn(Pass::new(BLIT_PASS_ORDER));
            let blit_source = nested
                .scope_mut(blit_pass)
                .expect("the blit pass is alive")
                .spawn(bundle![Source::new(BlitSource::new(target.view))]);
            // The presenting pass takes each frame's swap chain image in
            // [`Presenter::present`], which is the only place that knows it.
            let (surface, attachments) =
                open_surface(&nested, renderer, gpu, window, surface, 1, false)
                    .map_err(|_| "failed to open the window surface: the renderer was gone")?;
            (
                Some(Blit {
                    pass: blit_pass,
                    source: blit_source,
                }),
                surface,
                attachments,
            )
        } else {
            let (surface, attachments) =
                open_surface(&nested, renderer, gpu, window, surface, FRAME_SAMPLES, true)
                    .map_err(|_| "failed to open the window surface: the renderer was gone")?;
            (None, surface, attachments)
        }
    };
    world.root_scope_mut().spawn(bundle![Presenter {
        render,
        renderer,
        scene_pass,
        blit,
        gpu: gpu.clone(),
        surface,
        attachments,
        fallback_size,
    },]);
    Ok(())
}

/// One frame's dispatch, deferred to the command apply.
///
/// The frame behaviours need a mutable world, and they run between two
/// event callbacks — so the pump and the redraw request queue this
/// command instead of drawing inline.
#[derive(Clone)]
struct DrawFrame {
    /// The instant the previous frame dispatched, shared by both
    /// producers.
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
        let delta_time = self
            .last
            .lock()
            .expect("the frame clock is not poisoned")
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
/// Draining is what makes the sub-world a server: clients' requests land
/// as jobs here, on the main thread, between one frame's render and the
/// next.
///
/// The renderer walks every pass of the world here, so the scene's own pass
/// and the offscreen path's copying pass both record on this one call.
pub fn draw_presented() -> OnFrame {
    OnFrame::with_order(
        FrameBehaviourOrder(DRAW_ORDER),
        move |world, _entity, frame| {
            let Some(mut presenter) = world
                .root_scope()
                .query::<(Entity, &mut Presenter)>()
                .next()
                .map(|(_, p)| p)
            else {
                return;
            };
            let render = presenter.render;
            let Ok(mut nested) = world.root_scope().query_one::<&mut World>(render) else {
                return;
            };
            drain_sub_world(&mut nested);
            // The render world's tasks run between the jobs that queued them
            // and the dispatch that draws what they landed.
            LocalTasks::pump(&mut nested);
            dispatch_frame(&mut nested, frame);
            drop(nested);
            presenter.present(world);
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
        if let Some(mut presenter) = world
            .root_scope()
            .query::<(Entity, &mut Presenter)>()
            .next()
            .map(|(_, p)| p)
        {
            presenter.resize(world, (size.width, size.height));
        }
    })
}
