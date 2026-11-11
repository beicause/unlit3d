//! Frame sources: the things a frame is made of.
//!
//! A [`Renderer`](crate::renderer::Renderer) draws nothing itself. It assembles a frame
//! out of [`FramePass`](crate::pass::FramePass)es, and each pass out of the
//! [`FrameSource`]s below it: every source builds its own [`Scene`] from the
//! world, and the pass records it. A source's place in the entity tree is
//! therefore what chooses its pass. The built-in mesh rendering is one such
//! source ([`MeshSource`](crate::mesh_source::MeshSource)) and a UI overlay is
//! another; a caller's own pass over the frame is a third, with no more
//! privilege than the built-in ones.
//!
//! A source is an ordinary component holding a boxed [`AnySource`], so one
//! query drives every concrete source type: build all of them, then record
//! their scenes in [`FrameSourceOrder`]. The GPU state a source needs — the device,
//! the queue and the resource graph — lives in the world too, addressed by the
//! [`RenderContext`] the sources are handed; its accessors are what let a source
//! fetch the graph while the driver only holds a shared borrow of the world. What
//! the pass draws into, and where in it the source belongs, reaches the source
//! as [`PassInfo`].
//!
//! [`Scene`]: unlit_wgpu::scene::Scene

use core::any::Any;

use unlit_ecs::bundle;
use unlit_ecs::{
    Bundle, DynamicBundle, Entity, EntityAccessError, QueryOne, Scope, ScopeMut, World,
};
use unlit_wgpu::capabilities::DeviceCapabilities;
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::scene::Scene;

use crate::frame::FrameClock;
use crate::global_uniforms::GlobalUniforms;
use crate::order::OrderKey;
use crate::pass::PassInfo;

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
    /// The order of lines drawn over the meshes.
    ///
    /// A line tests against the depth the meshes wrote, so it belongs after
    /// them in the same pass: recording it with the meshes would let a mesh
    /// drawn later paint over a line in front of it. Whether a stroke also
    /// writes depth is its own [`LineOptions`](unlit_wgpu::line_shader::LineOptions)'s
    /// word: one that writes draws in the sequenced tier of the pass's
    /// composite, and one that leaves the depth alone joins the depth-sorted
    /// pool, sorting against every source's blended draws. See
    /// [`DrawOrder`](unlit_wgpu::scene::DrawOrder).
    pub const LINE: Self = Self(50);
    /// A source that composes over the meshes, such as the UI.
    pub const UI: Self = Self(100);
}

impl OrderKey for FrameSourceOrder {
    const ORDER: &'static str = "FrameSourceOrder";
    const SUBJECT: &'static str = "sources";

    fn value(self) -> i32 {
        self.0
    }
}

/// The GPU state a frame is drawn with, as world addresses.
///
/// It holds [`Entity`] handles rather than borrows, so it is `Copy` and never
/// conflicts with a source's own borrow of the world. The handles stay inside
/// the crate: callers reach the resources through the accessors below, such as
/// [`RenderContext::graph`], [`RenderContext::device`] and
/// [`RenderContext::uniforms`], which name the resource type for them.
///
/// The handles are spawned once by [`spawn_context`] and stay valid for the
/// world's life; a resource entity is never despawned while frames are drawn.
///
/// The context is itself a component: [`spawn_context`] stores a copy on the
/// entity carrying the [`ResourceGraph`], so a reader that was handed one of
/// the addresses can query its way back to the whole context instead of
/// assuming the world holds exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderContext {
    /// The [`wgpu::Device`] the frame is drawn with, as a resource entity.
    pub(crate) device: Entity,
    /// The [`wgpu::Queue`] the frame is submitted to, as a resource entity.
    pub(crate) queue: Entity,
    /// The [`ResourceGraph`] holding the frame's resources, as a resource
    /// entity.
    pub(crate) graph: Entity,
    /// What the device can do beyond the WebGPU baseline, as a resource
    /// entity.
    ///
    /// The device cannot report this itself — it is a property of the adapter,
    /// which is normally gone by the time frames are drawn — so it is captured
    /// once and carried here. See
    /// [`DeviceCapabilities`].
    pub(crate) capabilities: Entity,
    /// The frame's [`FrameClock`], as a resource entity.
    ///
    /// The clock belongs to the world rather than to each source: a source may
    /// be built once per pass, and a clock every source counted for itself
    /// would run faster on a frame with more passes.
    pub(crate) clock: Entity,
    /// The frame's [`GlobalUniforms`], as a resource entity.
    ///
    /// The camera, globals and view records every draw binds belong to the
    /// frame: two sources drawn through one camera read the same bytes, and a
    /// source built once per pass reads one record per pass. Keeping them here
    /// rather than in each source is what makes that one upload instead of
    /// one per build.
    pub(crate) uniforms: Entity,
}

