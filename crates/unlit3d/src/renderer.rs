//! The frame driver: assembling one frame out of its sources.
//!
//! A [`Renderer`] draws nothing itself. It owns the frame's render target and
//! drives the frame's [`FrameSource`](crate::source::FrameSource)s: every source builds its own
//! [`Scene`](wgpu_unlit_render::scene::Scene) from the world, then the driver records those scenes in
//! [`FrameOrder`](crate::source::FrameOrder) into one pass, opened over the target's attachments. That is
//! the whole of it — the built-in mesh rendering is one source
//! ([`MeshSource`](crate::mesh_source::MeshSource)) and has no more privilege
//! than a caller's own.
//!
//! The GPU state the frame is drawn with — the device, the queue and the
//! resource graph — lives in the world as resource components, addressed by a
//! [`RenderContext`]. Spawn them once with
//! [`spawn_context`](crate::source::spawn_context); the renderer keeps the
//! context's entity ids so it can reach the device and queue every frame.

use unlit_ecs::LocalWorld;
use wgpu_unlit_render::render_attachments::RenderAttachments;
use wgpu_unlit_render::resources::{ResourceGraph, ResourceId};
use wgpu_unlit_render::specialize::SurfaceKey;

use crate::components::RenderLoadOps;
use crate::source::{
    FrameTarget, OrderWarnings, RenderContext, Source, SourceOrder, set_frame_target,
    unset_frame_target,
};

/// The top-level frame driver.
///
/// Spawn it once — as a component on a resource entity — and call
/// [`Renderer::render`] every frame. It owns the render target binding and the
/// order bookkeeping; everything else a frame draws comes from its
/// [`FrameSource`](crate::source::FrameSource) components, which are mounted
/// with [`spawn_source`](crate::source::spawn_source) and are ordinary ECS
/// entities.
///
/// ```no_run
/// # use unlit3d::prelude::*;
/// # use wgpu_unlit_render::resources::ResourceGraph;
/// # let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
/// let mut world = LocalWorld::new();
/// // The frame's GPU state, the built-in mesh source and the driver itself.
/// let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
/// let source = MeshSource::new(&world, ctx);
/// let mesh = spawn_source(&mut world, source);
/// let renderer = world.spawn((Resource, Renderer::new(ctx)));
/// ```
pub struct Renderer {
    /// The world addresses of the frame's GPU state.
    ///
    /// Kept so [`Renderer::render`] can reach the queue to submit through;
    /// everything else a frame needs the world hands out itself.
    context: RenderContext,

    /// The color attachment the frame renders into, as a texture-view resource
    /// in the context's graph, or `None` until [`Self::set_render_target`] binds
    /// one.
    color_view: Option<ResourceId>,
    /// The depth-stencil attachment the frame renders into, as a texture-view
    /// resource in the context's graph, or `None` until
    /// [`Self::set_render_target`] binds one.
    depth_view: Option<ResourceId>,
    /// The multisample attachment, if any, as a texture-view resource in the
    /// context's graph; `None` for a non-multisampled pass or before
    /// [`Self::set_render_target`] binds one.
    msaa_view: Option<ResourceId>,
    /// The [`SurfaceKey`] of the currently bound attachments, cached so the
    /// surface does not have to be re-derived every draw. `None` until
    /// [`Self::set_render_target`] is called.
    surface: Option<SurfaceKey>,
    /// The physical size of the currently bound attachments, cached alongside
    /// [`Self::surface`] so the per-frame [`FrameTarget`] costs no graph access.
    /// `None` until [`Self::set_render_target`] is called.
    bound_size: Option<(u32, u32)>,

    /// Reports sources that declared the same
    /// [`FrameOrder`](crate::source::FrameOrder), without repeating itself
    /// every frame.
    warnings: OrderWarnings,
    /// The frame's sources in record order, reused every frame so resolving the
    /// order allocates nothing.
    order: SourceOrder,
}

impl Renderer {
    /// Build a renderer driving the frame context `ctx`.
    ///
    /// This creates no GPU resources: `ctx` names the device, the queue and the
    /// resource graph the frame is drawn with, all of which
    /// [`spawn_context`](crate::source::spawn_context) already put in the
    /// world. The renderer starts with no render target: bind one with
    /// [`Self::set_render_target`] before the first [`Self::render`].
    pub fn new(ctx: RenderContext) -> Self {
        Self {
            context: ctx,
            color_view: None,
            depth_view: None,
            msaa_view: None,
            surface: None,
            bound_size: None,
            warnings: OrderWarnings::default(),
            order: SourceOrder::default(),
        }
    }

