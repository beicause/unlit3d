//! The frame driver: assembling one frame out of its passes and sources.
//!
//! A [`Renderer`] draws nothing itself. It drives the frame's [`FramePass`]es:
//! each pass states what it draws into, every
//! [`FrameSource`](crate::source::FrameSource) whose scope reaches the pass
//! fills the pass's [`Scene`] from the world, and the
//! driver records that scene in [`FrameSourceOrder`] into the pass. The passes
//! themselves record in [`FramePassOrder`]. That is the whole of it —
//! the built-in mesh rendering is one source
//! ([`MeshSource`](crate::mesh_source::MeshSource)) and has no more privilege
//! than a caller's own.
//!
//! A scene belongs to a (pass, source) pair rather than to the source: a
//! source several passes see is built once per pass, into that pass's own
//! scene, so each pass records exactly the draws its own target asked for.
//!
//! The GPU state the frame is drawn with — the device, the queue and the
//! resource graph — lives in the world as resource components, addressed by a
//! [`RenderContext`]. Spawn them once with
//! [`spawn_context`](crate::source::spawn_context); the renderer keeps the
//! context's entity ids so it can reach the device and queue every frame.

use std::collections::{BTreeMap, BTreeSet};

use unlit_ecs::{Entity, World};
use unlit_wgpu::scene::{Scene, ViewportRect};

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
///     .spawn(Pass::new(FramePassOrder(0)));
/// world
///     .scope_mut(pass)
///     .expect("the pass entity is spawned")
///     .spawn(bundle![Source::new(source)]);
/// let renderer = world.root_scope_mut().spawn(bundle![Renderer::new(ctx)]);
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
    /// The scene each (pass, source) pair built this frame, keyed by the pair.
    ///
    /// A scene is the frame's temporary product rather than the source's
    /// state: a source several passes see is built once per pass, and each
    /// pass needs the draws for its own target. The slots outlive the frame so
    /// a steady scene reuses their allocations and a lookup finds one without
    /// a scan; a pair the world no longer reaches gives its buffers back at the
    /// end of the frame.
    scenes: BTreeMap<(Entity, Entity), SceneSlot>,
    /// Reports sources no pass reaches, without repeating itself every frame.
    placement_warnings: PlacementWarnings,
    /// The sources some pass reached this frame, reused to sort out the ones
    /// no pass reached.
    reached: BTreeSet<Entity>,
    /// The sources no pass reached this frame, reused between frames.
    unreached: Vec<Entity>,
}

/// The scene of one (pass, source) pair, kept between frames.
#[derive(Default)]
struct SceneSlot {
    /// The draws of the last frame the pair was built in.
    scene: Scene,
    /// Whether the pair was built this frame; a slot left unbuilt is dropped.
    used: bool,
}

/// Reports sources that no pass reaches, and so never draws.
///
/// A source is drawn by the passes whose scope contains it: one under a pass
/// draws into that pass, one in the shared subtree draws into every pass. A
/// source that only the root reaches is drawn by none of them, which is silent
/// otherwise, so it is reported once and again only when the set changes.
#[derive(Default)]
struct PlacementWarnings {
    reported: Vec<Entity>,
}

