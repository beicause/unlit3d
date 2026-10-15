//! The presentation path: the window surface, the frame loop that drives
//! it, and the offscreen target an image tool can read.
//!
//! By default the scene draws straight into the swap chain: one renderer,
//! one world, no copy. Turning the offscreen path on with
//! [`setup_presenter`]'s last argument renders every frame into a
//! persistent offscreen color texture instead, and a second, minimal world
//! copies it over the swap chain. The texture survives present, so a
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

use unlit_ecs::{Command, Entity, World};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit3d::attachments::FrameAttachments;
use unlit3d::blit::{BlitSource, BlitTexture};
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame, dispatch_frame};
use unlit3d::renderer::Renderer;
use unlit3d::source::{WorldSourceExt, spawn_context};
use unlit3d::winit::WindowSurface;
use unlit3d::winit::builtin::window;
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use winit::event::WindowEvent;
use winit::window::Window;

use crate::gpu::Gpu;
use crate::host::drain_sub_world;
use crate::world::WorldName;

/// The color format the offscreen target uses; the blit converts into the
/// surface's own format.
pub(crate) const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The sample count a windowed frame resolves with, whether it lands in
/// the swap chain or in an offscreen target.
pub(crate) const FRAME_SAMPLES: u32 = 4;

/// The frame-behaviour order the draw runs at: after any behaviour that
/// feeds the frame, before behaviours that read its result.
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
    /// The GPU context the offscreen target is rebuilt with on resize.
    gpu: Gpu,
    /// The swap chain wrapper, held by whichever world presents.
    surface: WindowSurface,
    /// The presenting world's attachments to the swap chain: the scene's
    /// own sample count without the offscreen path, one sample and no
    /// depth with it, since the blit covers the target with one triangle.
    attachments: FrameAttachments,
    /// The second world that copies the offscreen target over the surface;
    /// `None` when the scene presents itself.
    blit: Option<Blit>,
    /// The size used until the window reports its own.
    fallback_size: (u32, u32),
}

/// The world that copies the offscreen target into the swap chain.
struct Blit {
    /// The host entity carrying the blit world.
    world: Entity,
    /// The renderer inside the blit world.
    renderer: Entity,
    /// The blit world's texture entity, replaced on resize.
    texture: Entity,
}

impl Presenter {
    /// The world a frame is presented through, and its renderer.
    ///
    /// The scene presents itself unless an offscreen target sits in front
    /// of the swap chain.
    fn presented(&self) -> (Entity, Entity) {
        self.blit
            .as_ref()
            .map_or((self.render, self.renderer), |blit| {
                (blit.world, blit.renderer)
            })
    }

    /// Draw the frame's result into the swap chain and present it.
    ///
    /// The world that owns the surface is the one rendered here: the
    /// blit world, whose only source copies the offscreen target over the
    /// image, or the scene's own — which therefore renders straight into
    /// the swap chain.
    fn present(&mut self, host: &World) {
        let (world, renderer) = self.presented();
        let Some(nested) = host.get::<World>(world) else {
            return;
        };
        let _ = nested.with_mut::<Renderer, _>(renderer, |renderer| {
            let Some(swap_frame) = self.surface.acquire(&nested, renderer) else {
                return;
            };
            self.attachments
                .bind(&nested, renderer, swap_frame.color_view().clone());
            renderer.render(&nested);
            let queue = nested
                .get::<wgpu::Queue>(renderer.context().queue)
                .expect("the presented world carries its queue resource");
            swap_frame.present(&queue);
        });
    }

    /// Match the surface, the attachments and the offscreen target to a
    /// new size.
    ///
    /// The surface belongs to the presenting world, so that world resizes
    /// first and reports the size the surface ended up with; its
    /// attachments follow it, and the offscreen target is rebuilt for that
    /// size.
    fn resize(&mut self, host: &World, size: (u32, u32)) {
        let (world, renderer) = self.presented();
        let Some(nested) = host.get::<World>(world) else {
            return;
        };
        let Some(size) = nested
            .with_mut::<Renderer, _>(renderer, |renderer| {
                self.surface.resize(&nested, renderer, size.0, size.1);
                self.surface.size()
            })
            .filter(|size| size.0 > 0 && size.1 > 0)
        else {
            return;
        };
        self.attachments.resize(&nested, size);
        let Some(blit) = self.blit.as_mut() else {
            return;
        };
        let Some(render_nested) = host.get::<World>(self.render) else {
            return;
        };
        let Some(offscreen) = render_nested
            .with_mut::<Renderer, _>(self.renderer, |renderer| {
                bind_offscreen(&render_nested, renderer, &self.gpu.device, size)
            })
            .flatten()
        else {
            return;
        };
        // Point the blit at the new target: the replaced texture component
        // is what makes the blit source rebuild its bind group on the next
        // frame.
        let view = TextureExt::create_view(&offscreen, &wgpu::TextureViewDescriptor::default());
        let context = nested
            .get::<Renderer>(renderer)
            .expect("the presenting renderer is a resource entity")
            .context();
        let texture = BlitTexture::new(&nested, context, view);
        let _ = nested.with_mut::<BlitTexture, _>(blit.texture, |slot| *slot = texture);
    }
}

