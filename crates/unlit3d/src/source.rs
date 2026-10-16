//! Frame sources: the things a frame is made of.
//!
//! A [`Renderer`](crate::renderer::Renderer) draws nothing itself. It assembles a frame
//! out of [`FrameSource`]s, each of which builds its own [`Scene`] from the
//! world and states where in the frame it belongs. The built-in mesh rendering
//! is one such source ([`MeshSource`](crate::mesh_source::MeshSource)) and a
//! UI overlay is another; a caller's own pass over the frame is a third, with
//! no more privilege than the built-in ones.
//!
//! A source is an ordinary component holding a boxed [`AnySource`], so one
//! query drives every concrete source type: build all of them, then record
//! their scenes in [`FrameSourceOrder`]. The GPU state a source needs — the device,
//! the queue and the resource graph — lives in the world too, addressed by the
//! [`RenderContext`] the sources are handed; that is what lets a source fetch
//! the graph while the driver only holds a shared borrow of the world.
//!
//! [`Scene`]: unlit_wgpu::scene::Scene

use core::any::Any;

use unlit_ecs::{Entity, World};
use unlit_wgpu::capabilities::DeviceCapabilities;
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::scene::{Scene, ViewportRect};
use unlit_wgpu::specialize::SurfaceKey;

/// Where a source records, relative to every other source.
///
/// Lower values record first. Deliberately has no `Default`: every source
/// states its ordering intent, and a silent default is exactly what an explicit
/// order exists to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameSourceOrder(pub i32);

impl FrameSourceOrder {
    /// The built-in mesh source's order.
    pub const MESH: Self = Self(0);
    /// A source that composes over the meshes, such as the UI.
    pub const UI: Self = Self(100);
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
///
/// The context is itself a component: [`spawn_context`] stores a copy on the
/// entity carrying the [`ResourceGraph`], so a reader that was handed one of
/// the four addresses can query its way back to the whole context instead of
/// assuming the world holds exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderContext {
    /// The [`wgpu::Device`] the frame is drawn with, as a resource entity.
    pub device: Entity,
    /// The [`wgpu::Queue`] the frame is submitted to, as a resource entity.
    pub queue: Entity,
    /// The [`ResourceGraph`] holding the frame's resources, as a resource
    /// entity.
    pub graph: Entity,
    /// What the device can do beyond the WebGPU baseline, as a resource
    /// entity.
    ///
    /// The device cannot report this itself — it is a property of the adapter,
    /// which is normally gone by the time frames are drawn — so it is captured
    /// once and carried here. See
    /// [`DeviceCapabilities`].
    pub capabilities: Entity,
}

impl RenderContext {
    /// Maintain the frame's [`ResourceGraph`] when the returned scope drops.
    ///
    /// Call this at the top of [`FrameSource::build_scene`] and drop the scope
    /// before reading a resource: every return in between — including the early
    /// ones a source takes when it has nothing to draw — settles the graph, so
    /// no path has to remember to. The pass is the same one
    /// [`ResourceGraph::maintain`] makes; the scope only decides when it runs.
    ///
    /// The scope borrows neither the graph nor the source: it holds the world
    /// and this context's graph address, and fetches the graph when it ends, so
    /// the code between taking the scope and dropping it is free to borrow the
    /// graph itself, as a build does.
    pub fn maintain_scope<'w>(self, world: &'w World) -> MaintainScope<'w> {
        MaintainScope { world, ctx: self }
    }
}

/// A [`RenderContext`] as plain data.
///
/// The context's four resource entities, for a reader — a debug overlay, an MCP
/// server — that reports the GPU state without borrowing the world. The handles
/// round-trip through [`Entity::to_bits`] and [`Entity::from_bits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct RenderContextInfo {
    /// The entity carrying the [`wgpu::Device`].
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = unlit_ecs::EntityProxy))]
    pub device: Entity,
    /// The entity carrying the [`wgpu::Queue`].
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = unlit_ecs::EntityProxy))]
    pub queue: Entity,
    /// The entity carrying the [`ResourceGraph`].
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = unlit_ecs::EntityProxy))]
    pub graph: Entity,
    /// The entity carrying the [`DeviceCapabilities`].
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = unlit_ecs::EntityProxy))]
    pub capabilities: Entity,
}

impl RenderContext {
    /// The context the world holds, found from any one of its addresses.
    ///
    /// The four resource entities and the graph entity all carry the whole
    /// [`RenderContext`], so a caller that was handed a device, queue, graph
    /// or capabilities address gets the rest without assuming the world holds
    /// exactly one context. `None` when `anchor` carries no context.
    #[must_use]
    pub fn of(world: &World, anchor: Entity) -> Option<Self> {
        world.get::<RenderContext>(anchor).map(|context| *context)
    }

