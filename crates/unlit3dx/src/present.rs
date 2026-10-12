//! The presentation path: an offscreen render target, a blit into the
//! window surface, and the frame loop that drives them.
//!
//! A scene renders into a persistent offscreen color texture instead of
//! straight into the swap chain. The texture survives present, so tools
//! can read the last frame back from it, and the swap chain's format or
//! size changes never touch the scene's pipeline. A second, minimal
//! world owns the surface and copies the target over it each frame.
//!
//! [`setup_presenter`] builds all of that once the GPU context exists;
//! [`draw_presented`], [`pump_presented`] and [`resize_presented`]
//! are the behaviours a windowed app spawns to drive it.

use std::sync::{Arc, Mutex};

use unlit_ecs::{Command, Entity, World};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit3d::attachments::FrameAttachments;
use unlit3d::blit::{BlitSource, BlitTexture};
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame, dispatch_frame};
use unlit3d::renderer::Renderer;
use unlit3d::source::WorldSourceExt;
use unlit3d::source::spawn_context;
use unlit3d::winit::WindowSurface;
use unlit3d::winit::builtin::window;
use unlit3d::winit::event::{OnAboutToWait, OnWindowEvent};
use winit::event::WindowEvent;

use crate::gpu::Gpu;
use crate::host::drain_sub_world;

/// The color format the offscreen target uses; the blit converts into the
/// surface's own format.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The sample count the offscreen target resolves with.
pub const OFFSCREEN_SAMPLES: u32 = 4;

/// The frame-behaviour order the draw runs at: after any behaviour that
/// feeds the frame, before behaviours that read its result.
pub const DRAW_ORDER: i32 = 10;

/// Everything needed to render a frame offscreen and present it.
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
    /// The swap chain wrapper the blit world presents through.
    surface: WindowSurface,
    /// The blit world's frame attachments: one sample, no depth.
    attachments: FrameAttachments,
    /// The persistent color texture frames land in.
    offscreen: wgpu::Texture,
    /// The host entity carrying the blit world.
    blit_world: Entity,
    /// The renderer inside the blit world.
    blit_renderer: Entity,
    /// The blit world's texture entity, replaced on resize.
    blit_texture: Entity,
    /// The size used until the window reports its own.
    fallback_size: (u32, u32),
}

impl Presenter {
    /// Copy the offscreen target into the swap chain and present it.
    fn present(&mut self, host: &World) {
        let Some(blit_nested) = host.get::<World>(self.blit_world) else {
            return;
        };
        let _ = blit_nested.with_mut::<Renderer, _>(self.blit_renderer, |renderer| {
            let Some(swap_frame) = self.surface.acquire(&blit_nested, renderer) else {
                return;
            };
            self.attachments
                .bind(&blit_nested, renderer, swap_frame.color_view().clone());
            renderer.render(&blit_nested);
            let queue = blit_nested
                .get::<wgpu::Queue>(renderer.context().queue)
                .expect("the blit world carries its queue resource");
            swap_frame.present(&queue);
        });
    }

    /// Match the surface, the blit attachments and the offscreen target
    /// to a new size.
    fn resize(&mut self, host: &World, size: (u32, u32)) {
        let Some(blit_nested) = host.get::<World>(self.blit_world) else {
            return;
        };
        let _ = blit_nested.with_mut::<Renderer, _>(self.blit_renderer, |renderer| {
            self.surface.resize(&blit_nested, renderer, size.0, size.1);
        });
        self.attachments.resize(&blit_nested, self.surface.size());
        let Some(nested) = host.get::<World>(self.render) else {
            return;
        };
        let offscreen = nested
            .with_mut::<Renderer, _>(self.renderer, |renderer| {
                bind_offscreen(&nested, renderer, &self.gpu.device, self.surface.size())
            })
            .flatten();
        let Some(offscreen) = offscreen else {
            return;
        };
        // Point the blit at the new target: the replaced texture component
        // is what makes the blit source rebuild its bind group on the next
        // frame.
        let view = TextureExt::create_view(&offscreen, &wgpu::TextureViewDescriptor::default());
        let context = blit_nested
            .get::<Renderer>(self.blit_renderer)
            .expect("the blit renderer is a resource entity")
            .context();
        let texture = BlitTexture::new(&blit_nested, context, view);
        let replaced =
            blit_nested.with_mut::<BlitTexture, _>(self.blit_texture, |slot| *slot = texture);
        if replaced.is_none() {
            return;
        }
        self.offscreen = offscreen;
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
        OFFSCREEN_SAMPLES,
        true,
    );
    attachments.bind(world, renderer, color_view);
    Some(color)
}