    /// The world addresses of the frame's GPU state.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// Bind the render target the next frames draw into.
    ///
    /// `color_view`, `depth_view` and `msaa_view` are texture-view resources
    /// already registered in the context's resource graph. Any may be `None`: a
    /// depth-only pass omits the color view, a color-only pass omits the depth
    /// view, and a non-multisampled pass omits the MSAA view. At least one of
    /// the color and depth views must be present — a pass with neither cannot
    /// exist — and the MSAA view is only valid alongside a color view it
    /// resolves into.
    ///
    /// The renderer reads the views from the graph each frame, so replacing a
    /// view's texture (and re-binding it here, or relying on the graph's dirty
    /// propagation) is how a swapchain resize reaches the renderer.
    ///
    /// # Panics
    ///
    /// If a given id is not a texture view in the graph, if neither the color
    /// nor the depth view is present, or if an MSAA view is given without a
    /// color view.
    pub fn set_render_target(
        &mut self,
        world: &LocalWorld,
        color_view: Option<ResourceId>,
        depth_view: Option<ResourceId>,
        msaa_view: Option<ResourceId>,
    ) {
        // Resolve every view up front so a bad id panics before any field is
        // touched. The handles are cloned out only to derive the surface key;
        // the ids are what the frame path keeps.
        let graph = world
            .get_mut::<ResourceGraph>(self.context.graph)
            .expect("the context's resource graph exists");
        let color = color_view.map(|id| {
            graph
                .get_texture_view(id)
                .expect("color_view is a texture view in the graph")
                .clone()
        });
        let depth = depth_view.map(|id| {
            graph
                .get_texture_view(id)
                .expect("depth_view is a texture view in the graph")
                .clone()
        });
        let msaa = msaa_view.map(|id| {
            graph
                .get_texture_view(id)
                .expect("msaa_view is a texture view in the graph")
                .clone()
        });
        // The graph records the format each view was created with, which wgpu
        // itself cannot report. A view that reinterprets its texture — an sRGB
        // view over a non-sRGB swap-chain image — is what a pipeline has to
        // match, so it wins over the texture's own format.
        let attachments = RenderAttachments::from_views(color, depth, msaa);
        let attachments = match color_view.and_then(|id| graph.get_texture_view_format(id)) {
            Some(format) => attachments.with_color_format(format),
            None => attachments,
        };
        self.surface = Some(attachments.surface_key());
        self.bound_size = Some((attachments.width(), attachments.height()));
        self.color_view = color_view;
        self.depth_view = depth_view;
        self.msaa_view = msaa_view;
        drop(graph);

        // Sources read the frame's target from the world, so binding one here
        // is also what states it for the frame; see [`FrameTarget`].
        set_frame_target(world, self.frame_target());
    }

    /// Unbind the render target, leaving the renderer with none.
    ///
    /// The inverse of [`Self::set_render_target`]: the renderer then has no
    /// target and [`Self::render`] panics like any renderer that was never
    /// bound one. A frame loop calls this when the target's attachments are
    /// released — a swap chain dropped on suspension — and sets a new target
    /// before rendering again.
    ///
    /// The attachments themselves are not touched: this only forgets them, so
    /// the caller removes them from the graph. It is named for the setter it
    /// inverts rather than for *releasing* anything, which the caller does.
    pub fn unset_render_target(&mut self, world: &LocalWorld) {
        self.color_view = None;
        self.depth_view = None;
        self.msaa_view = None;
        self.surface = None;
        self.bound_size = None;

        // Sources read the frame's target from the world, so unbinding here is
        // also what unsets it for the frame; see [`FrameTarget`].
        unset_frame_target(world);
    }

    /// The frame's target, as the bound attachments describe it.
    ///
    /// # Panics
    ///
    /// If no render target is bound.
    pub fn frame_target(&self) -> FrameTarget {
        let surface = self
            .surface
            .expect("set_render_target binds the target before rendering");
        let (width, height) = self
            .bound_size
            .expect("set_render_target binds the target before rendering");
        FrameTarget {
            surface,
            width,
            height,
        }
    }