    /// The context's resource addresses as plain data.
    pub fn info(self) -> RenderContextInfo {
        RenderContextInfo {
            device: self.device,
            queue: self.queue,
            graph: self.graph,
            capabilities: self.capabilities,
        }
    }
}

/// Maintains the frame's resource graph when it ends.
///
/// [`RenderContext::maintain_scope`] returns one. It is a scope guard rather
/// than a call so that a frame source cannot forget the pass on a return: the
/// guard settles the graph on every way out of the scope it lives in, and a
/// build that has to read the graph before the end — assembling a frame from
/// the bind groups the pass just rebuilt — drops it explicitly at that point.
#[must_use = "the graph is maintained when the scope is dropped"]
pub struct MaintainScope<'w> {
    world: &'w World,
    ctx: RenderContext,
}

impl Drop for MaintainScope<'_> {
    fn drop(&mut self) {
        // A missing graph is a caller error, and panicking here would be a
        // second panic on the way out of one, so the scope does nothing.
        if let Some(mut graph) = self.world.get_mut::<ResourceGraph>(self.ctx.graph) {
            graph.maintain();
        }
    }
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

/// The region of the frame's target that scene-like sources draw into, or
/// `None` for the whole of it.
///
/// This is how a frame loop letterboxes content whose own aspect differs from
/// its target's: it states the rectangle once per frame, and every source that
/// draws 3D content records into it — so the content is scaled uniformly and
/// the same picture appears on any target shape. A source that composes over
/// the content rather than being part of it, a UI overlay for instance, ignores
/// this and covers the whole target.
///
/// It is a per-frame value rather than a field on [`RenderContext`] for the
/// same reason [`FrameTarget`] is: the context is spawned once and stays `Copy`,
/// while this changes with every resize.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameViewport(pub ViewportRect);

/// The world resource holding the frame's current [`FrameViewport`].
///
/// Internal: the frame loop writes it through [`set_frame_viewport`] and
/// sources read it through [`frame_viewport`].
#[derive(Clone, Copy, Debug, Default)]
struct FrameViewportSlot(Option<FrameViewport>);

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

/// A URL a frame source asked the host to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenUrl {
    /// The URL to open.
    pub url: String,
    /// Whether to open it in a new tab rather than the current one. Only a
    /// browser draws that distinction; anywhere else a URL opens however that
    /// platform opens one.
    pub new_tab: bool,
}

/// The URLs the frame's sources asked to open, queued for the host.
///
/// A source cannot open a browser tab itself: it builds the frame while the
/// world is borrowed, and has no host to ask. It queues the request here
/// instead, and the host drains it after the frame and opens each URL where it
/// has the means to — see [`drain_open_urls`]. The frame context spawns it, so
/// a world that draws a frame has one.
#[derive(Debug, Default)]
pub struct OpenUrls(Vec<OpenUrl>);

impl OpenUrls {
    /// Queue a URL to open.
    pub fn push(&mut self, url: impl Into<String>, new_tab: bool) {
        self.0.push(OpenUrl {
            url: url.into(),
            new_tab,
        });
    }

    /// Replace everything queued with `urls`.
    ///
    /// A source that publishes the requests of a frame in one go uses this
    /// rather than pushing, so a frame's requests never mix with a stale
    /// queue.
    pub fn replace(&mut self, urls: impl IntoIterator<Item = OpenUrl>) {
        self.0.clear();
        self.0.extend(urls);
    }
}

/// Take the URLs the frame's sources asked the host to open.
///
/// The host calls this after the frame. Nothing is intercepted automatically,
/// so a caller decides what opening a URL means where it runs: a browser tab
/// on the web, and nothing at all where there is no browser. A world without a
/// queue — one built without [`spawn_context`] — yields none.
pub fn drain_open_urls(world: &World) -> Vec<OpenUrl> {
    let Some(entity) = world.query::<&OpenUrls>().next().map(|(e, _)| e) else {
        return Vec::new();
    };
    world
        .with_mut::<OpenUrls, _>(entity, |urls| std::mem::take(&mut urls.0))
        .unwrap_or_default()
}