impl RenderContext {
    /// The frame's [`wgpu::Device`], cloned out of `scope`.
    ///
    /// # Errors
    ///
    /// If the context's device resource is gone.
    pub fn device(self, scope: &Scope<'_>) -> Result<wgpu::Device, EntityAccessError> {
        scope
            .query_one::<&wgpu::Device>(self.device)
            .map(|mut row| row.get().clone())
    }

    /// The frame's [`wgpu::Queue`], cloned out of `scope`.
    ///
    /// # Errors
    ///
    /// If the context's queue resource is gone.
    pub fn queue(self, scope: &Scope<'_>) -> Result<wgpu::Queue, EntityAccessError> {
        scope
            .query_one::<&wgpu::Queue>(self.queue)
            .map(|mut row| row.get().clone())
    }

    /// What the device can do beyond the WebGPU baseline.
    ///
    /// # Errors
    ///
    /// If the context's capabilities resource is gone.
    pub fn capabilities(self, scope: &Scope<'_>) -> Result<DeviceCapabilities, EntityAccessError> {
        scope
            .query_one::<&DeviceCapabilities>(self.capabilities)
            .map(|mut row| *row.get())
    }

    /// The frame's [`FrameClock`].
    ///
    /// # Errors
    ///
    /// If the context's clock resource is gone.
    pub fn clock(self, scope: &Scope<'_>) -> Result<FrameClock, EntityAccessError> {
        scope
            .query_one::<&FrameClock>(self.clock)
            .map(|mut row| *row.get())
    }

    /// The frame's [`ResourceGraph`], borrowed from `scope`.
    ///
    /// # Errors
    ///
    /// If the context's graph resource is gone.
    pub fn try_graph<'w>(
        self,
        scope: &Scope<'w>,
    ) -> Result<QueryOne<'w, &'w mut ResourceGraph>, EntityAccessError> {
        scope.query_one::<&mut ResourceGraph>(self.graph)
    }

    /// The frame's [`ResourceGraph`], borrowed from `scope`.
    ///
    /// A source builds the frame's resources through it: the graph is a
    /// component like any other, so this is the query every build makes
    /// before it starts.
    ///
    /// # Panics
    ///
    /// If the context's graph resource is gone.
    pub fn graph<'w>(self, scope: &Scope<'w>) -> QueryOne<'w, &'w mut ResourceGraph> {
        self.try_graph(scope)
            .expect("the context's resource graph exists")
    }

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
    pub fn maintain_scope<'w>(self, scope: Scope<'w>) -> MaintainScope<'w> {
        MaintainScope { scope, ctx: self }
    }

    /// The frame's [`GlobalUniforms`], borrowed from `scope`.
    ///
    /// A source asks it for the camera and view records of the pass it is
    /// building, and the group it binds them through comes from the same
    /// place. The renderer advances the frame globals before any source
    /// builds, so the record is already the frame's when a source reads it.
    ///
    /// # Panics
    ///
    /// If the context's uniforms resource is gone.
    pub fn uniforms<'w>(self, scope: &Scope<'w>) -> QueryOne<'w, &'w mut GlobalUniforms> {
        scope
            .query_one::<&mut GlobalUniforms>(self.uniforms)
            .expect("the context's global uniforms exist")
    }

    /// The context the world holds, found from any one of its addresses.
    ///
    /// The resource entities all carry the whole [`RenderContext`], so a
    /// caller that was handed a device, queue, graph, capabilities or clock
    /// address gets the rest without assuming the world holds exactly one
    /// context. [`EntityAccessError`] when `anchor` carries no context, which
    /// also covers an entity that is not alive or not visible from the root.
    pub fn of(world: &World, anchor: Entity) -> Result<Self, EntityAccessError> {
        world
            .root_scope()
            .query_one::<&RenderContext>(anchor)
            .map(|mut context| {
                let context = context.get();
                *context
            })
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
    scope: Scope<'w>,
    ctx: RenderContext,
}

