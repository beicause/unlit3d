//! The frame driver: assembling one frame out of its passes and sources.
//!
//! A [`Renderer`] draws nothing itself. It drives the frame's [`FramePass`]es:
//! each pass states what it draws into, every
//! [`FrameSource`](crate::source::FrameSource) under it builds its own
//! [`Scene`](unlit_wgpu::scene::Scene) from the world, and the driver records
//! those scenes in [`FrameSourceOrder`] into that pass. The passes themselves
//! record in [`FramePassOrder`]. That is the whole of it —
//! the built-in mesh rendering is one source
//! ([`MeshSource`](crate::mesh_source::MeshSource)) and has no more privilege
//! than a caller's own.
//!
//! The GPU state the frame is drawn with — the device, the queue and the
//! resource graph — lives in the world as resource components, addressed by a
//! [`RenderContext`]. Spawn them once with
//! [`spawn_context`](crate::source::spawn_context); the renderer keeps the
//! context's entity ids so it can reach the device and queue every frame.

use unlit_ecs::{Entity, World};
use unlit_wgpu::scene::full_viewport;

use crate::components::RenderLoadOps;
use crate::order::{OrderWarnings, Ordering};
use crate::pass::{FramePass, FramePassOrder, PassInfo};
use crate::source::{FrameSourceOrder, RenderContext, Source};

/// The top-level frame driver.
///
/// Spawn it once — as a component on a resource entity — and call
/// [`Renderer::render`] every frame. It drives the [`FramePass`]es in the
/// world: each pass records the [`Source`]s under it.
///
/// ```rust
/// # use unlit3d::prelude::*;
/// # use unlit_wgpu::resources::ResourceGraph;
/// # let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
/// let mut world = World::new();
/// // The frame's GPU state, the built-in mesh source and the driver itself.
/// let ctx = spawn_context(
///     &mut world,
///     device,
///     queue,
///     ResourceGraph::new(),
///     DeviceCapabilities::default(),
/// );
/// let source = MeshSource::new(&world.root_scope(), ctx);
/// // One pass, drawing the mesh source under it.
/// let pass = world
///     .root_scope_mut()
///     .spawn(bundle!(FramePass::new(FramePassOrder(0)), RenderLoadOps::default()));
/// world
///     .scope_mut(pass)
///     .expect("the pass entity is spawned")
///     .spawn(bundle!(Source::new(source),));
/// let renderer = world.root_scope_mut().spawn(bundle!(Renderer::new(ctx),));
/// ```
pub struct Renderer {
    /// The world addresses of the frame's GPU state.
    ///
    /// Kept so [`Renderer::render`] can reach the queue to submit through;
    /// everything else a frame needs the world hands out itself.
    context: RenderContext,
    /// The frame's passes in record order, reused every frame so resolving
    /// the order allocates nothing.
    passes: Ordering<FramePassOrder>,
    /// Reports passes that declared the same [`FramePassOrder`], without
    /// repeating itself every frame.
    pass_warnings: OrderWarnings<FramePassOrder>,
    /// The sources of the pass being recorded, in record order.
    ///
    /// One buffer serves every pass in turn, because a pass's sources are
    /// recorded before the next pass is looked at.
    sources: Ordering<FrameSourceOrder>,
    /// Reports sources that declared the same [`FrameSourceOrder`], without
    /// repeating itself every frame.
    source_warnings: OrderWarnings<FrameSourceOrder>,
}

impl Renderer {
    /// Build a renderer driving the frame context `ctx`.
    ///
    /// This creates no GPU resources: `ctx` names the device, the queue and the
    /// resource graph the frame is drawn with, all of which
    /// [`spawn_context`](crate::source::spawn_context) already put in the
    /// world. What the frame draws into is not the renderer's to hold — the
    /// world states it per pass with a [`FramePass`] — so a renderer needs no
    /// binding step before its first [`Self::render`].
    pub fn new(ctx: RenderContext) -> Self {
        Self {
            context: ctx,
            passes: Ordering::default(),
            pass_warnings: OrderWarnings::default(),
            sources: Ordering::default(),
            source_warnings: OrderWarnings::default(),
        }
    }

