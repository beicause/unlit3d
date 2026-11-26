//! Draw sources: the things a frame is made of.
//!
//! A [`Renderer`](crate::renderer::Renderer) draws nothing itself. It assembles a frame
//! out of [`FramePass`](crate::pass::FramePass)es, and each pass out of the
//! [`DrawSource`]s below it: every source builds its own [`Scene`] from the
//! world, and the pass records it. A source's place in the entity tree is
//! therefore what chooses its pass. The built-in mesh rendering is one such
//! source ([`MeshSource`](crate::mesh_source::MeshSource)) and an egui overlay is
//! another; a caller's own pass over the frame is a third, with no more
//! privilege than the built-in ones.
//!
//! A source is an ordinary component holding a boxed [`AnySource`], so one
//! query drives every concrete source type: build all of them, then record
//! their scenes in [`SourceOrder`]. The GPU state a source needs — the device,
//! the queue and the resource graph — lives in the world too, addressed by the
//! [`RenderContext`] the sources are handed; its accessors are what let a source
//! fetch the graph while the driver only holds a shared borrow of the world. What
//! the pass draws into, and where in it the source belongs, reaches the source
//! as [`PassInfo`].
//!
//! [`Scene`]: unlit_wgpu::scene::Scene

use core::any::Any;

use unlit_ecs::bundle;
use unlit_ecs::{Entity, EntityAccessError, QueryOne, Scope, World};
use unlit_wgpu::capabilities::DeviceCapabilities;
use unlit_wgpu::context::{DeviceHealth, RenderDevice};
use unlit_wgpu::resources::ResourceGraph;
use unlit_wgpu::scene::Scene;

use crate::frame::FrameClock;
use crate::global_uniforms::GlobalUniforms;
use crate::order::OrderKey;
use crate::pass::PassInfo;

/// Where a source records, relative to every other source.
///
/// Lower values record first. Deliberately has no `Default`: where a source
/// records relative to the others is a decision, and a silent default is
/// exactly what an explicit order exists to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceOrder(pub i32);

impl SourceOrder {
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
    /// A source that composes over the meshes, such as the overlay.
    pub const EGUI: Self = Self(100);
    /// The order of a Blitz page, which composes over the meshes but under the
    /// overlay.
    ///
    /// A page is a whole document rather than an attachment laid over one, and
    /// it covers the target, so an overlay recorded after it stays readable.
    pub const BLITZ: Self = Self(90);
}