/// Build the presentation path for a live render sub-world and spawn its
/// [`Presenter`] component.
///
/// `render` and `renderer` name the sub-world and its renderer, both
/// already filled in. The window comes from the host world; when it has
/// not reported a size yet, `fallback_size` stands in.
pub fn setup_presenter(
    world: &mut World,
    render: Entity,
    renderer: Entity,
    gpu: &Gpu,
    fallback_size: (u32, u32),
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
    let offscreen = {
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

    // The blit world owns the swap chain and copies the offscreen target
    // over it every frame. It draws with one sample and no depth: the
    // blit covers the target with a single triangle.
    let mut blit_world = World::new();
    let blit_frame = spawn_context(
        &mut blit_world,
        gpu.device.clone(),
        gpu.queue.clone(),
        ResourceGraph::new(),
        gpu.capabilities,
    );
    let blit_renderer = blit_world.spawn((Renderer::new(blit_frame),));
    let created = blit_world.with_mut::<Renderer, _>(blit_renderer, |blit_renderer| {
        let window_surface = WindowSurface::new(
            &blit_world,
            blit_renderer,
            &gpu.instance,
            &gpu.adapter,
            window.clone(),
            surface,
        );
        let attachments = FrameAttachments::new(
            &blit_world,
            blit_renderer.context(),
            window_surface.color_format(),
            window_surface.size(),
            1,
            false,
        );
        (window_surface, attachments)
    });
    let (surface, attachments) =
        created.ok_or("failed to open the window surface: the renderer was gone")?;
    let blit_texture = {
        let context = blit_world
            .get::<Renderer>(blit_renderer)
            .expect("the blit renderer is a resource entity")
            .context();
        let view = TextureExt::create_view(&offscreen, &wgpu::TextureViewDescriptor::default());
        blit_world.spawn((BlitTexture::new(&blit_world, context, view),))
    };
    blit_world.spawn_source(BlitSource::new());
    let blit_entity = world.spawn((blit_world,));

    world.spawn((Presenter {
        render,
        renderer,
        gpu: gpu.clone(),
        surface,
        attachments,
        offscreen,
        blit_world: blit_entity,
        blit_renderer,
        blit_texture,
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
            index: 0,
            size,
        };
        dispatch_frame(world, &mut frame);
    }
}

/// The draw behaviour: drain the render sub-world's command queue, then
/// dispatch the frame into it, render into the offscreen target, and
/// present the target through the window surface.
///
/// Draining is what makes the sub-world a server: clients' requests land
/// as jobs here, on the main thread, between one frame's render and the
/// next.
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
            let _ = nested.with_mut::<Renderer, _>(renderer, |renderer| {
                renderer.render(&nested);
            });
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
    let pump = {
        let last = Arc::clone(&last);
        OnAboutToWait::new(move |world, _entity, ()| {
            world.queue().push(DrawFrame {
                last: Arc::clone(&last),
            });
        })
    };
    let redraw = {
        let last = Arc::clone(&last);
        OnWindowEvent::new(move |world, _entity, payload| {
            if matches!(payload.event, WindowEvent::RedrawRequested) {
                world.queue().push(DrawFrame {
                    last: Arc::clone(&last),
                });
            }
        })
    };
    (pump, redraw)
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
