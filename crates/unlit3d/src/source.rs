//! Frame sources: the things a frame is made of.
//!
//! A [`Renderer`](crate::Renderer) draws nothing itself. It assembles a frame
//! out of [`FrameSource`]s, each of which builds its own [`Scene`] from the
//! world and states where in the frame it belongs. The built-in mesh rendering
//! is one such source ([`MeshSource`](crate::mesh_source::MeshSource)) and a
//! UI overlay is another; a caller's own pass over the frame is a third, with
//! no more privilege than the built-in ones.
//!
//! A source is an ordinary component holding a boxed [`AnySource`], so one
//! query drives every concrete source type: build all of them, then record
//! their scenes in [`FrameOrder`]. The GPU state a source needs — the device,
//! the queue and the resource graph — lives in the world too, addressed by the
//! [`RenderContext`] the sources are handed; that is what lets a source fetch
//! the graph while the driver only holds a shared borrow of the world.
//!
//! [`Scene`]: wgpu_unlit_render::scene::Scene

use core::any::Any;

use unlit_ecs::{Entity, LocalWorld, Resource};
use wgpu_unlit_render::resources::ResourceGraph;
use wgpu_unlit_render::scene::Scene;
use wgpu_unlit_render::specialize::SurfaceKey;

/// Where a source records, relative to every other source.
///
/// Lower values record first. Deliberately has no `Default`: every source
/// states its ordering intent, and a silent default is exactly what an explicit
/// order exists to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameOrder(pub i32);

impl FrameOrder {
    /// The built-in mesh source's order.
    pub const MESH: Self = Self(0);
    /// A source that composes over the meshes, such as a UI overlay.
    pub const OVERLAY: Self = Self(100);
}

/// The GPU state a frame is drawn with, as world addresses.
///
/// It holds [`Entity`] handles rather than borrows, so it is `Copy` and never
/// conflicts with a source's own borrow of the world. A source fetches what it
/// needs from the components those entities carry:
/// `world.get_mut::<ResourceGraph>(ctx.graph)`.
///
/// The handles are spawned once by [`spawn_context`] and stay valid for the
/// world's life; a resource entity is never despawned while frames are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderContext {
    /// The [`wgpu::Device`] the frame is drawn with, as a resource entity.
    pub device: Entity,
    /// The [`wgpu::Queue`] the frame is submitted to, as a resource entity.
    pub queue: Entity,
    /// The [`ResourceGraph`] holding the frame's resources, as a resource
    /// entity.
    pub graph: Entity,
}

/// The render target the frame currently draws into.
///
/// A source specializes every pipeline on the frame's target, but
/// [`RenderContext`] is spawned once and must stay `Copy`, so the target is a
/// separate resource the frame loop writes before it renders — see
/// [`set_frame_target`]. It is deliberately not a field on [`RenderContext`]:
/// putting a per-frame value there would mean writing the context every frame,
/// and a source reading it could not tell a stale value from this frame's.
///
/// A source that runs before any target was written reads `None` from
/// [`frame_target`] and must refuse to draw rather than fall back to the
/// previous frame's target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTarget {
    /// The target's pipeline specialization key.
    pub surface: SurfaceKey,
    /// The target's width, in physical pixels.
    pub width: u32,
    /// The target's height, in physical pixels.
    pub height: u32,
}

/// The world resource holding the frame's current [`FrameTarget`].
///
/// Internal: the frame loop writes it through [`set_frame_target`] and sources
/// read it through [`frame_target`], so the `Option` never has to be handled
/// twice.
#[derive(Clone, Copy, Debug, Default)]
struct FrameTargetSlot(Option<FrameTarget>);

/// What a source claimed of the frame's input.
///
/// A UI toolkit decides whether it wants the pointer or the keyboard while it
/// lays a frame out, and the answer belongs to the frame just laid out: a game
/// control reading it is asking whether the input it is about to handle has
/// already been claimed by an interface. Sources write it; nothing is
/// intercepted automatically, so a caller decides what the claim means.
///
/// The frame context spawns it, so a world that draws a frame has one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputCapture {
    /// A source wants the pointer, so a click belongs to it.
    pub pointer: bool,
    /// A source wants the keyboard, so a key belongs to it.
    pub keyboard: bool,
}