impl OrderKey for SourceOrder {
    const ORDER: &'static str = "SourceOrder";
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
/// The context is itself a component, spawned once by [`spawn_context`] on a
/// row of its own under the [`RenderContext`] singleton key. A caller that
/// knows only the world asks the root scope for that key to reach the whole
/// context; no resource row carries a copy, so the addresses cannot disagree
/// with one another.
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
    /// Whether the device behind the frame is still able to work, as a
    /// resource entity.
    ///
    /// A lost device accepts no further work and reports nothing on its own,
    /// so the frame loop asks here before it builds: building into a device
    /// that is gone produces either nothing or a validation panic, and neither
    /// says what happened. See [`DeviceHealth`].
    pub(crate) health: Entity,
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
            .try_query_one::<&wgpu::Device>(self.device)
            .map(|mut row| row.get().clone())
    }

    /// The frame's [`wgpu::Queue`], cloned out of `scope`.
    ///
    /// # Errors
    ///
    /// If the context's queue resource is gone.
    pub fn queue(self, scope: &Scope<'_>) -> Result<wgpu::Queue, EntityAccessError> {
        scope
            .try_query_one::<&wgpu::Queue>(self.queue)
            .map(|mut row| row.get().clone())
    }

    /// What the device can do beyond the WebGPU baseline.
    ///
    /// # Errors
    ///
    /// If the context's capabilities resource is gone.
    pub fn capabilities(self, scope: &Scope<'_>) -> Result<DeviceCapabilities, EntityAccessError> {
        scope
            .try_query_one::<&DeviceCapabilities>(self.capabilities)
            .map(|mut row| *row.get())
    }

    /// Whether the device behind the frame is still able to work.
    ///
    /// The health is a handle on the state the [`GpuContext`] that made the
    /// device installed, so it answers for that device and keeps answering
    /// after the context is dropped. A world whose device was built by hand,
    /// or one made from [`DeviceHealth::unwatched`], always says it is alive.
    ///
    /// [`GpuContext`]: unlit_wgpu::context::GpuContext
    ///
    /// # Errors
    ///
    /// If the context's health resource is gone.
    pub fn health(self, scope: &Scope<'_>) -> Result<DeviceHealth, EntityAccessError> {
        scope
            .try_query_one::<&DeviceHealth>(self.health)
            .map(|mut row| row.get().clone())
    }

    /// The frame's [`FrameClock`].
    ///
    /// # Errors
    ///
    /// If the context's clock resource is gone.
    pub fn clock(self, scope: &Scope<'_>) -> Result<FrameClock, EntityAccessError> {
        scope
            .try_query_one::<&FrameClock>(self.clock)
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
        scope.try_query_one::<&mut ResourceGraph>(self.graph)
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
    /// Call this at the top of [`DrawSource::build_scene`] and drop the scope
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
        scope.query_one::<&mut GlobalUniforms>(self.uniforms)
    }

    /// The context the world holds, if it has one.
    ///
    /// [`spawn_context`] spawns the context on a row of its own under the
    /// [`RenderContext`] singleton key, so a caller that knows only the world
    /// reaches it here. `None` for a world that was never given a device.
    pub fn of(world: &World) -> Option<Self> {
        let scope = world.root_scope();
        let entity = scope.singleton::<RenderContext>()?;
        let mut context = scope.try_query_one::<&RenderContext>(entity).ok()?;
        Some(*context.get())
    }
}

/// Maintains the frame's resource graph when it ends.
///
/// [`RenderContext::maintain_scope`] returns one. It is a scope guard rather
/// than a call so that a draw source cannot forget the pass on a return: the
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
        if let Ok(mut graph) = self
            .scope
            .try_query_one::<&mut ResourceGraph>(self.ctx.graph)
        {
            graph.get().maintain();
        }
    }
}

/// A URL a draw source asked the host to open.
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
///
/// The queue is a singleton keyed by [`OpenUrls`]: one world opens one set of
/// tabs, so every producer appends to the same queue and one drain empties it.
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

    /// Move every URL `urls` holds onto the queue, leaving `urls` empty so
    /// its allocation is reused by the next frame.
    ///
    /// A source that publishes the requests of a frame in one go uses this
    /// rather than pushing one by one, and the queue grows by them instead
    /// of being emptied first: what a previous frame left behind is served
    /// by a later [ `drain_open_urls` ] rather than dropped.
    pub fn append(&mut self, urls: &mut Vec<OpenUrl>) {
        self.0.append(urls);
    }
}

/// Take the URLs the frame's sources asked the host to open.
///
/// The host calls this after the frame. Nothing is intercepted automatically,
/// so a caller decides what opening a URL means where it runs: a browser tab
/// on the web, and nothing at all where there is no browser. A world without a
/// queue — one built without [`spawn_context`] — yields none.
pub fn drain_open_urls(world: &World) -> Vec<OpenUrl> {
    let scope = world.root_scope();
    let Some(entity) = scope.singleton::<OpenUrls>() else {
        return Vec::new();
    };
    match scope.try_query_one::<&mut OpenUrls>(entity) {
        Ok(mut row) => core::mem::take(&mut row.get().0),
        Err(error) => {
            log::warn!("the open-url queue of {entity:?} is unreachable: {error}");
            Vec::new()
        }
    }
}