    /// The world addresses of the frame's GPU state.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// Render one frame from the ECS `world`.
    ///
    /// The frame is assembled in two phases. First every source of every pass
    /// builds its scene, taking its own short-lived borrow of the world; then
    /// the passes are recorded in [`FramePassOrder`], and within each pass its
    /// sources in [`FrameSourceOrder`]. A source draws into whichever pass it
    /// hangs under, so its place in the entity tree is what chooses its pass.
    ///
    /// The [`RenderLoadOps`] on each pass entity decide what that pass loads
    /// and clears, so two passes may disagree about it — the usual way to draw
    /// an overlay over what an earlier pass left. A pass with no attachment to
    /// draw into is skipped, so a frame always records, and a world with no
    /// camera, no visible mesh or no source at all still applies its clears.
    ///
    /// # Panics
    ///
    /// If the context's device, queue or graph resource is gone.
    pub fn render(&mut self, world: &World) {
        profiling::scope!("renderer.frame");

        let device = world
            .root_scope()
            .query_one::<&wgpu::Device>(self.context.device)
            .expect("the context's device resource exists")
            .clone();
        let queue = world
            .root_scope()
            .query_one::<&wgpu::Queue>(self.context.queue)
            .expect("the context's queue resource exists")
            .clone();

        // One encoder carries the frame's staged uploads and its passes, so
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
        // entities is built: the query hands out a `&mut Source` while the rest
        // of the world stays shared for `build_scene` to read.
        {
            profiling::scope!("renderer.frame.build_sources");
            let root = world.root_scope();
            for (pass_entity, pass) in root.query::<(Entity, &FramePass)>() {
                let Some(info) = self.pass_info(world, &pass) else {
                    continue;
                };
                // Each source builds against its own subtree: the source
                // entity is the scope, so what it draws is what hangs below
                // it, and two sources under one pass draw different sets
                // without either one seeing the other's. The resources a
                // source needs — the device, the graph, the frame's target —
                // live in the shared subtree, which every scope sees.
                let pass_scope = world.scope(pass_entity).unwrap_or(root);
                for (entity, mut source) in pass_scope.query::<(Entity, &mut Source)>() {
                    let scope = world.scope(entity).unwrap_or(pass_scope);
                    source.build_scene(&scope, self.context, info, &mut encoder);
                }
            }
        }

        // Record phase: the passes in declared order, and within each pass its
        // sources in theirs. Both orders are resolved into buffers the
        // renderer keeps, so a steady scene reorders nothing.
        {
            profiling::scope!("renderer.frame.resolve_passes");
            self.passes.resolve(
                world
                    .root_scope()
                    .query::<(Entity, &FramePass)>()
                    .map(|(entity, pass)| (pass.order(), entity)),
            );
            self.pass_warnings.check(self.passes.ambiguous());
        }

        for index in 0..self.passes.entities().len() {
            let pass_entity = self.passes.entities()[index];
            let root = world.root_scope();
            let Some(pass) = root.query_one::<&FramePass>(pass_entity) else {
                continue;
            };
            // A pass with no attachment cannot be opened; the world parks one
            // that way while its target is gone.
            let Some(attachments) = pass.attachments(world, self.context) else {
                continue;
            };
            let pass_scope = world.scope(pass_entity).unwrap_or(root);
            {
                profiling::scope!("renderer.frame.resolve_sources");
                self.sources.resolve(
                    pass_scope
                        .query::<(Entity, &Source)>()
                        .map(|(entity, source)| (source.order(), entity)),
                );
                self.source_warnings.check(self.sources.ambiguous());
            }
            {
                profiling::scope!("renderer.frame.record_pass");
                // The viewport is pass state with no reset call, so a scene
                // that states none has to be given the whole target explicitly
                // — otherwise it would inherit the region an earlier scene
                // set and draw inside another scene's letterbox. The pass's own
                // region reaches its sources through PassInfo: one that is part
                // of the content states it on its scene, and one that composes
                // over the content covers the whole target.
                let whole = full_viewport(attachments.width(), attachments.height());
                let load = root
                    .query_one::<&RenderLoadOps>(pass_entity)
                    .map_or_else(RenderLoadOps::default, |load| *load);
                let mut render_pass =
                    attachments.begin_pass(&mut encoder, load.color, load.depth, load.stencil);
                for index in 0..self.sources.entities().len() {
                    let entity = self.sources.entities()[index];
                    pass_scope
                        .query_one::<&mut Source>(entity)
                        .map(|source| source.scene().record_with_viewport(&mut render_pass, whole))
                        .expect("the source entity exists");
                }
            }
        }

        {
            profiling::scope!("renderer.frame.submit");
            queue.submit([encoder.finish()]);
        }
    }

    /// What a pass tells its sources, or `None` when it has no attachment to
    /// draw into and so never records them.
    ///
    /// # Panics
    ///
    /// If one of the pass's views is gone from the graph, or if it is not a
    /// texture view.
    fn pass_info(&self, world: &World, pass: &FramePass) -> Option<PassInfo> {
        let attachments = pass.attachments(world, self.context)?;
        Some(PassInfo {
            surface: attachments.surface_key(),
            width: attachments.width(),
            height: attachments.height(),
            viewport: pass.viewport(),
        })
    }
}