impl Drop for MaintainScope<'_> {
    fn drop(&mut self) {
        // A missing graph is a caller error, and panicking here would be a
        // second panic on the way out of one, so the scope does nothing.
        if let Ok(mut graph) = self.scope.query_one::<&mut ResourceGraph>(self.ctx.graph) {
            graph.get().maintain();
        }
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
    // The scan claims every `OpenUrls` row and the write below claims one of
    // them exclusively, so the scan's borrow has to end before it: the claim
    // lasts as long as the row holder, not as long as the loop that read it.
    let scanned = {
        let mut rows = world.root_scope().query::<(Entity, &OpenUrls)>();
        rows.iter().next().map(|(entity, _)| entity)
    };
    let Some(entity) = scanned else {
        return Vec::new();
    };
    world
        .root_scope()
        .query_one::<&mut OpenUrls>(entity)
        .map(|mut urls| {
            let urls = urls.get();
            std::mem::take(&mut urls.0)
        })
        .unwrap_or_default()
}

/// A source of one frame's draws.
///
/// A source owns whatever state it draws from, and fills a [`Scene`] with the
/// draws of one frame each time the driver calls
/// [`FrameSource::build_scene`]. The scene is the driver's, not the source's:
/// a source in the scope of several passes is built once per pass, each time
/// into that pass's own scene.
pub trait FrameSource: 'static {
    /// Fill `scene` with this frame's draws, fetching what it needs from
    /// `world`.
    ///
    /// `info` states the pass the source is recorded by: what its target looks
    /// like, how large it is, and the region the pass letterboxes to. It is the
    /// only way a source learns where it draws, because the pass is an ancestor
    /// entity it cannot reach from its own scope.
    ///
    /// `scene` arrives empty and belongs to the pass for this frame: write the
    /// draws of what `info` describes into it, and the driver records it into
    /// that pass. A source the scope of several passes reaches is called once
    /// per pass, so per-pass state must be keyed by [`PassInfo::surface`]
    /// rather than kept in one field.
    ///
    /// `ctx` carries only entity ids, so it never conflicts with the source's
    /// own borrow of `world`. `encoder` is the frame's one encoder, and a
    /// source stages its uploads through it: passes are opened only after every
    /// source has built, so the uploads and the draws that read them share one
    /// submission.
    ///
    /// A source that has nothing to draw returns with `scene` still empty: the
    /// frame still records, and still applies its load ops.
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    );

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

/// Why a typed access to a [`Source`] was refused.
///
/// A source is held behind an erased handle so one query drives every
/// concrete type. Asking it for a type it does not hold is a mistake the
/// caller can read back: both the type it actually holds and the one asked
/// for are named, rather than collapsed into a silent `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("the source holds {actual}, not {requested}")]
pub struct WrongSourceType {
    /// The concrete type the source was built from.
    pub actual: &'static str,
    /// The concrete type the caller asked for.
    pub requested: &'static str,
}

/// A frame source as an ECS component.
///
/// The trait API is reachable directly (`source.order()`,
/// `source.build_scene(..)`); typed access upcasts to `dyn Any` and
/// downcasts, which [`Source::as_mut`] and [`Source::as_ref`] wrap as a
/// [`Result`] carrying [`WrongSourceType`].
pub struct Source {
    source: Box<dyn AnySource>,
    /// The concrete type `source` was built from, for a readable refusal.
    type_name: &'static str,
    /// An explicit order that overrides the source's own.
    order: Option<FrameSourceOrder>,
}