/// The frame's target, or `None` before any frame loop wrote one.
///
/// A source calls this in
/// [`build_scene`](FrameSource::build_scene). `None` means the frame loop never
/// stated what the frame draws into, which is a programming error on its part —
/// a source cannot invent a target — so a source that needs it should refuse to
/// draw rather than fall back.
pub fn frame_target(world: &World) -> Option<FrameTarget> {
    world
        .query::<&FrameTargetSlot>()
        .next()
        .and_then(|(_, slot)| slot.0)
}

/// State the target the frame draws into.
///
/// The frame loop calls this before it renders — [`Renderer::set_render_target`](crate::renderer::Renderer::set_render_target)
/// does it for the built-in path. Returns whether a slot existed to write; a
/// world only gets one from [`spawn_context`].
pub fn set_frame_target(world: &World, target: FrameTarget) -> bool {
    let Some(entity) = world.query::<&FrameTargetSlot>().next().map(|(e, _)| e) else {
        return false;
    };
    world
        .with_mut::<FrameTargetSlot, _>(entity, |slot| slot.0 = Some(target))
        .is_some()
}

/// Unset the target the frame draws into.
///
/// The inverse of [`set_frame_target`]: sources then read no target and refuse
/// to draw instead of drawing into one that is gone. Returns whether a slot
/// existed to write; a world only gets one from [`spawn_context`].
///
/// Internal: this is how [`Renderer::unset_render_target`](crate::renderer::Renderer::unset_render_target)
/// stops the world claiming a target whose attachments it released, so a
/// caller states a target again by setting one rather than by unsetting one.
pub(crate) fn unset_frame_target(world: &World) -> bool {
    let Some(entity) = world.query::<&FrameTargetSlot>().next().map(|(e, _)| e) else {
        return false;
    };
    world
        .with_mut::<FrameTargetSlot, _>(entity, |slot| slot.0 = None)
        .is_some()
}

/// The region of the frame's target that 3D sources draw into.
///
/// A source calls this in [`FrameSource::build_scene`] to learn where the
/// content it is part of belongs, and passes it to
/// [`Scene::set_viewport`] so its draws land inside it.
///
/// `None` means the frame loop stated no letterbox, so the whole target is the
/// region — the ordinary case, and the one every caller that never letterboxes
/// leaves the world in.
pub fn frame_viewport(world: &World) -> Option<FrameViewport> {
    world
        .query::<&FrameViewportSlot>()
        .next()
        .and_then(|(_, slot)| slot.0)
}

/// State the region of the frame's target that 3D sources draw into.
///
/// The frame loop calls this before it renders, next to
/// [`set_frame_target`](crate::renderer::Renderer::set_render_target). A frame
/// loop that never calls it leaves the whole target as the region, which is
/// what every caller that does not letterbox wants.
///
/// A letterbox is a property of the target, not of a source, so it is stated
/// here rather than by each source: see [`FrameViewport`]. Returns whether a
/// slot existed to write; a world only gets one from [`spawn_context`].
pub fn set_frame_viewport(world: &World, viewport: Option<FrameViewport>) -> bool {
    let Some(entity) = world.query::<&FrameViewportSlot>().next().map(|(e, _)| e) else {
        return false;
    };
    world
        .with_mut::<FrameViewportSlot, _>(entity, |slot| slot.0 = viewport)
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
        world: &World,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    );

    /// The scene built by the last [`FrameSource::build_scene`].
    fn scene(&self) -> &Scene;

    /// Where this source records, relative to the others.
    ///
    /// Required, not defaulted: a source that stayed silent would be ordered
    /// by entity alone, which is the implicit behaviour this exists to
    /// replace. Use [`FrameSourceOrder::MESH`], [`FrameSourceOrder::UI`], or a value
    /// of your own.
    ///
    /// A [`Source`] may override this per entity with [`Source::with_order`],
    /// which is how a caller reorders an existing source without rebuilding it.
    fn order(&self) -> FrameSourceOrder;
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
    order: Option<FrameSourceOrder>,
}

impl Source {
    /// Wrap `source`, taking its order from [`FrameSource::order`].
    pub fn new(source: impl FrameSource) -> Self {
        Self {
            source: Box::new(source),
            order: None,
        }
    }

    /// Wrap `source`, recording it at `order` regardless of what
    /// [`FrameSource::order`] says.
    pub fn with_order(mut self, order: FrameSourceOrder) -> Self {
        self.order = Some(order);
        self
    }

    /// The order this source records at, explicit or declared.
    pub fn order(&self) -> FrameSourceOrder {
        self.order.unwrap_or_else(|| self.source.order())
    }