impl PlacementWarnings {
    /// Report `unreached` unless it is the set reported last time, and return
    /// whether there was anything to report.
    fn check(&mut self, unreached: &[Entity]) -> bool {
        if self.reported == unreached {
            return false;
        }
        if !unreached.is_empty() {
            log::warn!(
                "{} sources are reached by no pass, so nothing draws them: entities {:?}; move each one under the pass it belongs to, or into the shared subtree to draw it into every pass",
                unreached.len(),
                unreached
            );
        }
        self.reported.clear();
        self.reported.extend_from_slice(unreached);
        !unreached.is_empty()
    }
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
            scenes: BTreeMap::new(),
            placement_warnings: PlacementWarnings::default(),
            reached: BTreeSet::new(),
            unreached: Vec::new(),
        }
    }

    /// The world addresses of the frame's GPU state.
    pub fn context(&self) -> RenderContext {
        self.context
    }

    /// Render one frame from the ECS `world`.
    ///
    /// The frame is assembled in two phases. First every source of every pass
    /// fills that pass's scene, taking its own short-lived borrow of the world;
    /// then the passes are recorded in [`FramePassOrder`], and within each pass
    /// its sources in [`FrameSourceOrder`]. A source draws into the passes whose
    /// scope reaches it, so its place in the entity tree is what chooses its
    /// pass — and a source the scopes of several passes reach is built once per
    /// pass, each time into that pass's own scene. A source no pass reaches
    /// draws nowhere, and is reported once.
    ///
    /// The [`RenderLoadOps`] on each pass entity decide what that pass loads
    /// and clears, so two passes may disagree about it — the usual way to draw
    /// an overlay over what an earlier pass left. [`Pass`](crate::pass::Pass)
    /// spawns the two together, while a pass entity spawned without load ops of
    /// its own is opened with [`RenderLoadOps::default`]. A pass with no attachment to
    /// draw into is skipped, so a frame always records, and a world with no
    /// camera, no visible mesh or no source at all still applies its clears.
    /// Parking a pass that way leaves the sources under it where they are: a
    /// pass still reaches them, so they are not reported as misplaced.
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

        // Every scene of the last frame is stale until this frame builds it
        // again, and the two placement lists are this frame's.
        for slot in self.scenes.values_mut() {
            slot.used = false;
        }
        self.reached.clear();
        self.unreached.clear();

        // Build phase: each source fills the scene of the pass it is built for,
        // taking its own short-lived borrow of the world for the GPU state it
        // needs. Nothing of the graph or the encoder outlives this loop's body.
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
            let context = self.context;
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
                    let scene = self.scene_for(pass_entity, entity);
                    source.build_scene(&scope, context, info, scene, &mut encoder);
                }
            }
        }

        // A source the root reaches but no pass does draws nowhere, which
        // would otherwise be silent: it is in the tree, it is asked to build
        // nothing, and the frame simply does not show it. Whether a pass can
        // draw is not part of that question — see `note_placement`.
        {
            profiling::scope!("renderer.frame.check_placement");
            self.note_placement(world);
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
                    .map(|(entity, pass)| (pass.order, entity)),
            );
            self.pass_warnings.check(self.passes.ambiguous());
        }

        for index in 0..self.passes.entities().len() {
            let pass_entity = self.passes.entities()[index];
            let root = world.root_scope();
            let Ok(pass) = root.query_one::<&FramePass>(pass_entity) else {
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
                // Ambiguities are remembered per pass, so two passes with
                // their own tied sources do not keep overwriting each other's
                // report and warn every frame.
                self.source_warnings
                    .check_for(pass_entity, self.sources.ambiguous());
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
                let whole =
                    ViewportRect::new(attachments.width() as f32, attachments.height() as f32);
                let load = root
                    .query_one::<&RenderLoadOps>(pass_entity)
                    .map_or_else(|_| RenderLoadOps::default(), |load| *load);
                let mut render_pass =
                    attachments.begin_pass(&mut encoder, load.color, load.depth, load.stencil);
                for index in 0..self.sources.entities().len() {
                    let entity = self.sources.entities()[index];
                    if let Some(scene) = self.scene_slot(pass_entity, entity) {
                        scene.record(&mut render_pass, Some(whole));
                    }
                }
                // The scissor is pass state too, and unlike the viewport it has
                // a survivor: the multisample resolve that ends this pass is a
                // blit on the GL backend, and a blit there is clipped by the
                // scissor. A clipped draw left last — the UI window, say — would
                // resolve only that rectangle and leave the rest of the frame
                // stale. Cover the whole target so the pass ends unclipped.
                render_pass.set_scissor_rect(0, 0, attachments.width(), attachments.height());
            }
        }

        // The frame is over: a (pass, source) pair the world no longer reaches
        // keeps neither its draws nor the report of an ambiguity that no
        // longer exists.
        self.scenes.retain(|_, slot| slot.used);
        self.source_warnings.retain_owners(self.passes.entities());

        {
            profiling::scope!("renderer.frame.submit");
            queue.submit([encoder.finish()]);
        }
    }

    /// The scene `source` fills for `pass` this frame, cleared and ready.
    ///
    /// The pair's slot is reused when it exists, so a steady frame reallocates
    /// nothing; the scene is cleared here rather than by the source, so a
    /// source that returns early leaves no draws of the last frame behind.
    fn scene_for(&mut self, pass: Entity, source: Entity) -> &mut Scene {
        let slot = self.scenes.entry((pass, source)).or_default();
        slot.used = true;
        slot.scene.draws.clear();
        &mut slot.scene
    }

    /// The scene `source` built for `pass` this frame, if it built one.
    fn scene_slot(&self, pass: Entity, source: Entity) -> Option<&Scene> {
        self.scenes
            .get(&(pass, source))
            .filter(|slot| slot.used)
            .map(|slot| &slot.scene)
    }

    /// Collect the sources the passes reach this frame, and report the ones
    /// none of them reaches.
    ///
    /// Every pass counts whether or not it has an attachment to draw into: a
    /// parked pass still owns the sources under it, so parking is a frame loop
    /// waiting for its target rather than the sources being misplaced. What
    /// stays unreached is a source only the root sees — it is in the tree, no
    /// pass asks it to build anything, and the frame does not show it.
    fn note_placement(&mut self, world: &World) {
        let root = world.root_scope();
        for (pass_entity, _) in root.query::<(Entity, &FramePass)>() {
            let pass_scope = world.scope(pass_entity).unwrap_or(root);
            for (entity, _) in pass_scope.query::<(Entity, &Source)>() {
                self.reached.insert(entity);
            }
        }
        for (entity, _) in root.query::<(Entity, &Source)>() {
            if !self.reached.contains(&entity) {
                self.unreached.push(entity);
            }
        }
        self.placement_warnings.check(&self.unreached);
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
            viewport: pass.viewport,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlit_ecs::Scope;

    use crate::pass::{FramePassOrder, Pass};
    use crate::source::{FrameSource, spawn_context};

    use unlit_wgpu::capabilities::DeviceCapabilities;

    /// A source that draws nothing: placement is decided from the tree alone.
    struct Nowhere;

    impl FrameSource for Nowhere {
        fn build_scene(
            &mut self,
            _scope: &Scope<'_>,
            _ctx: RenderContext,
            _info: PassInfo,
            _scene: &mut Scene,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
        }

        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::MESH
        }
    }

    /// A world holding a renderer and one pass, ready to be given sources.
    fn world_with_a_pass() -> (World, Renderer, Entity) {
        let mut world = World::new();
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        let ctx = spawn_context(
            &mut world,
            device,
            queue,
            unlit_wgpu::resources::ResourceGraph::new(),
            DeviceCapabilities::default(),
        );
        let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
        (world, Renderer::new(ctx), pass)
    }

    /// A parked pass still owns the sources under it; a source beside every
    /// pass belongs to none of them.
    #[test]
    fn a_parked_pass_reaches_the_sources_under_it() {
        let (mut world, mut renderer, pass) = world_with_a_pass();
        let under = world
            .scope_mut(pass)
            .expect("the pass is spawned")
            .spawn(unlit_ecs::bundle![Source::new(Nowhere)]);
        let beside = world
            .root_scope_mut()
            .spawn(unlit_ecs::bundle![Source::new(Nowhere)]);

        world
            .root_scope()
            .query_one::<&mut FramePass>(pass)
            .map(|mut pass| pass.park())
            .expect("the pass is spawned");

        renderer.note_placement(&world);
        assert!(
            !renderer.reached.contains(&beside),
            "a source beside the passes is reached by none of them"
        );
        assert_eq!(
            renderer.unreached,
            vec![beside],
            "a parked pass still reaches the source under it, so only the \
             misplaced one is reported"
        );
        assert!(renderer.reached.contains(&under));
    }
}