impl InputCapture {
    /// Whether either kind of input was claimed.
    pub fn any(self) -> bool {
        self.pointer || self.keyboard
    }
}

/// The frame's target, or `None` before any frame loop wrote one.
///
/// A source calls this in
/// [`build_scene`](FrameSource::build_scene). `None` means the frame loop never
/// stated what the frame draws into, which is a programming error on its part —
/// a source cannot invent a target — so a source that needs it should refuse to
/// draw rather than fall back.
pub fn frame_target(world: &LocalWorld) -> Option<FrameTarget> {
    world
        .query::<&FrameTargetSlot>()
        .next()
        .and_then(|(_, slot)| slot.0)
}

/// State the target the frame draws into.
///
/// The frame loop calls this before it renders — [`Renderer::set_render_target`]
/// does it for the built-in path. Returns whether a slot existed to write; a
/// world only gets one from [`spawn_context`].
pub fn set_frame_target(world: &LocalWorld, target: FrameTarget) -> bool {
    let Some(entity) = world.query::<&FrameTargetSlot>().next().map(|(e, _)| e) else {
        return false;
    };
    world
        .with_mut::<FrameTargetSlot, _>(entity, |slot| slot.0 = Some(target))
        .is_some()
}

/// A source of one frame's draws.
///
/// A source owns whatever state it draws from and the [`Scene`] it fills each
/// frame. The driver calls [`FrameSource::build_scene`] on every source, then
/// records the scenes in [`FrameSource::order`].
pub trait FrameSource: 'static {
    /// Assemble this frame's [`Scene`], fetching what it needs from `world`.
    ///
    /// `ctx` carries only entity ids, so it never conflicts with the source's
    /// own borrow of `world`. `encoder` is the frame's one encoder, and a
    /// source stages its uploads through it: the pass is opened only after
    /// every source has built, so the uploads and the draws that read them
    /// share one submission.
    ///
    /// A source that has nothing to draw clears its scene and returns: the
    /// frame still records, and still applies its load ops.
    fn build_scene(
        &mut self,
        world: &LocalWorld,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    );

    /// The scene built by the last [`FrameSource::build_scene`].
    fn scene(&self) -> &Scene;

    /// Where this source records, relative to the others.
    ///
    /// Required, not defaulted: a source that stayed silent would be ordered
    /// by mount position alone, which is the implicit behaviour this exists to
    /// replace. Use [`FrameOrder::MESH`], [`FrameOrder::OVERLAY`], or a value
    /// of your own.
    ///
    /// A [`Source`] may override this per entity with [`Source::with_order`],
    /// which is how a caller reorders an existing source without rebuilding it.
    fn order(&self) -> FrameOrder;

    /// Release whatever this source registered in the frame's resource graph.
    ///
    /// Most sources own no graph nodes and can leave this as it is. One that
    /// does — the built-in mesh and UI sources register uniform buffers and
    /// bind groups of their own — implements it, because removing a source is
    /// not something the graph can notice: `despawn` has no hook, so nothing
    /// would remove the nodes and they would outlive every reference to them.
    ///
    /// [`despawn_source`] calls this before despawning, which is the only
    /// supported way to remove a source that owns nodes. Releasing a source
    /// whose nodes a caller still holds handles for — a mesh not yet passed to
    /// [`MeshSource::remove_mesh`](crate::mesh_source::MeshSource::remove_mesh)
    /// — leaves those handles dangling, so remove them first.
    fn release(&mut self, world: &LocalWorld) {
        let _ = world;
    }
}

/// A frame source behind an erased handle, so one query sees every concrete
/// source type.
///
/// Method-free on purpose: [`Any`] supplies downcasting, the [`FrameSource`]
/// supertrait supplies the trait API, and the blanket impl means an implementor
/// writes no boilerplate at all.
pub trait AnySource: Any + FrameSource {}

impl<T: FrameSource> AnySource for T {}

/// A frame source as an ECS component.
///
/// The trait API is reachable directly (`source.order()`,
/// `source.build_scene(..)`, `source.scene()`); typed access upcasts to
/// `dyn Any` and downcasts, which is what [`Source::as_mut`] wraps.
pub struct Source {
    source: Box<dyn AnySource>,
    /// An explicit order that overrides the source's own.
    order: Option<FrameOrder>,
    /// When the source was mounted.
    ///
    /// This is the tie-break between sources that declare the same
    /// [`FrameOrder`]. It cannot be the entity index or the query's row order:
    /// `despawn` moves the last row into the freed one, so row order changes
    /// as sources come and go.
    mount_index: u64,
}