    /// Override the order this source records at.
    ///
    /// Setting the same value again is a no-op, so a caller may apply an order
    /// every frame without disturbing the ambiguity bookkeeping.
    pub fn set_order(&mut self, order: Option<FrameSourceOrder>) {
        self.order = order;
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
        world: &World,
        ctx: RenderContext,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.source.build_scene(world, ctx, encoder);
    }

    /// The scene this source built for the frame.
    pub fn scene(&self) -> &Scene {
        self.source.scene()
    }
}

impl core::fmt::Debug for Source {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Source")
            .field("order", &self.order())
            .finish_non_exhaustive()
    }
}

/// Spawn the frame's GPU state as resources and return its addresses.
///
/// The device and queue are `Clone` handles on a shared context, so the world
/// holds its own reference to each; the graph is not `Sync` and moves into the
/// world whole. Call this once per world.
pub fn spawn_context(
    world: &mut World,
    device: wgpu::Device,
    queue: wgpu::Queue,
    graph_component: ResourceGraph,
    capabilities: DeviceCapabilities,
) -> RenderContext {
    // The graph entity is reserved first because the context names it, and
    // every one of the four entities carries the whole context: a reader handed
    // any single address can query the rest back instead of assuming the world
    // holds exactly one context.
    let graph = world.reserve_entity();
    let context = RenderContext {
        device: world.reserve_entity(),
        queue: world.reserve_entity(),
        graph,
        capabilities: world.reserve_entity(),
    };
    world.spawn_at(context.device, (device, context));
    world.spawn_at(context.queue, (queue, context));
    world.spawn_at(context.capabilities, (capabilities, context));
    world.spawn_at(graph, (graph_component, context));
    // The frame loop writes the target here every frame; until it does, no
    // source may draw.
    world.spawn((FrameTargetSlot::default(),));
    // Where in that target the frame's 3D content belongs; the whole of it
    // until a frame loop states a letterbox.
    world.spawn((FrameViewportSlot::default(),));
    // What the frame's sources claimed of its input, written by whichever
    // source consumes input and read by the caller's own logic.
    world.spawn((InputCapture::default(),));
    // The URLs the frame's sources asked to open, drained by the host after
    // the frame.
    world.spawn((OpenUrls::default(),));
    context
}

/// One group of sources that declared the same [`FrameSourceOrder`].
pub type AmbiguousGroup = (FrameSourceOrder, Vec<Entity>);

/// The frame's sources in record order, and the ambiguous groups among them.
///
/// Resolved once per frame, so it is kept as a reusable buffer rather than
/// handed back as fresh `Vec`s: a steady scene then resolves its order without
/// allocating. [`Self::resolve`] clears and refills every buffer in place, so
/// the capacity survives from one frame to the next.
#[derive(Debug, Default)]
pub struct SourceOrder {
    /// `(order, entity)` for every source, sorted by both. Scratch: the
    /// entities are copied into [`Self::order`] once the run boundaries have
    /// been found.
    keys: Vec<(FrameSourceOrder, Entity)>,
    /// The sources to record, in the order they record.
    order: Vec<Entity>,
    /// The runs of equal orders among them, in order.
    ambiguous: Vec<AmbiguousGroup>,
}

impl SourceOrder {
    /// Resolve `world`'s sources, reusing whatever this already holds.
    ///
    /// Sorted by `(order, entity)`, so the entity breaks a tie between equal
    /// orders; every run of more than one equal order is an ambiguity.
    pub fn resolve(&mut self, world: &World) {
        self.keys.clear();
        self.keys.extend(
            world
                .query::<&Source>()
                .map(|(entity, source)| (source.order(), entity)),
        );
        self.keys
            .sort_unstable_by_key(|(order, entity)| (*order, *entity));

        self.order.clear();
        self.ambiguous.clear();
        let mut start = 0;
        while start < self.keys.len() {
            let order = self.keys[start].0;
            let mut end = start + 1;
            while end < self.keys.len() && self.keys[end].0 == order {
                end += 1;
            }
            if end - start > 1 {
                // A run of equal orders: an ambiguity. Its entities are in
                // entity order already, because that is what the sort used as
                // the tie-break.
                let group = self.keys[start..end]
                    .iter()
                    .map(|(_, entity)| *entity)
                    .collect();
                self.ambiguous.push((order, group));
            }
            start = end;
        }
        self.order
            .extend(self.keys.iter().map(|(_, entity)| *entity));
    }

    /// The sources to record, in the order they record.
    pub fn entities(&self) -> &[Entity] {
        &self.order
    }