impl Source {
    /// Wrap `source`, taking its order from [`FrameSource::order`].
    pub fn new<S: FrameSource>(source: S) -> Self {
        Self {
            type_name: core::any::type_name::<S>(),
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

    /// The source as its concrete type.
    ///
    /// # Errors
    ///
    /// Answers [`WrongSourceType`] when the source holds a different type,
    /// naming both the type it holds and the one asked for.
    pub fn as_mut<T: FrameSource>(&mut self) -> Result<&mut T, WrongSourceType> {
        let actual = self.type_name;
        let erased: &mut dyn Any = &mut *self.source;
        erased.downcast_mut::<T>().ok_or(WrongSourceType {
            actual,
            requested: core::any::type_name::<T>(),
        })
    }

    /// The source as its concrete type.
    ///
    /// # Errors
    ///
    /// Answers [`WrongSourceType`] when the source holds a different type,
    /// naming both the type it holds and the one asked for.
    pub fn as_ref<T: FrameSource>(&self) -> Result<&T, WrongSourceType> {
        let actual = self.type_name;
        let erased: &dyn Any = &*self.source;
        erased.downcast_ref::<T>().ok_or(WrongSourceType {
            actual,
            requested: core::any::type_name::<T>(),
        })
    }

    /// Fill `scene` with this source's draws for the frame.
    pub fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.source.build_scene(scope, ctx, info, scene, encoder);
    }
}

impl core::fmt::Debug for Source {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Source")
            .field("order", &self.order())
            .finish_non_exhaustive()
    }
}

/// One of the frame's GPU resources, together with the context that addresses
/// the rest.
///
/// Every entity the context names carries the whole context, so a reader handed
/// any single address can query the rest back instead of assuming the world
/// holds exactly one context. A resource without it would be reachable only by
/// guessing which entity holds what.
struct ContextRow<T> {
    resource: T,
    context: RenderContext,
}

impl<T: 'static> Bundle for ContextRow<T> {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.resource, self.context]
    }
}

/// Spawn one resource on the handle reserved for it, with the context that
/// addresses the rest.
fn spawn_row<T: 'static>(
    shared: &mut ScopeMut<'_>,
    entity: Entity,
    resource: T,
    context: RenderContext,
) {
    shared
        .spawn_at(entity, ContextRow { resource, context })
        .expect("the handle was just reserved");
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
    let graph = world.reserve_entity("render_graph");
    let context = RenderContext {
        device: world.reserve_entity("gpu_device"),
        queue: world.reserve_entity("gpu_queue"),
        graph,
        capabilities: world.reserve_entity("device_capabilities"),
        clock: world.reserve_entity("frame_clock"),
        uniforms: world.reserve_entity("global_uniforms"),
    };
    // The frame's uniform registry is built against the graph it registers
    // its buffers in, so the graph is taken before either is spawned.
    let mut graph_component = graph_component;
    let uniforms = GlobalUniforms::new(&device, &mut graph_component);
    // Every one of these belongs to the shared subtree: a source reads the GPU
    // state from its own scope, and no scope may remove what every other scope
    // depends on.
    let shared = world.shared();
    let mut shared_view = world
        .scope_mut(shared)
        .expect("the shared scope exists from the moment the world is built");
    spawn_row(&mut shared_view, context.device, device, context);
    spawn_row(&mut shared_view, context.queue, queue, context);
    spawn_row(
        &mut shared_view,
        context.capabilities,
        capabilities,
        context,
    );
    spawn_row(&mut shared_view, graph, graph_component, context);
    spawn_row(&mut shared_view, context.uniforms, uniforms, context);
    // The URLs the frame's sources asked to open, drained by the host after
    // the frame.
    shared_view.spawn_named("open_urls", bundle![OpenUrls::default()]);
    // How much time has passed, advanced once per frame by the frame loop. It
    // belongs to the world rather than to a source because a source may be
    // built once per pass: a clock each source counted for itself would run
    // faster on a frame with more passes.
    spawn_row(
        &mut shared_view,
        context.clock,
        FrameClock::default(),
        context,
    );
    context
}

#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod tests;