/// What one build of a [`SceneSource`] is handed.
///
/// Everything a build needs arrives together: the scope the source hangs under,
/// the frame's GPU state, the pass and target it draws into, and the frame's
/// encoder. A source that reads all four can take them as one value instead of
/// remembering a five-argument order, and a later field reaches every source
/// without rewriting each signature.
pub struct SceneBuild<'a> {
    /// The entity the source hangs under. What the build draws is what lies
    /// below it, and two sources under one pass see different sets.
    pub scope: Scope<'a>,
    /// The frame's GPU state: the device, the queue, the resource graph.
    pub ctx: RenderContext,
    /// The pass the build belongs to, and the target it draws into.
    pub info: PassInfo,
    /// The frame's one command encoder.
    ///
    /// Uploads and draws share its submission, because passes are opened only
    /// after every source of the frame has built. A source records its own
    /// commands into it and hands it back untouched; nothing it does with the
    /// encoder outlives the build, and the driver alone ends the encoder.
    pub encoder: &'a mut wgpu::CommandEncoder,
}

/// A source of one pass's draws, given its whole context at once.
///
/// This is the trait to implement. Every built-in source is one, and a caller's
/// own source needs nothing else: implement [`Self::build`], answer the draws of
/// one frame, and the blanket [`DrawSource`] impl makes it spawnable.
///
/// The driver calls [`Self::build`] once per pass that reaches the source, in
/// the same frame, each time into that pass's own [`Scene`]. Per-pass state is
/// therefore keyed by [`PassInfo::pass`] rather than kept in one field, and
/// per-target state by [`PassInfo::target`] — two passes may draw into one
/// target, and two targets may share a surface key. A source under two passes
/// is built twice before any pass is opened, so nothing it records may assume
/// another source's uploads landed first.
pub trait SceneSource: 'static {
    /// Fill `scene` with this frame's draws of the pass [`SceneBuild::info`]
    /// names.
    ///
    /// `scene` arrives empty. A source with nothing to draw returns leaving it
    /// empty: the frame still records, and still applies its load ops.
    fn build(&mut self, build: SceneBuild<'_>, scene: &mut Scene);
}

impl<T: SceneSource> DrawSource for T {
    fn build_scene(
        &mut self,
        scope: &Scope<'_>,
        ctx: RenderContext,
        info: PassInfo,
        scene: &mut Scene,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.build(
            SceneBuild {
                scope: *scope,
                ctx,
                info,
                encoder,
            },
            scene,
        );
    }
}

/// A source of one frame's draws.
///
/// The low-level form of [`SceneSource`]: the same build, with every part of the
/// context as its own argument. Implement this instead when a source has to
/// control what it does with the encoder beyond staging uploads, or when the
/// erased handle must be built without going through [`SceneSource`]. Everything
/// else should implement [`SceneSource`].
///
/// A source owns whatever state it draws from, and fills a [`Scene`] with the
/// draws of one frame each time the driver calls [`Self::build_scene`]. The
/// scene is the driver's, not the source's: a source in the scope of several
/// passes is built once per pass, each time into that pass's own scene.
///
/// Where a source records is not part of the trait: it belongs to the
/// [`Source`] the entity carries: one kind of source can record at two
/// different orders, and writing that field reorders a live source.
pub trait DrawSource: 'static {
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
    /// per pass, so per-pass state must be keyed by [`PassInfo::pass`] and
    /// per-target state by [`PassInfo::target`] rather than kept in one field.
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
}

/// A draw source behind an erased handle, so one query sees every concrete
/// source type.
///
/// Method-free on purpose: [`Any`] supplies downcasting, the [`DrawSource`]
/// supertrait supplies the trait API, and the blanket impl means an implementor
/// writes no boilerplate at all.
pub trait AnySource: Any + DrawSource {}

impl<T: DrawSource> AnySource for T {}

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

/// A draw source as an ECS component.
///
/// The trait API is reachable directly (`source.build_scene(..)`); typed access
/// upcasts to `dyn Any` and downcasts, which [`Source::as_mut`] and
/// [`Source::as_ref`] wrap as a [`Result`] carrying [`WrongSourceType`].
///
/// The order belongs to the entity rather than to the source's type: two
/// entities built from the same kind of source record at whatever order each
/// was given, and writing the field reorders a live source in place.
pub struct Source {
    source: Box<dyn AnySource>,
    /// The concrete type `source` was built from, for a readable refusal.
    type_name: &'static str,
    /// Where this source records, relative to the others.
    pub order: SourceOrder,
}