    /// The groups of sources that declared the same [`FrameSourceOrder`], in order.
    ///
    /// Equal orders are not an error — two sources that really are
    /// interchangeable may share one — but they are worth reporting; see
    /// [`OrderWarnings`].
    pub fn ambiguous(&self) -> &[AmbiguousGroup] {
        &self.ambiguous
    }
}

/// Reports sources that declared the same [`FrameSourceOrder`], without repeating
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
                "{} sources declared the same FrameSourceOrder({}): entities {:?}; \
                 recording them in an unspecified order — give them distinct orders \
                 to choose explicitly",
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
    /// exercise the ordering bookkeeping, which never touches the GPU.
    fn test_context(world: &mut World) -> RenderContext {
        let (device, queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
        spawn_context(
            world,
            device,
            queue,
            ResourceGraph::new(),
            DeviceCapabilities::default(),
        )
    }

    /// A source that records how many times it was built and what it saw.
    struct RecordingSource {
        order: FrameSourceOrder,
        built: Rc<Cell<u32>>,
        scene: Scene,
    }

    impl RecordingSource {
        fn new(order: FrameSourceOrder) -> Self {
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
            _world: &World,
            _ctx: RenderContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            self.built.set(self.built.get() + 1);
        }

        fn scene(&self) -> &Scene {
            &self.scene
        }

        fn order(&self) -> FrameSourceOrder {
            self.order
        }
    }

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// Resolve `world`'s order in one shot, as the tests want it: the entities
    /// and the ambiguous groups, both owned so an assertion can hold them while
    /// the world changes.
    fn record_order(world: &World) -> (Vec<Entity>, Vec<AmbiguousGroup>) {
        let mut resolved = SourceOrder::default();
        resolved.resolve(world);
        (resolved.entities().to_vec(), resolved.ambiguous().to_vec())
    }

    /// Sources record in the order they declare, not the order they were
    /// spawned in — the whole point of an explicit order.
    #[test]
    fn explicit_order_wins_over_spawn_order() {
        let mut world = World::new();
        spawn_context(
            &mut world,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
            ResourceGraph::new(),
            DeviceCapabilities::default(),
        );

        // Spawned overlay-first, so spawn order is the opposite of draw order.
        let overlay = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));
        let mesh = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));

        let (order, ambiguous) = record_order(&world);
        assert_eq!(order, vec![mesh, overlay], "the lower order records first");
        assert!(ambiguous.is_empty(), "distinct orders are not ambiguous");
    }

    /// Equal orders fall back to entity order, which keeps them stable while
    /// the entities live.
    #[test]
    fn equal_orders_fall_back_to_entity_order() {
        let mut world = World::new();
        test_context(&mut world);

        let first = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));
        let second = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));
        let third = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));
        assert_eq!(record_order(&world).0, vec![first, second, third]);

        // An explicit order is the way to choose: the entity is only a
        // tie-break, and it does not survive a respawn reusing the index.
        world.despawn(second);
        assert_eq!(
            record_order(&world).0,
            vec![first, third],
            "the surviving sources keep their relative order"
        );
    }

    /// A source spawned at an explicit order ignores what its own `order`
    /// says, and `set_order` can change it later.
    #[test]
    fn an_explicit_order_overrides_the_one_the_source_declares() {
        let mut world = World::new();
        test_context(&mut world);

        let a = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH))
            .with_order(FrameSourceOrder::UI),));
        let b = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));

        // `a` was spawned as an overlay, so it records last despite declaring
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
                _world: &World,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
            }
            fn scene(&self) -> &Scene {
                &self.0
            }
            fn order(&self) -> FrameSourceOrder {
                FrameSourceOrder::MESH
            }
        }

        let mut world = World::new();
        test_context(&mut world);

        let recording = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        let other = world.spawn((Source::new(OtherSource(Scene::new())),));

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
                _world: &World,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
                self.0 += 1;
            }
            fn scene(&self) -> &Scene {
                &self.1
            }
            fn order(&self) -> FrameSourceOrder {
                FrameSourceOrder::UI
            }
        }

        let mut world = World::new();
        let ctx = test_context(&mut world);
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        world.spawn((Source::new(CountingSource(0, Scene::new())),));

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
        let mut world = World::new();
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
                _world: &World,
                _ctx: RenderContext,
                _encoder: &mut wgpu::CommandEncoder,
            ) {
                self.log.borrow_mut().push(format!("build {}", self.name));
            }
            fn scene(&self) -> &Scene {
                &self.scene
            }
            fn order(&self) -> FrameSourceOrder {
                FrameSourceOrder::MESH
            }
        }

        for name in ["a", "b"] {
            world.spawn((Source::new(PhasedSource {
                name,
                log: Rc::clone(&order),
                scene: Scene::new(),
            }),));
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

    /// A source that was never spawned at an explicit order takes the one it
    /// declares, and a fresh source reports what it declared.
    #[test]
    fn a_source_reports_its_declared_order_until_overridden() {
        let source = Source::new(RecordingSource::new(FrameSourceOrder::UI));
        assert_eq!(source.order(), FrameSourceOrder::UI);

        let mut source = Source::new(RecordingSource::new(FrameSourceOrder::MESH))
            .with_order(FrameSourceOrder::UI);
        assert_eq!(source.order(), FrameSourceOrder::UI, "the override wins");

        source.set_order(None);
        assert_eq!(source.order(), FrameSourceOrder::MESH, "and can be dropped");
    }

    /// An ambiguity is reported once, and only again when it changes: a steady
    /// frame must not flood the log.
    #[test]
    fn an_ambiguity_is_reported_once_and_again_only_when_it_changes() {
        let mut world = World::new();
        test_context(&mut world);

        let a = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        let b = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));

        let (_, ambiguous) = record_order(&world);
        assert_eq!(ambiguous.len(), 1, "the two share one order");
        assert_eq!(ambiguous[0].0, FrameSourceOrder::MESH);
        assert_eq!(ambiguous[0].1, vec![a, b], "in entity order");

        let mut warnings = OrderWarnings::default();
        assert!(warnings.check(&ambiguous), "the first frame reports it");
        assert!(!warnings.check(&ambiguous), "a steady frame stays quiet");

        // Resolving the ambiguity stops the reporting; reintroducing it warns
        // once more.
        let _ =
            world.with_mut::<Source, _>(b, |source| source.set_order(Some(FrameSourceOrder::UI)));
        let (_, resolved) = record_order(&world);
        assert!(resolved.is_empty());
        assert!(!warnings.check(&resolved));

        let _ =
            world.with_mut::<Source, _>(b, |source| source.set_order(Some(FrameSourceOrder::MESH)));
        let (_, again) = record_order(&world);
        assert!(warnings.check(&again), "the ambiguity is back");
        assert!(!warnings.check(&again), "and still reports only once");
    }

    /// Resolving the order reuses its buffers, so a steady scene stops
    /// allocating.
    ///
    /// The capacity is what proves reuse: a fresh `Vec` per frame would leave
    /// each one at its own length, while a cleared-and-refilled one keeps the
    /// largest capacity it has ever needed.
    #[test]
    fn resolving_the_order_reuses_its_buffers() {
        let mut world = World::new();
        test_context(&mut world);
        for _ in 0..4 {
            world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        }
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));

        let mut resolved = SourceOrder::default();
        resolved.resolve(&world);
        let order_capacity = resolved.order.capacity();
        let ambiguous_capacity = resolved.ambiguous.capacity();
        assert_eq!(resolved.entities().len(), 5);
        assert_eq!(resolved.ambiguous().len(), 1, "four share one order");

        // A later frame with fewer sources keeps both allocations.
        let first = world.query::<&Source>().next().expect("a source").0;
        world.despawn(first);
        resolved.resolve(&world);
        assert_eq!(resolved.entities().len(), 4);
        assert_eq!(
            resolved.order.capacity(),
            order_capacity,
            "the order buffer is reused rather than reallocated"
        );
        assert_eq!(
            resolved.ambiguous.capacity(),
            ambiguous_capacity,
            "the ambiguity buffer is reused rather than reallocated"
        );
    }

    /// Distinct orders produce no ambiguity at all.
    #[test]
    fn distinct_orders_are_never_ambiguous() {
        let mut world = World::new();
        test_context(&mut world);
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));

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
        let mut world = World::new();
        test_context(&mut world);
        let a = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));
        let b = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));

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
            message.contains(&FrameSourceOrder::UI.0.to_string()),
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
        let mut world = World::new();
        test_context(&mut world);
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::UI)),));

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
        let mut world = World::new();
        test_context(&mut world);
        let a = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        let _b = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
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
            world.with_mut::<Source, _>(a, |source| source.set_order(Some(FrameSourceOrder::UI)));
        let messages = capture_logs(|| {
            let (_, resolved) = record_order(&world);
            assert!(!warnings.check(&resolved), "resolved: nothing to say");
        });
        assert!(messages.is_empty(), "resolving is silent: {messages:?}");
    }

    /// Re-applying the order a source already has does not disturb the
    /// ambiguity bookkeeping, so a caller may set an order every frame.
    #[test]
    fn re_applying_the_same_order_keeps_the_log_quiet() {
        let mut world = World::new();
        test_context(&mut world);
        let a = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        let b = world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),));
        let mut warnings = OrderWarnings::default();

        let messages = capture_logs(|| {
            for _ in 0..3 {
                // What a caller that pins its sources' order every frame does.
                let _ = world.with_mut::<Source, _>(a, |source| {
                    source.set_order(Some(FrameSourceOrder::MESH))
                });
                let _ = world.with_mut::<Source, _>(b, |source| {
                    source.set_order(Some(FrameSourceOrder::MESH))
                });
                let (_, ambiguous) = record_order(&world);
                warnings.check(&ambiguous);
            }
        });
        assert_eq!(
            messages.len(),
            1,
            "the second and third frames change nothing: {messages:?}"
        );
    }

    /// Three sources sharing an order are one group, not three pairs, and the
    /// group lists every entity.
    #[test]
    fn a_three_way_ambiguity_is_one_group_of_three() {
        let mut world = World::new();
        test_context(&mut world);

        let ids: Vec<Entity> = (0..3)
            .map(|_| world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),)))
            .collect();

        let (_, ambiguous) = record_order(&world);
        assert_eq!(ambiguous.len(), 1, "one group, not pairs");
        assert_eq!(ambiguous[0].1, ids);
    }

    /// Sources spawned later break a tie after ones spawned earlier, because
    /// the entity index is allocated in spawn order.
    #[test]
    fn later_entities_break_a_tie_last() {
        let mut world = World::new();
        test_context(&mut world);

        let ids: Vec<Entity> = (0..3)
            .map(|_| world.spawn((Source::new(RecordingSource::new(FrameSourceOrder::MESH)),)))
            .collect();
        assert_eq!(record_order(&world).0, ids);
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    use unlit_wgpu::resources::{ResHandle, Resource as GraphResource};

    /// A source that registers a node, so dropping it has something to give up.
    ///
    /// It holds the id in its own state, as a real source does: the node stays
    /// alive for as long as the source component does.
    struct OwningSource {
        context: RenderContext,
        node: Option<ResHandle>,
        scene: Scene,
    }

    impl FrameSource for OwningSource {
        fn build_scene(
            &mut self,
            world: &World,
            ctx: RenderContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            // Register once, on the first frame: a node the source owns and
            // nothing else refers to.
            self.context = ctx;
            if self.node.is_none() {
                self.node = Some(
                    world
                        .get_mut::<ResourceGraph>(ctx.graph)
                        .expect("the context's graph exists")
                        .insert(GraphResource::Virtual, None),
                );
            }
        }

        fn scene(&self) -> &Scene {
            &self.scene
        }

        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::UI
        }
    }

    /// A source whose build takes a maintain scope and then returns at once,
    /// the way a source with nothing to draw does.
    struct EarlyReturnSource {
        context: RenderContext,
        node: Option<ResHandle>,
        scene: Scene,
    }

    impl FrameSource for EarlyReturnSource {
        fn build_scene(
            &mut self,
            world: &World,
            ctx: RenderContext,
            _encoder: &mut wgpu::CommandEncoder,
        ) {
            // The scope is the whole of the build's graph handling: this source
            // never calls `maintain`, and the return is what settles the graph.
            let _maintain = ctx.maintain_scope(world);
            self.context = ctx;
            if self.node.is_none() {
                self.node = Some(
                    world
                        .get_mut::<ResourceGraph>(ctx.graph)
                        .expect("the context's graph exists")
                        .insert(GraphResource::Virtual, None),
                );
            } else {
                // A source with nothing left to draw: it gives its node up and
                // returns, leaving the scope's drop to collect it.
                self.node = None;
            }
        }

        fn scene(&self) -> &Scene {
            &self.scene
        }

        fn order(&self) -> FrameSourceOrder {
            FrameSourceOrder::MESH
        }
    }

    fn noop_context(world: &mut World) -> RenderContext {
        spawn_context(
            world,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).0,
            wgpu::Device::noop(&wgpu::DeviceDescriptor::default()).1,
            ResourceGraph::new(),
            DeviceCapabilities::default(),
        )
    }

    /// Build the source once, so it registers its node.
    ///
    /// The node's id is deliberately not handed back: an id is a strong
    /// reference, so a test holding one would keep the very node it wants to
    /// watch the graph collect.
    fn build_once(world: &mut World, entity: Entity, ctx: RenderContext) {
        let mut encoder = wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
            .0
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        world
            .with_mut::<Source, _>(entity, |source| {
                source.build_scene(world, ctx, &mut encoder)
            })
            .expect("the source entity exists");
    }

    /// How many nodes the context's graph holds.
    fn graph_len(world: &World, ctx: RenderContext) -> usize {
        world.get_mut::<ResourceGraph>(ctx.graph).unwrap().len()
    }

    /// Drive the graph's collection the way a frame would.
    fn maintain(world: &mut World, ctx: RenderContext) {
        world
            .get_mut::<ResourceGraph>(ctx.graph)
            .unwrap()
            .maintain();
    }

    /// Despawning a source drops the ids it holds, and the next `maintain`
    /// collects the nodes they were the last references to.
    ///
    /// `despawn` drops the component, so nothing has to tell the graph that an
    /// entity is gone — the reference counts carry that on their own.
    #[test]
    fn dropping_a_source_releases_the_nodes_it_owns() {
        let mut world = World::new();
        let ctx = noop_context(&mut world);
        let entity = world.spawn((Source::new(OwningSource {
            context: ctx,
            node: None,
            scene: Scene::new(),
        }),));
        build_once(&mut world, entity, ctx);
        assert_eq!(
            graph_len(&world, ctx),
            1,
            "the source registered exactly the node it owns"
        );

        world.despawn(entity);

        // Giving up the id only moves a count: the node is still there, and
        // the graph resolves it until something maintains.
        assert_eq!(
            graph_len(&world, ctx),
            1,
            "dropping an id does not itself remove the node"
        );

        maintain(&mut world, ctx);
        assert_eq!(
            graph_len(&world, ctx),
            0,
            "maintain collected the node nothing refers to"
        );
    }

    /// A source that owns nothing still despawns, so dropping a source is safe
    /// for every source.
    #[test]
    fn removing_a_source_that_owns_nothing_still_removes_it() {
        let mut world = World::new();
        let ctx = noop_context(&mut world);
        let entity = world.spawn((Source::new(OwningSource {
            context: ctx,
            node: None,
            scene: Scene::new(),
        }),));

        world.despawn(entity);
        assert!(!world.contains(entity));
        maintain(&mut world, ctx);
        assert_eq!(graph_len(&world, ctx), 0);
    }

    /// Two sources leaving in the same frame give up their ids, and the frame's
    /// single `maintain` is what drops both.
    ///
    /// Neither removal touches the graph: each is a reference count reaching
    /// zero, and one pass collects whatever the frame left unreferenced.
    #[test]
    fn one_maintain_drops_the_nodes_every_source_gave_up() {
        let mut world = World::new();
        let ctx = noop_context(&mut world);
        let mut entities = Vec::new();
        for _ in 0..2 {
            entities.push(world.spawn((Source::new(OwningSource {
                context: ctx,
                node: None,
                scene: Scene::new(),
            }),)));
        }

        for &entity in &entities {
            build_once(&mut world, entity, ctx);
        }
        assert_eq!(graph_len(&world, ctx), 2, "each source registered a node");

        for &entity in &entities {
            world.despawn(entity);
        }

        // Both removals only moved counts: the nodes the two sources owned are
        // still there until the frame maintains.
        assert_eq!(
            graph_len(&world, ctx),
            2,
            "dropping an id does not itself remove the node"
        );

        // The frame's one pass drops everything the two of them gave up.
        maintain(&mut world, ctx);
        assert_eq!(
            graph_len(&world, ctx),
            0,
            "the single maintain collected every unreferenced node"
        );
    }

    /// A build that returns early still settles the graph: the scope a source
    /// takes at its top maintains on the way out, so no return has to remember
    /// the call.
    #[test]
    fn a_build_that_returns_early_still_maintains() {
        let mut world = World::new();
        let ctx = noop_context(&mut world);
        let entity = world.spawn((Source::new(EarlyReturnSource {
            context: ctx,
            node: None,
            scene: Scene::new(),
        }),));

        // The first build registers the node the source owns.
        build_once(&mut world, entity, ctx);
        assert_eq!(graph_len(&world, ctx), 1, "the source registered a node");

        // The second build gives it up and returns at once. Nothing calls
        // `maintain`: the scope's drop is the pass, and it collects the node
        // the build left unreferenced.
        build_once(&mut world, entity, ctx);
        assert_eq!(
            graph_len(&world, ctx),
            0,
            "the early return maintained the graph"
        );
    }
}