/// Bind an offscreen target of `size` pixels as the render world's render
/// target, and return the color texture the frames land in.
///
/// The attachments go into the render world's resource graph exactly as a
/// window surface's do, so the renderer specializes its pipelines on them
/// the same way — and the color texture is the one a screenshot copies
/// out of.
pub fn bind_offscreen(
    world: &World,
    renderer: &mut Renderer,
    device: &wgpu::Device,
    size: (u32, u32),
) -> Option<wgpu::Texture> {
    let color = create_color_target(device, OFFSCREEN_FORMAT, size.0, size.1);
    // The texture is registered as a resource in its own right, and the
    // view depends on it, so the renderer holding the view keeps the
    // texture alive — and a screenshot can resolve the texture by handle
    // and read it back.
    let color_view = {
        let mut graph = world.get_mut::<ResourceGraph>(renderer.context().graph)?;
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments = FrameAttachments::new(
        world,
        renderer.context(),
        OFFSCREEN_FORMAT,
        size,
        FRAME_SAMPLES,
        true,
    );
    attachments.bind(world, renderer, color_view);
    Some(color)
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
) -> Option<(WindowSurface, FrameAttachments)> {
    world
        .with_mut::<Renderer, _>(renderer, |renderer| {
            let surface = WindowSurface::new(
                world,
                renderer,
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
            Some((surface, attachments))
        })
        .flatten()
}

/// Build the presentation path for a live render sub-world and spawn its
/// [`Presenter`] component.
///
/// `render` and `renderer` name the sub-world and its renderer, both
/// already filled in. The window comes from the host world; when it has
/// not reported a size yet, `fallback_size` stands in.
///
/// `offscreen` chooses the path. With it the scene's frames land in a
/// persistent target that a screenshot reads back, copied over the swap
/// chain by a second world holding nothing else; without it the scene
/// draws straight into the swap chain — one world, one render pass and one
/// copy fewer, which is what a plain window wants.
pub fn setup_presenter(
    world: &mut World,
    render: Entity,
    renderer: Entity,
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

    // The offscreen path keeps the scene away from the swap chain, so its
    // renderer rebinds to a target of its own and the surface moves to the
    // blit world.
    if offscreen {
        let texture = {
            let nested = world
                .get::<World>(render)
                .ok_or("the scene setup found no render world")?;
            nested
                .with_mut::<Renderer, _>(renderer, |renderer| {
                    bind_offscreen(&nested, renderer, &gpu.device, size)
                })
                .flatten()
                .ok_or("failed to bind the offscreen target: the resource graph was gone")?
        };
        let mut blit_world = World::new();
        let blit_frame = spawn_context(
            &mut blit_world,
            gpu.device.clone(),
            gpu.queue.clone(),
            ResourceGraph::new(),
            gpu.capabilities,
        );
        let blit_renderer = blit_world.spawn((Renderer::new(blit_frame),));
        let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
        let blit_texture = blit_world.spawn((BlitTexture::new(&blit_world, blit_frame, view),));
        blit_world.spawn_source(BlitSource::new());
        let blit_entity = world.spawn((blit_world, WorldName("blit")));
        let (surface, attachments) = {
            let nested = world
                .get::<World>(blit_entity)
                .ok_or("the blit world did not land in the host")?;
            open_surface(&nested, blit_renderer, gpu, window, surface, 1, false)
                .ok_or("failed to open the window surface: the blit renderer was gone")?
        };
        world.spawn((Presenter {
            render,
            renderer,
            gpu: gpu.clone(),
            surface,
            attachments,
            blit: Some(Blit {
                world: blit_entity,
                renderer: blit_renderer,
                texture: blit_texture,
            }),
            fallback_size,
        },));
        return Ok(());
    }

    let (surface, attachments) = {
        let nested = world
            .get::<World>(render)
            .ok_or("the scene setup found no render world")?;
        open_surface(&nested, renderer, gpu, window, surface, FRAME_SAMPLES, true)
            .ok_or("failed to open the window surface: the renderer was gone")?
    };
    world.spawn((Presenter {
        render,
        renderer,
        gpu: gpu.clone(),
        surface,
        attachments,
        blit: None,
        fallback_size,
    },));
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
    fn apply(self: Box<Self>, world: &mut World) {
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
                    .query::<&Presenter>()
                    .next()
                    .map(|(_, presenter)| presenter.fallback_size)
            })
            .unwrap_or((1, 1));
        let mut frame = Frame {
            delta_time,
            index: self.index.fetch_add(1, Ordering::Relaxed),
            size,
        };
        dispatch_frame(world, &mut frame);
        // A frame has been dispatched, so the next producer may queue one.
        self.pending.store(false, Ordering::Release);
    }
}

/// The draw behaviour: drain the render sub-world's command queue, then
/// dispatch the frame into it and present the result.
///
/// Draining is what makes the sub-world a server: clients' requests land
/// as jobs here, on the main thread, between one frame's render and the
/// next.
///
/// The scene is only rendered here when an offscreen target is in use: the
/// presenter renders the world that owns the swap chain, and that is the
/// scene's own world otherwise.
pub fn draw_presented() -> OnFrame {
    OnFrame::new(
        FrameBehaviourOrder(DRAW_ORDER),
        move |world, _entity, frame| {
            let Some(mut presenter) = world.query::<&mut Presenter>().next().map(|(_, p)| p) else {
                return;
            };
            let (render, renderer) = (presenter.render, presenter.renderer);
            let Some(mut nested) = world.get_mut::<World>(render) else {
                return;
            };
            drain_sub_world(&mut nested);
            dispatch_frame(&mut nested, frame);
            if presenter.blit.is_some() {
                let _ = nested.with_mut::<Renderer, _>(renderer, |renderer| {
                    renderer.render(&nested);
                });
            }
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
    world.queue().push(DrawFrame {
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
        if let Some(mut presenter) = world.query::<&mut Presenter>().next().map(|(_, p)| p) {
            presenter.resize(world, (size.width, size.height));
        }
    })
}