impl Source {
    /// Wrap `source`, recording it at `order`.
    pub fn new<S: DrawSource>(order: SourceOrder, source: S) -> Self {
        Self {
            type_name: core::any::type_name::<S>(),
            source: Box::new(source),
            order,
        }
    }

    /// The source as its concrete type.
    ///
    /// # Errors
    ///
    /// Answers [`WrongSourceType`] when the source holds a different type,
    /// naming both the type it holds and the one asked for.
    pub fn as_mut<T: DrawSource>(&mut self) -> Result<&mut T, WrongSourceType> {
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
    pub fn as_ref<T: DrawSource>(&self) -> Result<&T, WrongSourceType> {
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
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

/// Spawn the frame's GPU state as resources and return its addresses.
///
/// The device and queue are `Clone` handles on a shared context, so the world
/// holds its own reference to each; the graph is not `Sync` and moves into the
/// world whole. Each address gets a row of its own in the shared subtree, and
/// the returned [`RenderContext`] names them; the context itself is spawned
/// under its own singleton key, so a second call panics rather than leaving two
/// contexts that disagree about which rows belong to the frame.
///
/// `render_device` carries the device and what a frame reads about it: the
/// capabilities it was requested with, and whether it is still alive. Build one
/// from the context that opened the device — `RenderDevice::from(&gpu)`, or
/// `gpu.into()` for the last user of it — or state the device's facts directly
/// for a device nobody opened through a [`GpuContext`](unlit_wgpu::context::GpuContext).
pub fn spawn_context(
    world: &mut World,
    render_device: RenderDevice,
    graph_component: ResourceGraph,
) -> RenderContext {
    let RenderDevice {
        device,
        queue,
        capabilities,
        health,
    } = render_device;
    // The frame's uniform registry is built against the graph it registers
    // its buffers in, so the graph is taken before either is spawned.
    let mut graph_component = graph_component;
    let uniforms = GlobalUniforms::new(&device, &mut graph_component);
    // Every one of these belongs to the shared subtree: a source reads the GPU
    // state from its own scope, and no scope may remove what every other scope
    // depends on.
    let shared = world.shared();
    let mut shared_view = world.scope_mut(shared);
    let device = shared_view.spawn_named("gpu_device", bundle![device]);
    let queue = shared_view.spawn_named("gpu_queue", bundle![queue]);
    let graph = shared_view.spawn_named("render_graph", bundle![graph_component]);
    let capabilities = shared_view.spawn_named("device_capabilities", bundle![capabilities]);
    let health = shared_view.spawn_named("device_health", bundle![health]);
    let uniforms = shared_view.spawn_named("global_uniforms", bundle![uniforms]);
    // How much time has passed, advanced once per frame by the frame loop. It
    // belongs to the world rather than to a source because a source may be
    // built once per pass: a clock each source counted for itself would run
    // faster on a frame with more passes.
    let clock = shared_view.spawn_named("frame_clock", bundle![FrameClock::default()]);
    // The URLs the frame's sources asked to open, drained by the host after
    // the frame. The queue is a singleton: one world opens one set of tabs,
    // and every producer appends to the same queue.
    shared_view.spawn(bundle![OpenUrls::default()].singleton::<OpenUrls>());
    let context = RenderContext {
        device,
        queue,
        graph,
        capabilities,
        health,
        clock,
        uniforms,
    };
    // The context names the rows above, so it is spawned once they exist. It is
    // a singleton: a world draws its frames through one context, and a second
    // one would leave every address ambiguous.
    shared_view.spawn_named(
        "render_context",
        bundle![context].singleton::<RenderContext>(),
    );
    context
}

#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod tests;