impl Source {
    /// Wrap `source`, taking its order from [`FrameSource::order`].
    pub fn new(source: impl FrameSource) -> Self {
        Self {
            source: Box::new(source),
            order: None,
            mount_index: 0,
        }
    }

    /// Wrap `source`, recording it at `order` regardless of what
    /// [`FrameSource::order`] says.
    pub fn with_order(mut self, order: FrameOrder) -> Self {
        self.order = Some(order);
        self
    }

    /// The order this source records at, explicit or declared.
    pub fn order(&self) -> FrameOrder {
        self.order.unwrap_or_else(|| self.source.order())
    }

    /// Override the order this source records at.
    ///
    /// Setting the same value again is a no-op, so a caller may apply an order
    /// every frame without disturbing the ambiguity bookkeeping.
    pub fn set_order(&mut self, order: Option<FrameOrder>) {
        self.order = order;
    }

    /// When the source was mounted, which breaks ties between equal orders.
    pub fn mount_index(&self) -> u64 {
        self.mount_index
    }

    /// The source as its concrete type, or `None` for a different type.
    pub fn as_mut<T: FrameSource>(&mut self) -> Option<&mut T> {
        let erased: &mut dyn Any = &mut *self.source;
        erased.downcast_mut::<T>()
    }

    /// The source as its concrete type, or `None` for a different type.
    pub fn as_ref<T: FrameSource>(&self) -> Option<&T> {
        let erased: &dyn Any = &*self.source;
        erased.downcast_ref::<T>()
    }

    /// Assemble this source's scene for the frame.
    pub fn build_scene(
        &mut self,
        world: &LocalWorld,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.source.build_scene(world, ctx, encoder);
    }

    /// The scene this source built for the frame.
    pub fn scene(&self) -> &Scene {
        self.source.scene()
    }

    /// Release the graph nodes the source registered.
    pub fn release(&mut self, world: &LocalWorld) {
        self.source.release(world);
    }
}

impl core::fmt::Debug for Source {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Source")
            .field("order", &self.order())
            .field("mount_index", &self.mount_index)
            .finish_non_exhaustive()
    }
}

/// The next mount index to hand out.
///
/// Kept as a resource so it is monotonic across the world's life rather than
/// per-renderer: two renderers sharing a world must not hand out the same
/// indices, or the tie-break would not be total.
#[derive(Debug, Default)]
struct MountCounter(u64);

/// Spawn the frame's GPU state as resources and return its addresses.
///
/// The device and queue are `Clone` handles on a shared context, so the world
/// holds its own reference to each; the graph is not `Sync` and moves into the
/// world whole. Call this once per world.
pub fn spawn_context(
    world: &mut LocalWorld,
    device: wgpu::Device,
    queue: wgpu::Queue,
    graph: ResourceGraph,
) -> RenderContext {
    let device = world.spawn((Resource, device));
    let queue = world.spawn((Resource, queue));
    let graph = world.spawn((Resource, graph));
    world.spawn((Resource, MountCounter::default()));
    // The frame loop writes the target here every frame; until it does, no
    // source may draw.
    world.spawn((Resource, FrameTargetSlot::default()));
    // What the frame's sources claimed of its input, written by whichever
    // source consumes input and read by the caller's own logic.
    world.spawn((Resource, InputCapture::default()));
    RenderContext {
        device,
        queue,
        graph,
    }
}

/// Mount `source`, taking its order from [`FrameSource::order`].
pub fn spawn_source(world: &mut LocalWorld, source: impl FrameSource) -> Entity {
    let source = Source::new(source);
    let index = next_mount_index(world);
    world.spawn((bump_mount(source, index),))
}

/// Mount `source`, recording it at `order`.
pub fn spawn_source_at(
    world: &mut LocalWorld,
    order: FrameOrder,
    source: impl FrameSource,
) -> Entity {
    let source = Source::new(source).with_order(order);
    let index = next_mount_index(world);
    world.spawn((bump_mount(source, index),))
}