    /// Render one frame from the ECS `world`.
    ///
    /// The frame is assembled in two phases: every [`Source`] component builds
    /// its scene from the world, then the scenes are recorded in
    /// [`FrameOrder`](crate::source::FrameOrder) into one pass, opened over the target bound with
    /// [`Self::set_render_target`]. The frame's [`RenderLoadOps`] — the first
    /// one any entity carries, or the defaults — decide what the pass loads and
    /// clears. The frame always records, so a world with no camera, no visible
    /// mesh or no source at all still applies its clears.
    ///
    /// There is no implicit target: a renderer that has not been bound one
    /// panics here.
    ///
    /// # Panics
    ///
    /// If no render target is bound, or if the context's device, queue or graph
    /// resource is gone.
    pub fn render(&mut self, world: &LocalWorld) {
        let load_ops = frame_load_ops(world);

        let device = world
            .get::<wgpu::Device>(self.context.device)
            .expect("the context's device resource exists")
            .clone();
        let queue = world
            .get::<wgpu::Queue>(self.context.queue)
            .expect("the context's queue resource exists")
            .clone();

        // One encoder carries the frame's staged uploads and its pass, so
        // whatever a source stages reaches the GPU in this frame's own
        // submission.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("unlit3d::encoder"),
        });

        // Build phase: each source assembles its own scene, taking its own
        // short-lived borrow of the world for the GPU state it needs. Nothing
        // of the graph or the encoder outlives this loop's body.
        //
        // The sources are visited by row order, which is *not* draw order: the
        // order they record in is resolved below, from the `order` each one
        // declares.
        //
        // Each source is fetched and dropped before the next one, so no list of
        // entities is built: `build_scene` only needs a shared borrow of the
        // world, which is what `for_each` gives alongside the `&mut Source`.
        world.for_each::<&mut Source, _>(|mut source| {
            source.build_scene(world, self.context, &mut encoder);
        });

        // Record phase: the scenes, in declared order. The order is resolved
        // into a buffer the renderer keeps, so a steady scene reorders nothing.
        self.order.resolve(world);
        self.warnings.check(self.order.ambiguous());

        let attachments = self.attachments(world);
        {
            let mut pass = attachments.begin_pass(
                &mut encoder,
                load_ops.color,
                load_ops.depth,
                load_ops.stencil,
            );
            for &entity in self.order.entities() {
                world
                    .with_mut::<Source, _>(entity, |source| source.scene().record(&mut pass))
                    .expect("the source entity exists");
            }
        }

        queue.submit([encoder.finish()]);
    }

    /// Build the one-frame attachment set from the views bound with
    /// [`Self::set_render_target`].
    ///
    /// The views are cloned out of the graph, so the returned set borrows
    /// nothing from the renderer. No texture is allocated here: the caller owns
    /// every attachment through the graph.
    fn attachments(&self, world: &LocalWorld) -> RenderAttachments {
        let graph = world
            .get_mut::<ResourceGraph>(self.context.graph)
            .expect("the context's resource graph exists");
        let color = self.color_view.map(|id| {
            graph
                .get_texture_view(id)
                .expect("bound color view")
                .clone()
        });
        let depth = self.depth_view.map(|id| {
            graph
                .get_texture_view(id)
                .expect("bound depth view")
                .clone()
        });
        let msaa = self
            .msaa_view
            .map(|id| graph.get_texture_view(id).expect("bound msaa view").clone());
        RenderAttachments::from_views(color, depth, msaa)
    }
}

/// The load ops a frame is opened with: the first [RenderLoadOps] in `world`,
/// or the defaults when no entity carries one.
///
/// Load ops are their own component, so they need no particular entity: any
/// one may carry them, and the renderer reads only the first.
fn frame_load_ops(world: &LocalWorld) -> RenderLoadOps {
    world
        .query::<&RenderLoadOps>()
        .next()
        .map_or_else(RenderLoadOps::default, |(_, ops)| *ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_world_without_load_ops_clears_with_the_defaults() {
        let world = LocalWorld::new();

        assert_eq!(frame_load_ops(&world), RenderLoadOps::default());
    }

    #[test]
    fn load_ops_are_read_from_any_entity() {
        let mut world = LocalWorld::new();
        // The entity carries load ops and nothing else: the component needs
        // no camera, no transform and no mesh to take effect.
        world.spawn((RenderLoadOps {
            color: wgpu::LoadOp::Load,
            ..Default::default()
        },));

        assert_eq!(frame_load_ops(&world).color, wgpu::LoadOp::Load);
    }

    #[test]
    fn the_first_load_ops_entity_wins() {
        let mut world = LocalWorld::new();
        let first = world.spawn((RenderLoadOps {
            depth: wgpu::LoadOp::Load,
            ..Default::default()
        },));
        world.spawn((RenderLoadOps {
            depth: wgpu::LoadOp::Clear(1.0),
            ..Default::default()
        },));

        assert_eq!(frame_load_ops(&world).depth, wgpu::LoadOp::Load);
        assert!(world.get::<RenderLoadOps>(first).is_some());
    }
}