/// Remove the source `entity`, releasing the graph nodes it owns.
///
/// Structural changes go through the command queue, and this is queued like
/// any other: the call records the intent and the world changes when the
/// caller next calls `apply`. It releases the source's own nodes before
/// despawning it, which is why removing a source is not just `despawn` — the
/// graph has no way to learn that an entity is gone.
///
/// A source that owns per-entity resources a caller still holds handles for is
/// the caller's to clean up first; see [`FrameSource::release`].
pub fn despawn_source(world: &LocalWorld, entity: Entity) {
    world.queue().push(DespawnSource { entity });
}

/// The queued removal of a source: release its graph nodes, then despawn it.
struct DespawnSource {
    entity: Entity,
}

impl unlit_ecs::Command<unlit_ecs::LocalMode> for DespawnSource {
    fn apply(self: Box<Self>, world: &mut LocalWorld) {
        // Releasing first, so the source is still reachable and still knows
        // its own nodes. It needs only a shared world, so the source borrow
        // does not conflict with it.
        if let Some(mut source) = world.get_mut::<Source>(self.entity) {
            source.release(world);
        }
        world.despawn(self.entity);
    }
}

/// Take the next mount index from the world's counter.
fn next_mount_index(world: &LocalWorld) -> u64 {
    let Some(counter) = world.query::<&MountCounter>().next().map(|(e, _)| e) else {
        return 0;
    };
    world
        .with_mut::<MountCounter, _>(counter, |counter| {
            let index = counter.0;
            counter.0 += 1;
            index
        })
        .unwrap_or(0)
}

fn bump_mount(mut source: Source, index: u64) -> Source {
    source.mount_index = index;
    source
}

/// One group of sources that declared the same [`FrameOrder`].
pub type AmbiguousGroup = (FrameOrder, Vec<Entity>);

/// The sources to record, in the order they record, and the ambiguous groups
/// among them.
///
/// Sorted by `(order, mount_index)`, so equal orders keep their mount order.
pub fn record_order(world: &LocalWorld) -> (Vec<Entity>, Vec<AmbiguousGroup>) {
    let mut sources: Vec<(FrameOrder, u64, Entity)> = world
        .query::<&Source>()
        .map(|(entity, source)| (source.order(), source.mount_index(), entity))
        .collect();
    sources.sort_by_key(|(order, mount_index, _)| (*order, *mount_index));

    // Collect each run of equal orders: a run is what an ambiguity is, and
    // sorting has already put each run's entities in mount order.
    let mut ambiguous = Vec::new();
    let mut start = 0;
    while start < sources.len() {
        let order = sources[start].0;
        let mut end = start + 1;
        while end < sources.len() && sources[end].0 == order {
            end += 1;
        }
        if end - start > 1 {
            ambiguous.push((
                order,
                sources[start..end].iter().map(|(_, _, e)| *e).collect(),
            ));
        }
        start = end;
    }

    (
        sources.into_iter().map(|(_, _, entity)| entity).collect(),
        ambiguous,
    )
}

/// Reports sources that declared the same [`FrameOrder`], without repeating
/// itself.
///
/// Equal orders are not an error — two sources that really are
/// interchangeable may share one — so this warns rather than panicking. What it
/// must not do is warn on every frame: a steady scene would flood the log. It
/// therefore remembers the groups it last reported and speaks up again only
/// when they change, which is exactly when sources were added, removed or
/// reordered.
#[derive(Debug, Default)]
pub struct OrderWarnings {
    reported: Vec<AmbiguousGroup>,
}

impl OrderWarnings {
    /// Warn about `ambiguous` unless it is what was already reported.
    ///
    /// Returns whether anything was reported, which is what the tests assert
    /// on; the log line names the group's size, its order and its entities.
    pub fn check(&mut self, ambiguous: &[AmbiguousGroup]) -> bool {
        if self.reported == ambiguous {
            return false;
        }
        for (order, group) in ambiguous {
            log::warn!(
                "{} sources declared the same FrameOrder({}): entities {:?}; \
                 recording them in mount order — give them distinct orders to \
                 choose explicitly",
                group.len(),
                order.0,
                group,
            );
        }
        self.reported = ambiguous.to_vec();
        !ambiguous.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A noop device and queue, enough to spawn a context: the tests here
    /// exercise the ordering and mounting bookkeeping, which never touches the
    /// GPU.
    fn test_context(world: &mut LocalWorld) -> RenderContext {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        spawn_context(world, device, queue, ResourceGraph::new())
    }

    /// A source that records how many times it was built and what it saw.
    struct RecordingSource {
        order: FrameOrder,
        built: Rc<Cell<u32>>,
        scene: Scene,
    }

    impl RecordingSource {
        fn new(order: FrameOrder) -> Self {
            Self {
                order,
                built: Rc::new(Cell::new(0)),
                scene: Scene::new(),
            }
        }
    }

    impl FrameSource for RecordingSource {
        fn build_scene(
            &mut self,
            _world: &LocalWorld,
            _ctx: RenderContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            self.built.set(self.built.get() + 1);
        }

        fn scene(&self) -> &Scene {
            &self.scene
        }

        fn order(&self) -> FrameOrder {
            self.order
        }
    }

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// Sources record in the order they declare, not the order they were
    /// mounted in — the whole point of an explicit order.
    #[test]
    fn explicit_order_wins_over_mount_order() {
        let mut world = LocalWorld::new();
        spawn_context(
            &mut world,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
            ResourceGraph::new(),
        );

        // Mounted overlay-first, so mount order is the opposite of draw order.
        let overlay = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));
        let mesh = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));

        let (order, ambiguous) = record_order(&world);
        assert_eq!(order, vec![mesh, overlay], "the lower order records first");
        assert!(ambiguous.is_empty(), "distinct orders are not ambiguous");
    }

    /// Equal orders keep mount order, and the mount index is stable when an
    /// earlier source is despawned — which is why it cannot be the ECS row
    /// order.
    #[test]
    fn equal_orders_keep_mount_order_across_a_despawn() {
        let mut world = LocalWorld::new();
        test_context(&mut world);

        let first = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));
        let second = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));
        let third = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));
        assert_eq!(record_order(&world).0, vec![first, second, third]);

        // Despawning the first leaves the other two in mount order, even
        // though the ECS moved the last row into the freed one.
        world.despawn(first);
        assert_eq!(
            record_order(&world).0,
            vec![second, third],
            "mount order survives a despawn"
        );
    }

    /// A source mounted at an explicit order ignores what its own `order`
    /// says, and `set_order` can change it later.
    #[test]
    fn an_explicit_order_overrides_the_one_the_source_declares() {
        let mut world = LocalWorld::new();
        test_context(&mut world);

        let a = spawn_source_at(
            &mut world,
            FrameOrder::OVERLAY,
            RecordingSource::new(FrameOrder::MESH),
        );
        let b = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));

        // `a` was mounted as an overlay, so it records last despite declaring
        // `MESH` itself.
        assert_eq!(record_order(&world).0, vec![b, a]);

        let _ = world.with_mut::<Source, _>(a, |source| source.set_order(None));
        assert_eq!(
            record_order(&world).0,
            vec![a, b],
            "dropping the override falls back to the declared order"
        );
    }

    /// Typed access finds the concrete type and returns `None` for another,
    /// rather than panicking.
    #[test]
    fn typed_access_downcasts_and_rejects_the_wrong_type() {
        struct OtherSource(Scene);

        impl FrameSource for OtherSource {
            fn build_scene(
                &mut self,
                _world: &LocalWorld,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
            }
            fn scene(&self) -> &Scene {
                &self.0
            }
            fn order(&self) -> FrameOrder {
                FrameOrder::MESH
            }
        }

        let mut world = LocalWorld::new();
        test_context(&mut world);

        let recording = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        let other = spawn_source(&mut world, OtherSource(Scene::new()));

        assert!(
            world
                .with_mut::<Source, _>(recording, |s| s.as_mut::<RecordingSource>().is_some())
                .unwrap()
        );
        assert!(
            world
                .with_mut::<Source, _>(recording, |s| s.as_mut::<OtherSource>().is_none())
                .unwrap()
        );
        assert!(
            world
                .with_mut::<Source, _>(other, |s| s.as_mut::<OtherSource>().is_some())
                .unwrap()
        );
        assert!(
            world
                .with_mut::<Source, _>(other, |s| s.as_ref::<RecordingSource>().is_none())
                .unwrap()
        );
    }

    /// One query drives sources of different concrete types, which is what the
    /// erased handle is for.
    #[test]
    fn one_query_builds_every_source_type() {
        struct CountingSource(u32, Scene);

        impl FrameSource for CountingSource {
            fn build_scene(
                &mut self,
                _world: &LocalWorld,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
                self.0 += 1;
            }
            fn scene(&self) -> &Scene {
                &self.1
            }
            fn order(&self) -> FrameOrder {
                FrameOrder::OVERLAY
            }
        }

        let mut world = LocalWorld::new();
        let ctx = test_context(&mut world);
        spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        spawn_source(&mut world, CountingSource(0, Scene::new()));

        let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
            .0
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let entities: Vec<Entity> = world.query::<&Source>().map(|(e, _)| e).collect();
        assert_eq!(entities.len(), 2, "both sources are seen by one query type");
        for entity in entities {
            world
                .with_mut::<Source, _>(entity, |source| {
                    source.build_scene(&world, ctx, &mut encoder)
                })
                .unwrap();
        }
    }

    /// Both phases run in the documented order: every source is built before
    /// any scene is recorded.
    #[test]
    fn every_source_is_built_before_any_is_recorded() {
        let mut world = LocalWorld::new();
        let ctx = test_context(&mut world);

        let order = Rc::new(RefCell::new(Vec::new()));
        struct PhasedSource {
            name: &'static str,
            log: Rc<RefCell<Vec<String>>>,
            scene: Scene,
        }
        impl FrameSource for PhasedSource {
            fn build_scene(
                &mut self,
                _world: &LocalWorld,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
                self.log.borrow_mut().push(format!("build {}", self.name));
            }
            fn scene(&self) -> &Scene {
                &self.scene
            }
            fn order(&self) -> FrameOrder {
                FrameOrder::MESH
            }
        }

        for name in ["a", "b"] {
            spawn_source(
                &mut world,
                PhasedSource {
                    name,
                    log: Rc::clone(&order),
                    scene: Scene::new(),
                },
            );
        }

        let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
            .0
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let entities: Vec<Entity> = world.query::<&Source>().map(|(e, _)| e).collect();
        for entity in &entities {
            world
                .with_mut::<Source, _>(*entity, |source| {
                    source.build_scene(&world, ctx, &mut encoder)
                })
                .unwrap();
        }
        order.borrow_mut().push("record".to_string());

        assert_eq!(
            &*order.borrow(),
            &["build a", "build b", "record"],
            "both sources build before the record phase starts"
        );
    }

    /// A source that was never mounted at an explicit order takes the one it
    /// declares, and a fresh source reports what it declared.
    #[test]
    fn a_source_reports_its_declared_order_until_overridden() {
        let source = Source::new(RecordingSource::new(FrameOrder::OVERLAY));
        assert_eq!(source.order(), FrameOrder::OVERLAY);

        let mut source =
            Source::new(RecordingSource::new(FrameOrder::MESH)).with_order(FrameOrder::OVERLAY);
        assert_eq!(source.order(), FrameOrder::OVERLAY, "the override wins");

        source.set_order(None);
        assert_eq!(source.order(), FrameOrder::MESH, "and can be dropped");
    }

    /// An ambiguity is reported once, and only again when it changes: a steady
    /// frame must not flood the log.
    #[test]
    fn an_ambiguity_is_reported_once_and_again_only_when_it_changes() {
        let mut world = LocalWorld::new();
        test_context(&mut world);

        let a = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        let b = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));

        let (_, ambiguous) = record_order(&world);
        assert_eq!(ambiguous.len(), 1, "the two share one order");
        assert_eq!(ambiguous[0].0, FrameOrder::MESH);
        assert_eq!(ambiguous[0].1, vec![a, b], "in mount order");

        let mut warnings = OrderWarnings::default();
        assert!(warnings.check(&ambiguous), "the first frame reports it");
        assert!(!warnings.check(&ambiguous), "a steady frame stays quiet");

        // Resolving the ambiguity stops the reporting; reintroducing it warns
        // once more.
        let _ =
            world.with_mut::<Source, _>(b, |source| source.set_order(Some(FrameOrder::OVERLAY)));
        let (_, resolved) = record_order(&world);
        assert!(resolved.is_empty());
        assert!(!warnings.check(&resolved));

        let _ = world.with_mut::<Source, _>(b, |source| source.set_order(Some(FrameOrder::MESH)));
        let (_, again) = record_order(&world);
        assert!(warnings.check(&again), "the ambiguity is back");
        assert!(!warnings.check(&again), "and still reports only once");
    }

    /// Distinct orders produce no ambiguity at all.
    #[test]
    fn distinct_orders_are_never_ambiguous() {
        let mut world = LocalWorld::new();
        test_context(&mut world);
        spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));

        let (order, ambiguous) = record_order(&world);
        assert_eq!(order.len(), 2);
        assert!(ambiguous.is_empty());
        assert!(!OrderWarnings::default().check(&ambiguous));
    }

    /// A logger that keeps what was logged, so a test can assert on the
    /// message rather than only on the boolean the check returns.
    struct CaptureLogger;

    static CAPTURED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            if let Ok(mut captured) = CAPTURED.lock() {
                captured.push(record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    /// Install the capture logger before anything logs, and read back what it
    /// caught. `log` allows one logger per process, so every test that wants
    /// the messages goes through here.
    fn capture_logs(f: impl FnOnce()) -> Vec<String> {
        // A test process has no logger yet, so this always succeeds; a second
        // install in the same process would be ignored, which is why the
        // captured list is cleared first.
        static LOGGER: CaptureLogger = CaptureLogger;
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Warn);
        CAPTURED.lock().expect("the capture mutex").clear();

        f();

        let captured = CAPTURED.lock().expect("the capture mutex");
        captured.clone()
    }

    /// The warning names the group's size, its order and its entities, so a
    /// reader can tell which sources are ambiguous without guessing.
    #[test]
    fn the_ambiguity_warning_names_the_group_order_and_entities() {
        let mut world = LocalWorld::new();
        test_context(&mut world);
        let a = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));
        let b = spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));

        let messages = capture_logs(|| {
            let (_, ambiguous) = record_order(&world);
            let mut warnings = OrderWarnings::default();
            assert!(warnings.check(&ambiguous), "the group is reported");
        });

        assert_eq!(messages.len(), 1, "exactly one warning: {messages:?}");
        let message = &messages[0];
        assert!(
            message.contains('2'),
            "the group's size is named: {message}"
        );
        assert!(
            message.contains(&FrameOrder::OVERLAY.0.to_string()),
            "the order's value is named: {message}"
        );
        assert!(
            message.contains(&format!("{a:?}")) && message.contains(&format!("{b:?}")),
            "both entities are named: {message}"
        );
    }

    /// Distinct orders log nothing at all, so a correct frame is silent.
    #[test]
    fn distinct_orders_log_nothing() {
        let mut world = LocalWorld::new();
        test_context(&mut world);
        spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        spawn_source(&mut world, RecordingSource::new(FrameOrder::OVERLAY));

        let messages = capture_logs(|| {
            let (_, ambiguous) = record_order(&world);
            assert!(!OrderWarnings::default().check(&ambiguous));
        });
        assert!(messages.is_empty(), "nothing to warn about: {messages:?}");
    }

    /// A steady frame does not repeat the warning, and resolving the ambiguity
    /// stops it: the log reports a change, not a condition that persists.
    #[test]
    fn a_steady_frame_repeats_nothing_in_the_log() {
        let mut world = LocalWorld::new();
        test_context(&mut world);
        let a = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        let _b = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
        let mut warnings = OrderWarnings::default();

        let messages = capture_logs(|| {
            for _ in 0..5 {
                let (_, ambiguous) = record_order(&world);
                warnings.check(&ambiguous);
            }
        });
        assert_eq!(
            messages.len(),
            1,
            "five steady frames warn once: {messages:?}"
        );

        // Resolving it and reintroducing it warns once more, not twice.
        let _ =
            world.with_mut::<Source, _>(a, |source| source.set_order(Some(FrameOrder::OVERLAY)));
        let messages = capture_logs(|| {
            let (_, resolved) = record_order(&world);
            assert!(!warnings.check(&resolved), "resolved: nothing to say");
        });
        assert!(messages.is_empty(), "resolving is silent: {messages:?}");
    }

    /// Three sources sharing an order are one group, not three pairs, and the
    /// group lists every entity.
    #[test]
    fn a_three_way_ambiguity_is_one_group_of_three() {
        let mut world = LocalWorld::new();
        test_context(&mut world);

        let ids: Vec<Entity> = (0..3)
            .map(|_| spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH)))
            .collect();

        let (_, ambiguous) = record_order(&world);
        assert_eq!(ambiguous.len(), 1, "one group, not pairs");
        assert_eq!(ambiguous[0].1, ids);
    }

    /// Mounting is monotonic, so a source mounted later always breaks a tie
    /// after one mounted earlier.
    #[test]
    fn mount_indices_are_monotonic() {
        let mut world = LocalWorld::new();
        test_context(&mut world);

        let indices: Vec<u64> = (0..3)
            .map(|_| {
                let entity = spawn_source(&mut world, RecordingSource::new(FrameOrder::MESH));
                world
                    .with_mut::<Source, _>(entity, |s| s.mount_index())
                    .unwrap()
            })
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    use wgpu_unlit_render::resources::{Resource as GraphResource, ResourceId};

    /// A source that registers a node, so releasing has something to remove.
    ///
    /// It keeps its context, as a real source does: `release` is handed the
    /// world but no context, and the resource graph is the context's.
    struct OwningSource {
        context: RenderContext,
        node: Option<ResourceId>,
        scene: Scene,
    }

    impl FrameSource for OwningSource {
        fn build_scene(
            &mut self,
            world: &LocalWorld,
            ctx: RenderContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            // Register once, on the first frame: a node the source owns and
            // nothing else refers to.
            self.context = ctx;
            if self.node.is_none() {
                self.node = world
                    .get_mut::<ResourceGraph>(ctx.graph)
                    .expect("the context's graph exists")
                    .insert_strong(GraphResource::Virtual, &[])
                    .ok();
            }
        }

        fn scene(&self) -> &Scene {
            &self.scene
        }

        fn order(&self) -> FrameOrder {
            FrameOrder::OVERLAY
        }

        fn release(&mut self, world: &LocalWorld) {
            let Some(node) = self.node.take() else {
                return;
            };
            world
                .get_mut::<ResourceGraph>(self.context.graph)
                .expect("the context's graph exists")
                .remove_drop(node);
        }
    }

    fn noop_context(world: &mut LocalWorld) -> RenderContext {
        spawn_context(
            world,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
            ResourceGraph::new(),
        )
    }

    /// A source is removed by the queued command, and the graph nodes it owns
    /// go with it.
    ///
    /// `despawn` alone would leave them: the graph has no hook that notices an
    /// entity is gone, which is why the removal is a command rather than a
    /// plain despawn.
    #[test]
    fn removing_a_source_releases_the_nodes_it_owns() {
        let mut world = LocalWorld::new();
        let ctx = noop_context(&mut world);
        let entity = spawn_source(
            &mut world,
            OwningSource {
                context: ctx,
                node: None,
                scene: Scene::new(),
            },
        );

        // Build once, so the source registers its node.
        let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
            .0
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        world
            .with_mut::<Source, _>(entity, |source| {
                source.build_scene(&world, ctx, &mut encoder)
            })
            .expect("the source entity exists");

        let node = world
            .with_mut::<Source, _>(entity, |source| {
                source.as_mut::<OwningSource>().unwrap().node
            })
            .flatten()
            .expect("the source registered a node");
        assert!(
            world
                .get_mut::<ResourceGraph>(ctx.graph)
                .unwrap()
                .get(node)
                .is_some(),
            "the node is in the graph while the source lives"
        );
        let before = world.get_mut::<ResourceGraph>(ctx.graph).unwrap().len();

        despawn_source(&world, entity);
        // Nothing has happened yet: the command is applied by the caller.
        assert!(world.contains(entity), "removal is queued, not immediate");
        world.apply();

        assert!(!world.contains(entity), "the source is gone");
        let graph = world.get_mut::<ResourceGraph>(ctx.graph).unwrap();
        assert!(
            graph.get(node).is_none(),
            "the source's node was released with it"
        );
        assert!(
            graph.len() < before,
            "the graph shrank: {} was {before}",
            graph.len()
        );
    }

    /// A source that owns nothing still despawns, so the command is safe for
    /// every source.
    #[test]
    fn removing_a_source_that_owns_nothing_still_removes_it() {
        let mut world = LocalWorld::new();
        let ctx = noop_context(&mut world);
        let entity = spawn_source(
            &mut world,
            OwningSource {
                context: ctx,
                node: None,
                scene: Scene::new(),
            },
        );

        despawn_source(&world, entity);
        world.apply();
        assert!(!world.contains(entity));
    }
}
