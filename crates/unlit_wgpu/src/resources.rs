//! Dependency-tracked GPU resource graph.
//!
//! The graph is the renderer's single source of truth for the wgpu resources a
//! frame draws with — textures and their views, samplers, buffers and bind
//! groups — and for the *virtual* nodes that hold no handle at all, which
//! group or stand in for the resources around them. Pipeline objects (shader
//! modules, layouts, pipelines) are not tracked: they are immutable once
//! built, and the render pipelines this crate builds are cached by their
//! variant rather than rebuilt from dependencies. Resources are plain wgpu
//! handles — the graph does not wrap them — but it remembers which resource
//! was built from which, so that derived resources can be rebuilt when their
//! inputs change:
//!
//! * [`ResourceGraph::replace`] swaps a resource and marks every resource
//!   transitively built from it as *dirty*.
//! * [`ResourceGraph::remove`] drops a resource together with everything
//!   transitively built from it and returns what was dropped;
//!   [`ResourceGraph::remove_drop`] does the same without returning it.
//! * [`ResourceGraph::rebuild_dirty`] walks the dirty resources in dependency
//!   order and lets the caller rebuild each one.
//! * [`ResourceGraph::cleanup`] collects the resources nothing alive reads any
//!   more and returns them; [`ResourceGraph::cleanup_drop`] drops them without
//!   returning them.
//!
//! Updates are therefore lazy and precise: uploading new bytes into an
//! existing buffer does not dirty anything, reallocating it does, and only the
//! resources that actually consumed the old handle are affected.
//!
//! # Typed ids
//!
//! A [`ResourceId`] is typed by the [`ResourceKind`] it names, and the kind is
//! the resource itself: [`ResourceId<wgpu::Buffer>`](ResourceId) resolves to a
//! buffer, [`ResourceId<TextureView>`](ResourceId) to a texture view together
//! with its format, and [`ResourceId<Virtual>`](ResourceId) to a node that
//! holds no handle.
//! [`ResourceGraph::get`] hands that resource back directly, so no caller
//! matches on which variant a node holds and a texture view cannot be read
//! where a buffer is wanted.
//!
//! The kind is a compile-time claim rather than runtime state: the graph stores
//! every resource in one [`Resource`] enum, and a typed id is an index that
//! remembers what it names. [Insertion](ResourceGraph::insert_strong) derives
//! the kind from the resource it is given and
//! [`replace`](ResourceGraph::replace) requires the replacement to have the
//! same kind, so an id never names a resource of another kind. Where the kind
//! is genuinely unknown at compile time — a node's dependencies, a dirty node,
//! whatever a removal drops — the [erased](ResourceId::erase)
//! `ResourceId<Resource>` names it instead.
//!
//! # Retention
//!
//! A removal only reaches the resources derived from the one removed, so a
//! resource that *feeds* the removed subtree — a uniform a bind group reads —
//! outlives the walk. Such a node is an orphan: nothing alive needs it, but no
//! walk from an existing node reaches it. [`ResourceGraph::cleanup`] collects
//! it.
//!
//! Because a node with no dependents is indistinguishable from a resource the
//! caller still uses, every insertion says which one it is:
//! [`ResourceGraph::insert_strong`] for a resource the caller holds and uses
//! directly, and [`ResourceGraph::insert_weak`] for one that exists only to
//! feed a consumer. Cleanup keeps a strong node, and keeps every resource a
//! live node was built from; a weak node whose dependents are all gone is
//! collected.
//!
//! Insertion is immediate: `insert_strong` adds the node and returns its id,
//! and [`add_dependency`](ResourceGraph::add_dependency) records an edge.
//! Nothing defers, so there is no insertion state to finish and no failure to
//! report — a dependency id that no longer resolves, or an edge that would
//! close a cycle, is a bug in the caller and panics where it is declared.
//!
//! A [`Virtual`] node commonly serves as an *aggregation root* for a group of
//! resources: its parts are inserted weak and with no dependencies, and the
//! root is inserted strong with every part as a dependency, so the root is the
//! group's single lifetime entry point. Liveness runs from the strong root to
//! the parts, so the parts survive while the root does; removing the root drops
//! only what was built *from* it — nothing, for a virtual root — and leaves the
//! parts for [`ResourceGraph::cleanup`], which collects them unless another
//! live node is still built from them. A part shared by two roots therefore
//! outlives either one alone.

use smallvec::SmallVec;

use crate::dag::{Dag, EdgeError, NodeId};

/// A kind of resource that can live in a [`ResourceGraph`], and the resource an
/// id of that kind resolves to.
///
/// Implemented for the wgpu handles the graph stores and for [`Virtual`], the
/// kind that holds no handle. The implementor is not just a tag: it is the
/// payload [`ResourceGraph::get`] hands out, so the kind and the resource read
/// as one thing — a `ResourceId<wgpu::Buffer>` names a buffer, not a mark
/// saying "buffer".
///
/// [`Resource`] itself implements the trait as the *erased* kind: the one that
/// names a resource whose kind is known only at runtime.
pub trait ResourceKind: Sized {
    /// Borrow this kind out of a stored [`Resource`], or `None` when that
    /// resource is of another kind.
    fn borrow_from(resource: &Resource) -> Option<&Self>;

    /// Take this kind out of a stored [`Resource`], or `None` when that
    /// resource is of another kind.
    fn take_from(resource: Resource) -> Option<Self>;

    /// This value as a stored [`Resource`].
    fn into_resource(self) -> Resource;
}

/// The kind of a [virtual](Resource::Virtual) node: a graph citizen holding no
/// wgpu handle.
///
/// [`ResourceId<Virtual>`](ResourceId) names such a node. It is an ordinary
/// node otherwise — it takes dependencies, dependents and liveness like any
/// resource — and it is the natural aggregation root for a set of resources
/// that live and die together (see [Retention](self)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Virtual;

/// The one [`Virtual`] value, so its [`ResourceKind`] can hand out a borrow.
static VIRTUAL: Virtual = Virtual;

impl ResourceKind for Virtual {
    fn borrow_from(resource: &Resource) -> Option<&Self> {
        matches!(resource, Resource::Virtual).then_some(&VIRTUAL)
    }

    fn take_from(resource: Resource) -> Option<Self> {
        matches!(resource, Resource::Virtual).then_some(VIRTUAL)
    }

    fn into_resource(self) -> Resource {
        Resource::Virtual
    }
}

/// A texture view together with the format it views its texture as.
///
/// wgpu exposes no view format, and a view may be created with one besides its
/// texture's — an sRGB view over a non-sRGB swap-chain image, for example — so
/// the format a view was created with is tracked next to it rather than
/// assumed from the texture. This is the kind a
/// [`ResourceId<TextureView>`](ResourceId) names, and
/// [`TextureExt::create_view`] builds one from a descriptor, so the format and
/// the view cannot disagree.
///
/// A view wgpu itself handed out states its format explicitly with
/// [`TextureView::new`]: there is no way to recover it afterwards, which is
/// exactly why this wrapper exists.
#[expect(
    clippy::disallowed_types,
    reason = "the one place the bare wgpu type is named: every other caller \
              goes through this wrapper so a view's format is never guessed"
)]
#[derive(Clone, Debug)]
pub struct TextureView {
    /// The view.
    view: wgpu::TextureView,
    /// The format the view was created with.
    format: wgpu::TextureFormat,
}

#[expect(
    clippy::disallowed_types,
    reason = "the wrapper's own accessors are the boundary the bare type \
              crosses, and the only place it may"
)]
impl TextureView {
    /// Pair `view` with the `format` it views its texture as.
    pub fn new(view: wgpu::TextureView, format: wgpu::TextureFormat) -> Self {
        Self { view, format }
    }

    /// The view itself, for the wgpu calls that bind it.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The texture the view views.
    pub fn texture(&self) -> &wgpu::Texture {
        self.view.texture()
    }

    /// The format the view was created with.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "unwrapping the wrapper into the bare type has to name that type"
)]
impl From<TextureView> for wgpu::TextureView {
    fn from(value: TextureView) -> Self {
        value.view
    }
}

/// Creating texture views whose format the graph can track.
///
/// wgpu's own [`wgpu::Texture::create_view`] returns a bare
/// [`wgpu::TextureView`], which reports its texture's format rather than the
/// one it was created with, so a view that reinterprets its texture — an sRGB
/// view over a non-sRGB swap-chain image — cannot be told apart from a default
/// one afterwards. This trait's `create_view` reads the format out of the
/// descriptor and pairs it with the view.
///
/// It is deliberately *not* named differently from wgpu's own method: wgpu's
/// inherent method shadows it, so a call has to name the trait —
/// `TextureExt::create_view(&texture, &descriptor)` — which makes the choice
/// between the two visible at the call site.
pub trait TextureExt {
    /// Create a view of this texture, recording the format it views it as.
    ///
    /// The format is the descriptor's own when it states one, and the
    /// texture's otherwise — exactly what wgpu resolves the two to.
    fn create_view(&self, descriptor: &wgpu::TextureViewDescriptor<'_>) -> TextureView;
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one caller of wgpu's own constructor: reading the format out \
              of the descriptor is what makes the tracking possible"
)]
impl TextureExt for wgpu::Texture {
    fn create_view(&self, descriptor: &wgpu::TextureViewDescriptor<'_>) -> TextureView {
        let format = descriptor.format.unwrap_or_else(|| self.format());
        // Named in full: the trait method would otherwise shadow itself.
        let view = wgpu::Texture::create_view(self, descriptor);
        TextureView::new(view, format)
    }
}

/// A wgpu resource owned by the graph, or a virtual node standing in for one.
///
/// This is the stored form, and the way to name a resource whose kind is only
/// known at runtime: it is the [erased](ResourceId::erase) [`ResourceId`]'s
/// kind. Variants are thin — each holds the wgpu handle itself — so callers can
/// keep working with raw wgpu and use the graph purely for bookkeeping.
#[derive(Clone, Debug)]
pub enum Resource {
    /// A buffer (vertex, index, uniform or storage).
    Buffer(wgpu::Buffer),
    /// A texture.
    Texture(wgpu::Texture),
    /// A texture view, with the format it views its texture as.
    TextureView(TextureView),
    /// A sampler.
    Sampler(wgpu::Sampler),
    /// A bind group.
    BindGroup(wgpu::BindGroup),
    /// A virtual node: no wgpu handle, purely a graph citizen.
    ///
    /// It carries no GPU resource of its own: it is what an id over
    /// [`Virtual`] resolves to, and it exists to group or stand in for other
    /// nodes. The edges leaving it express ownership or derivation rather than
    /// handle derivation, which makes it the natural aggregation root for a
    /// group of resources that live and die together (see [Retention](self)).
    ///
    /// It is an ordinary node otherwise: it takes dependencies like any other,
    /// and replacing it swaps one virtual value for another — a node's kind
    /// never changes — while marking the nodes built from it dirty.
    Virtual,
}

impl Resource {
    /// Whether `other` is the same kind of resource as this one.
    ///
    /// A node keeps its kind for its whole life, so a replacement has to agree
    /// with what it replaces; this is how [`ResourceGraph::rebuild_dirty`],
    /// which hands resources around [erased](ResourceId::erase), checks that.
    fn same_kind(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

impl ResourceKind for Resource {
    fn borrow_from(resource: &Resource) -> Option<&Self> {
        Some(resource)
    }

    fn take_from(resource: Resource) -> Option<Self> {
        Some(resource)
    }

    fn into_resource(self) -> Resource {
        self
    }
}

/// Implement [`ResourceKind`] for a kind stored in one [`Resource`] variant.
///
/// The kind is what the variant holds, so the kind's type names the variant
/// too. Every kind the graph stores is one of these: a variant carries nothing
/// but its kind, and anything a kind needs beyond its handle — a texture view's
/// format — lives inside the kind itself rather than beside it in the variant.
macro_rules! stored_kind {
    ($( $handle:ty => $variant:ident ),+ $(,)?) => {
        $(
            impl ResourceKind for $handle {
                fn borrow_from(resource: &Resource) -> Option<&Self> {
                    match resource {
                        Resource::$variant(handle) => Some(handle),
                        _ => None,
                    }
                }

                fn take_from(resource: Resource) -> Option<Self> {
                    match resource {
                        Resource::$variant(handle) => Some(handle),
                        _ => None,
                    }
                }

                fn into_resource(self) -> Resource {
                    Resource::$variant(self)
                }
            }
        )+
    };
}

stored_kind! {
    wgpu::Buffer => Buffer,
    wgpu::Texture => Texture,
    TextureView => TextureView,
    wgpu::Sampler => Sampler,
    wgpu::BindGroup => BindGroup,
}

/// Implement the erasing conversion into [`Resource`] for a kind whose stored
/// variant carries it as its only payload.
macro_rules! into_resource {
    ($( $handle:ty => $variant:ident ),+ $(,)?) => {
        $(
            impl From<$handle> for Resource {
                fn from(value: $handle) -> Self {
                    Self::$variant(value)
                }
            }
        )+
    };
}

into_resource! {
    wgpu::Buffer => Buffer,
    wgpu::Texture => Texture,
    TextureView => TextureView,
    wgpu::Sampler => Sampler,
    wgpu::BindGroup => BindGroup,
}

/// Handle to a resource stored in a [`ResourceGraph`], typed by its kind.
///
/// `R` is the [`ResourceKind`] the id names, which is also what the graph hands
/// back for it: a [`ResourceId<wgpu::Buffer>`](ResourceId) resolves to a
/// buffer, a [`ResourceId<wgpu::TextureView>`](ResourceId) to a view. The
/// default, [`Resource`], is the *erased* kind — an id to a resource whose kind
/// is known only at runtime — which is what graph-wide reads like
/// [`ResourceGraph::dirty`] and [`ResourceGraph::dependencies`] hand out.
///
/// While the resource lives, its id resolves through every accessor.
/// Removing the resource invalidates every id to it: a later resource reuses
/// the freed slot, but the id records the generation it was handed out with,
/// so an id to a removed resource resolves to nothing rather than to whatever
/// took its place. Callers may therefore hold an id across removals and simply
/// see it stop resolving — an id is never confirmed dead and then revived.
pub struct ResourceId<R = Resource> {
    node: NodeId,
    kind: core::marker::PhantomData<fn() -> R>,
}

impl<R> ResourceId<R> {
    /// The graph index this id refers to.
    ///
    /// Only meaningful while the resource lives, but stable across the
    /// resource's lifetime: callers can group or sort by it (ordering draws
    /// so consecutive ones share a bind group, for example) without holding
    /// a borrow.
    pub fn index(&self) -> usize {
        self.node.index()
    }

    /// Forget the kind, yielding an id to the same resource that claims none.
    ///
    /// This is how a typed id is handed to an API that works in any kind — the
    /// erased [`ResourceId<Resource>`](Resource) that
    /// [`ResourceGraph::dependencies`] and [`ResourceGraph::dirty`] speak in,
    /// or a stored field a caller only ever passes back to the graph.
    pub fn erase(self) -> ResourceId {
        ResourceId {
            node: self.node,
            kind: core::marker::PhantomData,
        }
    }
}

// The trait impls are written out rather than derived: a derived bound would
// ask `R` to be comparable, hashable or cloneable, none of which a kind has to
// be. An id is a node handle, and only the handle takes part in any of them.
impl<R> Clone for ResourceId<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for ResourceId<R> {}

impl<R> PartialEq for ResourceId<R> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<R> Eq for ResourceId<R> {}

impl<R> PartialOrd for ResourceId<R> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for ResourceId<R> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.node.cmp(&other.node)
    }
}

impl<R> core::hash::Hash for ResourceId<R> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.node.hash(state);
    }
}

impl<R> core::fmt::Debug for ResourceId<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ResourceId({})", self.node.index())
    }
}

#[derive(Debug)]
struct Node {
    resource: Resource,
    /// Set when this resource or one of its dependencies was replaced.
    dirty: bool,
    /// Whether the resource survives cleanup on its own rather than only
    /// through the resources built from it. See [Retention](self).
    strong: bool,
}

/// Direct dependencies a rebuild keeps on the stack; beyond this, they spill
/// onto the heap. Edges are added as they are declared, so a rebuild that
/// *reads* a node's dependencies is the only place that collects them.
const MAX_INLINED_DIRECT_DEPENDENCIES: usize = 8;

/// A directed acyclic graph of wgpu resources.
///
/// Every edge points from a dependency to a resource built from it, so the
/// dependents of a node are exactly the nodes reachable from it. The graph
/// refuses an edge that would close a cycle, which is what lets
/// [`Self::dirty`] and [`Self::rebuild_dirty`] visit every node in dependency
/// order.
#[derive(Debug, Default)]
pub struct ResourceGraph {
    graph: Dag<Node>,
}

impl ResourceGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of resources in the graph.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    /// Whether the graph holds no resources.
    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    /// Add `resource` as a *strong* node that [`Self::cleanup`] keeps whether
    /// or not anything depends on it.
    ///
    /// Use this for a resource the caller holds and uses directly. Use
    /// [`Self::insert_weak`] for one that exists only to feed a consumer.
    ///
    /// The kind of the returned id is the resource's own:
    /// `graph.insert_strong(buffer)` yields a buffer id, and
    /// `insert_strong(Resource::Virtual)` — or `insert_strong(Virtual)` — a
    /// [virtual](Virtual) one. The node is added at once; declare what it was
    /// built from with [`Self::add_dependency`].
    pub fn insert_strong<R: ResourceKind>(&mut self, resource: R) -> ResourceId<R> {
        self.insert(resource, true)
    }

    /// Add `resource` as a *weak* node that [`Self::cleanup`] collects once
    /// nothing alive is built from it.
    ///
    /// Use this for a resource that only feeds a consumer — a uniform a bind
    /// group reads, an input to a derived resource — so that dropping the
    /// consumer also drops it. Use [`Self::insert_strong`] for a resource the
    /// caller holds itself.
    ///
    /// See [`Self::insert_strong`] for how the kind of the id follows from the
    /// resource.
    pub fn insert_weak<R: ResourceKind>(&mut self, resource: R) -> ResourceId<R> {
        self.insert(resource, false)
    }

    fn insert<R: ResourceKind>(&mut self, resource: R, strong: bool) -> ResourceId<R> {
        let node = self.graph.insert(Node {
            resource: resource.into_resource(),
            dirty: false,
            strong,
        });
        ResourceId {
            node,
            kind: core::marker::PhantomData,
        }
    }

    /// Record that the resource behind `dependent` was built from the one
    /// behind `dependency`, so replacing, removing or rebuilding the latter
    /// reaches the former.
    ///
    /// The dependency may be of any kind: one bind group is built from buffers,
    /// a view and a sampler all at once. Call this once per input; declaring
    /// the same pair twice records one edge, since a node depends on another
    /// once or not at all.
    ///
    /// An optional input is the caller's to skip — `if let Some(id) = ...`
    /// around the call, which reads the way the id itself does — rather than
    /// something this method silently ignores: an absent dependency is a
    /// decision about the caller's graph, not an edge.
    ///
    /// # Panics
    ///
    /// Panics if either id does not resolve, or if the edge would close a
    /// cycle. A non-resolving id means the caller kept one past the removal of
    /// the resource it named; a cycle means the declared dependencies are not
    /// a build order at all. Either way the graph would stop being a graph of
    /// resources in dependency order, so the offending edge is refused where
    /// it is declared rather than corrupting a later walk.
    pub fn add_dependency<D, R>(&mut self, dependent: ResourceId<R>, dependency: ResourceId<D>) {
        match self.graph.add_edge(dependency.node, dependent.node) {
            Ok(()) => {}
            Err(EdgeError::NoSuchNode(node)) if node == dependent.node => panic!(
                "add_dependency: the dependent {dependent:?} is not in the graph, \
                 so nothing can depend on {dependency:?}"
            ),
            Err(EdgeError::NoSuchNode(_)) => panic!(
                "add_dependency: the dependency {dependency:?} of {dependent:?} \
                 is not in the graph"
            ),
            Err(EdgeError::SelfLoop) => panic!(
                "add_dependency: {dependent:?} cannot depend on itself \
                 (the dependency {dependency:?} no longer resolves and its slot was reused)"
            ),
            Err(EdgeError::Cycle) => panic!(
                "add_dependency: {dependent:?} cannot depend on {dependency:?}, \
                 which would close a cycle"
            ),
        }
    }

    /// Borrow the resource behind `id`.
    ///
    /// The id says what it names, so this is the buffer, texture, view,
    /// sampler or bind group itself; the [erased](ResourceId::erase)
    /// `ResourceId<Resource>` is how a caller reads a resource whose kind it
    /// does not know. `None` when the id is unknown — nothing else can make it
    /// fail, since an id only ever names a resource of its own kind.
    pub fn get<R: ResourceKind>(&self, id: ResourceId<R>) -> Option<&R> {
        self.graph
            .get(id.node)
            .and_then(|node| R::borrow_from(&node.resource))
    }

    /// Replace the resource behind `id` and mark it — and every resource
    /// transitively built from it — dirty.
    ///
    /// The replacement has to be of the id's own kind: a node never changes
    /// kind, so an id that names a buffer cannot come to name a sampler.
    ///
    /// Returns the previous handle, or `None` if `id` is unknown.
    pub fn replace<R: ResourceKind>(
        &mut self,
        id: ResourceId<R>,
        resource: impl Into<R>,
    ) -> Option<R> {
        R::take_from(self.replace_resource(id.node, resource.into().into_resource())?)
    }

    /// Swap the resource stored at `node`, marking it and its dependents
    /// dirty, and hand back what was there.
    fn replace_resource(&mut self, node: NodeId, resource: Resource) -> Option<Resource> {
        let previous = {
            let slot = self.graph.get_mut(node)?;
            slot.dirty = true;
            core::mem::replace(&mut slot.resource, resource)
        };
        self.mark_dependents_dirty(node);
        Some(previous)
    }

    /// Remove `id` together with every resource transitively built from it,
    /// returning everything that was dropped.
    ///
    /// The removed resources are returned in dependency order (dependencies
    /// first), so the last entries are the roots of the removed subtree.
    ///
    /// When the dropped resources are not needed, prefer [`Self::remove_drop`],
    /// which skips building the return vector.
    pub fn remove<R>(&mut self, id: ResourceId<R>) -> Vec<Resource> {
        self.graph
            .remove_dependents(id.node)
            .into_iter()
            .map(|node| node.resource)
            .collect()
    }

    /// Remove `id` together with every resource transitively built from it,
    /// dropping everything without returning it.
    ///
    /// This is [`Self::remove`] for callers that do not need the dropped
    /// resources: it avoids allocating the return vector.
    pub fn remove_drop<R>(&mut self, id: ResourceId<R>) {
        self.graph.remove_dependents_drop(id.node);
    }

    /// Collect and return every resource nothing alive is built from.
    ///
    /// A resource is alive when it was inserted [strong](Self::insert_strong),
    /// or when something alive was built from it. Everything else — a weak
    /// resource whose every consumer has been removed — is dropped, and so is
    /// anything built only from resources that are themselves dropped.
    ///
    /// This is what collects an orphan like a uniform feeding a bind group:
    /// [`Self::remove`] on the bind group's inputs reaches the group but never
    /// the uniform it was built from, so the uniform survives that walk and is
    /// only freed here, once nothing that reads it is left.
    ///
    /// The returned resources are in unspecified order. Nothing that was
    /// removed is referenced by a surviving node: a strongly held resource is
    /// always kept, and a resource a live node was built from is kept too.
    ///
    /// When the dropped resources are not needed, prefer
    /// [`Self::cleanup_drop`], which skips building the return vector.
    pub fn cleanup(&mut self) -> Vec<Resource> {
        let stamp = self.mark_alive();
        self.graph
            .remove_unmarked(stamp)
            .into_iter()
            .map(|node| node.resource)
            .collect()
    }

    /// Drop every resource nothing alive is built from, without returning it.
    ///
    /// This is [`Self::cleanup`] for callers that do not need the dropped
    /// resources: it avoids allocating the return vector.
    pub fn cleanup_drop(&mut self) {
        let stamp = self.mark_alive();
        self.graph.remove_unmarked_drop(stamp);
    }

    /// Mark every node that is strong or that a strong node was built from,
    /// and return the stamp the marks carry.
    ///
    /// Aliveness propagates from a dependent to what it was built from, so the
    /// walk goes against the edges — from each strong node to its dependencies.
    /// The walk allocates nothing per strong node: the marks live in the
    /// graph's own per-slot stamps, which a single pass fills.
    fn mark_alive(&mut self) -> u32 {
        self.graph.mark_dependencies_where(|node| node.strong)
    }

    /// Whether `id` needs to be rebuilt before it can be used again.
    pub fn is_dirty<R>(&self, id: ResourceId<R>) -> bool {
        self.graph.get(id.node).is_some_and(|node| node.dirty)
    }

    /// Whether any resource in the graph is dirty.
    pub fn any_dirty(&self) -> bool {
        self.graph.node_weights().any(|node| node.dirty)
    }

    /// Mark `id` clean, for example after rebuilding it outside
    /// [`Self::rebuild_dirty`].
    pub fn mark_clean<R>(&mut self, id: ResourceId<R>) {
        if let Some(node) = self.graph.get_mut(id.node) {
            node.dirty = false;
        }
    }

    /// The immediate dependencies recorded for `id`.
    ///
    /// A node's inputs may be of any kind, so the ids come back
    /// [erased](ResourceId::erase).
    pub fn dependencies<R>(&self, id: ResourceId<R>) -> impl Iterator<Item = ResourceId> + '_ {
        self.graph.dependencies(id.node).map(|node| ResourceId {
            node,
            kind: core::marker::PhantomData,
        })
    }

    /// Every dirty resource, [erased](ResourceId::erase) like
    /// [`Self::dependencies`], in dependency order (a resource always follows
    /// the resources it was built from).
    pub fn dirty(&self) -> impl Iterator<Item = ResourceId> + '_ {
        // The graph is acyclic by construction, so its topological order is a
        // true dependency order and this visits every node.
        self.graph
            .topological_order()
            .into_iter()
            .filter(|&node| self.graph.get(node).is_some_and(|node| node.dirty))
            .map(|node| ResourceId {
                node,
                kind: core::marker::PhantomData,
            })
    }

    /// Rebuild every dirty resource by calling `rebuild` in dependency order.
    ///
    /// `rebuild` receives the dirty resource's id — [erased](ResourceId::erase),
    /// since the graph visits every kind — its current handle, and its direct
    /// dependencies' current handles. Returning `Some` replaces the resource
    /// and clears its dirty flag; returning `None` leaves it dirty, which is
    /// how a caller defers work it cannot do yet. The replacement must be of
    /// the resource's own kind — a node never changes kind, which is what lets
    /// a typed [`ResourceId`] keep naming the same thing — and a mismatch
    /// trips a debug assertion.
    ///
    /// Dependents are visited after their dependencies, so a rebuild observes
    /// already-updated inputs.
    pub fn rebuild_dirty<F>(&mut self, mut rebuild: F)
    where
        F: FnMut(ResourceId, &Resource, &[Resource]) -> Option<Resource>,
    {
        // The order is fixed up front, so the rebuilds cannot disturb the walk
        // they are part of even though `rebuild` runs between its steps.
        // `rebuild` cannot touch the graph — it receives only ids and handles
        // — so the nodes it names are all still there.
        for node in self.graph.topological_order() {
            if !self.graph.get(node).is_some_and(|node| node.dirty) {
                continue;
            }
            let id = ResourceId {
                node,
                kind: core::marker::PhantomData,
            };
            // Dependency handles are cheap reference-counted clones, so the
            // common case — a handful of dependencies — stays on the stack.
            let dependencies: SmallVec<[Resource; MAX_INLINED_DIRECT_DEPENDENCIES]> = self
                .dependencies(id)
                .filter_map(|dependency| self.get(dependency).cloned())
                .collect();
            let Some(current) = self.get(id) else {
                continue;
            };
            let Some(rebuilt) = rebuild(id, current, &dependencies) else {
                continue;
            };
            debug_assert!(
                current.same_kind(&rebuilt),
                "a rebuilt resource must be of the same kind as the one it replaces"
            );
            if let Some(node) = self.graph.get_mut(node) {
                node.resource = rebuilt;
                node.dirty = false;
            }
        }
    }

    /// Mark `node` and every node built from it dirty.
    fn mark_dependents_dirty(&mut self, node: NodeId) {
        self.graph.for_each_dependent_mut(node, |slot| {
            slot.dirty = true;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource stand-in: the graph logic never inspects the handle, so the
    /// tests can use a buffer.
    fn buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// A buffer of a distinctive size, so tests can tell handles apart —
    /// wgpu handles carry no comparable identity.
    fn sized_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn device() -> wgpu::Device {
        crate::util::test::noop_device().0
    }

    /// A dependence-free strong buffer, the leaf most tests build from.
    fn strong_buffer(graph: &mut ResourceGraph, device: &wgpu::Device, label: &str) -> BufferId {
        graph.insert_strong(buffer(device, label))
    }

    /// The id a [`strong_buffer`] hands back: a buffer id, spelled out so the
    /// tests do not have to infer it.
    type BufferId = ResourceId<wgpu::Buffer>;

    #[test]
    fn replace_marks_dependents_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let middle = graph.insert_strong(buffer(&device, "middle"));
        graph.add_dependency(middle, base);
        let leaf = graph.insert_strong(buffer(&device, "leaf"));
        graph.add_dependency(leaf, middle);

        assert!(!graph.any_dirty());

        graph.replace(base, buffer(&device, "base2"));
        assert!(graph.is_dirty(base));
        assert!(graph.is_dirty(middle));
        assert!(graph.is_dirty(leaf));
        // Dependency order: base before middle before leaf.
        assert_eq!(
            graph.dirty().collect::<Vec<_>>(),
            vec![base.erase(), middle.erase(), leaf.erase()]
        );
    }

    #[test]
    fn replace_does_not_dirty_unrelated_resources() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = strong_buffer(&mut graph, &device, "a");
        let b = strong_buffer(&mut graph, &device, "b");

        graph.replace(a, buffer(&device, "a2"));
        assert!(graph.is_dirty(a));
        assert!(!graph.is_dirty(b));
    }

    /// The old handle is handed back in the id's own kind, so a caller
    /// replacing a buffer never sees another kind of resource.
    #[test]
    fn replace_returns_the_previous_handle() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(sized_buffer(&device, 64));

        let previous = graph.replace(id, sized_buffer(&device, 128)).unwrap();
        assert_eq!(previous.size(), 64);
        assert_eq!(graph.get(id).unwrap().size(), 128);
    }

    #[test]
    fn remove_drops_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        graph.add_dependency(dependent, base);
        let unrelated = strong_buffer(&mut graph, &device, "unrelated");

        let removed = graph.remove(base);
        assert_eq!(removed.len(), 2);
        assert!(graph.get(base).is_none());
        assert!(graph.get(dependent).is_none());
        assert!(graph.get(unrelated).is_some());
        assert_eq!(graph.len(), 1);
    }

    #[test]
    fn rebuild_dirty_visits_in_dependency_order() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        graph.add_dependency(dependent, base);

        graph.replace(base, buffer(&device, "base2"));

        let mut visited = Vec::new();
        graph.rebuild_dirty(|id, _current, dependencies| {
            visited.push(id);
            if id == dependent.erase() {
                // The dependency was already rebuilt, so the caller observes
                // the fresh handle rather than the replaced one.
                assert_eq!(dependencies.len(), 1);
            }
            Some(buffer(&device, "rebuilt").into())
        });

        assert_eq!(visited, vec![base.erase(), dependent.erase()]);
        assert!(!graph.any_dirty());
    }

    #[test]
    fn rebuild_can_defer() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        graph.replace(base, buffer(&device, "base2"));

        graph.rebuild_dirty(|_, _, _| None);
        assert!(graph.is_dirty(base));

        graph.mark_clean(base);
        assert!(!graph.any_dirty());
    }

    #[test]
    #[should_panic(expected = "is not in the graph")]
    fn add_dependency_rejects_a_removed_dependency() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        let stale = graph.insert_strong(buffer(&device, "stale"));
        graph.remove_drop(stale);
        // Nothing was inserted since, so `stale`'s freed slot is still free —
        // the case the recycled-slot test below is the trap of.
        assert!(graph.get(base).is_some());

        graph.add_dependency(dependent, stale);
    }

    #[test]
    #[should_panic(expected = "is not in the graph")]
    fn add_dependency_rejects_a_dependency_whose_slot_it_reused() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = strong_buffer(&mut graph, &device, "a");
        graph.remove_drop(id);
        // The freed slot is handed straight back to the next node, so a stale
        // handle and the live one share an index. The id also records the
        // generation it was handed out with, so the two are still told apart:
        // the stale id resolves to nothing rather than to the node that took
        // its slot.
        let dependent = graph.insert_strong(buffer(&device, "b"));
        assert_eq!(dependent.index(), id.index(), "the slot is reused");
        graph.add_dependency(dependent, id);
    }

    /// An id that outlives its resource never comes back to life: the freed
    /// slot is reused, but the handle records the generation it was handed out
    /// with, so an id to a removed resource stays dead.
    #[test]
    fn a_stale_id_never_names_the_resource_that_reuses_its_slot() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let stale = strong_buffer(&mut graph, &device, "stale");
        graph.remove_drop(stale);

        let fresh = strong_buffer(&mut graph, &device, "fresh");
        assert_eq!(fresh.index(), stale.index(), "the slot is reused");
        assert!(graph.get(fresh).is_some());
        assert!(
            graph.get(stale).is_none(),
            "the stale id resolves to nothing"
        );
        assert!(!graph.is_dirty(stale));
        assert!(
            graph
                .replace(stale, buffer(&device, "replacement"))
                .is_none()
        );
    }

    #[test]
    #[should_panic(expected = "is not in the graph")]
    fn add_dependency_rejects_a_removed_dependent() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let dependency = strong_buffer(&mut graph, &device, "a");
        let dependent = graph.insert_strong(buffer(&device, "b"));
        graph.remove_drop(dependent);

        graph.add_dependency(dependent, dependency);
    }

    /// A cycle is refused where it is declared. Without that, a length-2 or
    /// longer cycle would make the topological order silently drop the nodes
    /// in it, and a walk would stop rebuilding them without saying so.
    #[test]
    #[should_panic(expected = "would close a cycle")]
    fn add_dependency_rejects_a_cycle() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = strong_buffer(&mut graph, &device, "a");
        let b = graph.insert_strong(buffer(&device, "b"));
        graph.add_dependency(b, a);
        // `a` already reaches `b`, so the reverse edge would close a cycle.
        graph.add_dependency(a, b);
    }

    #[test]
    fn cleanup_keeps_strong_nodes_with_no_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let strong = strong_buffer(&mut graph, &device, "strong");

        assert!(graph.cleanup().is_empty());
        assert!(graph.get(strong).is_some());
    }

    #[test]
    fn cleanup_collects_a_weak_node_nothing_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"));

        assert_eq!(graph.cleanup().len(), 1);
        assert!(graph.get(weak).is_none());
        assert!(graph.is_empty());
    }

    #[test]
    fn cleanup_keeps_a_weak_node_a_strong_dependent_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"));
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        graph.add_dependency(dependent, weak);

        assert!(graph.cleanup().is_empty());
        assert!(graph.get(weak).is_some());
        assert!(graph.get(dependent).is_some());
    }

    /// The case the graph exists for: a uniform feeding a bind group is
    /// reached by no removal walk from the group's inputs, so only cleanup
    /// collects it.
    #[test]
    fn cleanup_collects_a_weak_node_orphaned_by_removing_its_dependent() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "uniform"));
        let input = strong_buffer(&mut graph, &device, "input");
        let group = graph.insert_strong(buffer(&device, "group"));
        graph.add_dependency(group, input);
        graph.add_dependency(group, weak);

        graph.remove_drop(group);
        assert!(
            graph.get(weak).is_some(),
            "the removal walk does not reach it"
        );

        graph.cleanup_drop();
        assert!(graph.get(weak).is_none());
        assert!(
            graph.get(input).is_some(),
            "a strong node outlives the group"
        );
    }

    #[test]
    fn cleanup_collects_only_what_is_unreachable_from_a_strong_node() {
        let device = device();
        let mut graph = ResourceGraph::new();
        // A weak chain feeding a strong node: every link is kept, because the
        // strong node was built from it.
        let leaf = graph.insert_weak(buffer(&device, "leaf"));
        let middle = graph.insert_weak(buffer(&device, "middle"));
        graph.add_dependency(middle, leaf);
        let root = graph.insert_strong(buffer(&device, "root"));
        graph.add_dependency(root, middle);
        // A second chain whose strong node is removed: the whole chain goes.
        let gone_leaf = graph.insert_weak(buffer(&device, "gone_leaf"));
        let gone_middle = graph.insert_weak(buffer(&device, "gone_middle"));
        graph.add_dependency(gone_middle, gone_leaf);
        let gone_root = graph.insert_strong(buffer(&device, "gone_root"));
        graph.add_dependency(gone_root, gone_middle);

        graph.remove_drop(gone_root);
        graph.cleanup_drop();

        assert!(graph.get(root).is_some());
        assert!(graph.get(middle).is_some());
        assert!(graph.get(leaf).is_some());
        assert!(graph.get(gone_root).is_none());
        assert!(graph.get(gone_middle).is_none());
        assert!(graph.get(gone_leaf).is_none());
        assert_eq!(graph.len(), 3);
    }

    #[test]
    fn remove_of_a_leaf_removes_only_that_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let leaf = strong_buffer(&mut graph, &device, "leaf");
        let other = strong_buffer(&mut graph, &device, "other");

        let removed = graph.remove(leaf);
        assert_eq!(removed.len(), 1);
        assert!(graph.get(leaf).is_none());
        assert!(graph.get(other).is_some());
        assert_eq!(graph.len(), 1);
    }

    /// The doc promises the removed subtree in dependency order, so callers
    /// can rely on the last entries being its roots.
    #[test]
    fn remove_returns_the_subtree_in_dependency_order() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(sized_buffer(&device, 64));
        let middle = graph.insert_strong(sized_buffer(&device, 128));
        graph.add_dependency(middle, base);
        let root = graph.insert_strong(sized_buffer(&device, 256));
        graph.add_dependency(root, middle);

        let removed = graph.remove(middle);
        assert_eq!(removed.len(), 2);
        let Resource::Buffer(first) = &removed[0] else {
            panic!("the first removed resource is a buffer");
        };
        let Resource::Buffer(second) = &removed[1] else {
            panic!("the second removed resource is a buffer");
        };
        assert_eq!(first.size(), 128);
        assert_eq!(second.size(), 256);
        // The resource the subtree was built from outlives the removal.
        assert!(graph.get(base).is_some());
        assert!(graph.get(root).is_none());
    }

    /// [`ResourceGraph::remove_drop`] removes the same subtree as
    /// [`ResourceGraph::remove`] without returning it.
    #[test]
    fn remove_drop_removes_the_same_subtree() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(sized_buffer(&device, 64));
        let middle = graph.insert_strong(sized_buffer(&device, 128));
        graph.add_dependency(middle, base);
        let root = graph.insert_strong(sized_buffer(&device, 256));
        graph.add_dependency(root, middle);

        graph.remove_drop(middle);
        assert!(graph.get(base).is_some());
        assert!(graph.get(middle).is_none());
        assert!(graph.get(root).is_none());
        assert_eq!(graph.len(), 1);
    }

    /// [`ResourceGraph::cleanup_drop`] collects the same orphans as
    /// [`ResourceGraph::cleanup`] without returning them.
    #[test]
    fn cleanup_drop_collects_orphans() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"));

        graph.cleanup_drop();
        assert!(graph.get(weak).is_none());
        assert!(graph.is_empty());
    }

    #[test]
    fn rebuild_observes_the_updated_handles_of_its_dependencies() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        graph.add_dependency(dependent, base);

        graph.replace(base, sized_buffer(&device, 128));

        let mut observed = Vec::new();
        graph.rebuild_dirty(|id, _, dependencies| {
            if id == dependent.erase() {
                let Resource::Buffer(buffer) = &dependencies[0] else {
                    panic!("the dependency is a buffer");
                };
                observed.push(buffer.size());
            }
            // Defer the base itself so the dependent observes the replaced
            // handle rather than a rebuild of it.
            if id == base.erase() {
                return None;
            }
            Some(buffer(&device, "rebuilt").into())
        });

        assert_eq!(observed, vec![128]);
    }

    #[test]
    fn rebuild_visits_a_diamond_in_dependency_order() {
        let device = device();
        let mut graph = ResourceGraph::new();
        //     base
        //    /    \
        //  left   right
        //    \    /
        //     join
        let base = strong_buffer(&mut graph, &device, "base");
        let left = graph.insert_strong(buffer(&device, "left"));
        graph.add_dependency(left, base);
        let right = graph.insert_strong(buffer(&device, "right"));
        graph.add_dependency(right, base);
        let join = graph.insert_strong(buffer(&device, "join"));
        graph.add_dependency(join, left);
        graph.add_dependency(join, right);

        graph.replace(base, buffer(&device, "base2"));

        let mut visited = Vec::new();
        graph.rebuild_dirty(|id, _, dependencies| {
            if id == join.erase() {
                assert_eq!(dependencies.len(), 2);
            }
            visited.push(id);
            Some(buffer(&device, "rebuilt").into())
        });

        assert_eq!(visited.len(), 4);
        assert_eq!(visited[0], base.erase());
        assert_eq!(visited[3], join.erase());
    }

    // -- kinds -------------------------------------------------------------

    /// A typed id resolves to the resource itself, without the caller
    /// knowing which variant the node holds.
    #[test]
    fn a_typed_id_resolves_to_its_own_kind() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(sized_buffer(&device, 64));

        assert_eq!(graph.get(id).unwrap().size(), 64);
        assert!(graph.get(id.erase()).is_some());
    }

    /// Erasing an id keeps it pointing at the same node, so the erased id is
    /// what a graph-wide read hands back.
    #[test]
    fn erasing_keeps_the_node() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = strong_buffer(&mut graph, &device, "a");
        let erased = id.erase();

        assert_eq!(erased.index(), id.index());
        assert!(matches!(graph.get(erased), Some(Resource::Buffer(_))));
        assert!(!graph.is_dirty(erased));
        graph.replace(id, buffer(&device, "a2"));
        assert!(graph.is_dirty(erased));
    }

    /// A view records the format its descriptor stated.
    ///
    /// This is the whole point of [`TextureView`] and [`TextureExt`]: wgpu's
    /// bare method reports the texture's format for a view that reinterprets
    /// it, so the two would otherwise be indistinguishable.
    #[test]
    fn a_texture_view_records_the_format_it_was_created_with() {
        let device = device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            // The sRGB view the tests create has to be declared up front,
            // which is what a swap chain does for the view it presents.
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let mut graph = ResourceGraph::new();
        let default = graph.insert_strong(TextureExt::create_view(
            &texture,
            &wgpu::TextureViewDescriptor::default(),
        ));
        assert_eq!(
            graph.get(default).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8Unorm)
        );

        let srgb = graph.insert_strong(TextureExt::create_view(
            &texture,
            &wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            },
        ));
        assert_eq!(
            graph.get(srgb).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
        // The view really is an sRGB one over a non-sRGB texture, which wgpu
        // itself cannot report back.
        assert_eq!(
            graph.get(srgb).unwrap().view().texture().format(),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    /// Replacing a view replaces its format with it: the two travel together
    /// in a [`TextureView`], so they cannot drift apart.
    #[test]
    fn replace_records_the_new_views_format() {
        let device = device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            // The sRGB view the tests create has to be declared up front,
            // which is what a swap chain does for the view it presents.
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(TextureExt::create_view(
            &texture,
            &wgpu::TextureViewDescriptor::default(),
        ));

        graph.replace(
            id,
            TextureExt::create_view(
                &texture,
                &wgpu::TextureViewDescriptor {
                    format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                    ..Default::default()
                },
            ),
        );
        assert_eq!(
            graph.get(id).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
        assert!(graph.is_dirty(id));
    }

    // -- virtual nodes -----------------------------------------------------

    /// The aggregation-root pattern: parts are weak and dependency-free, the
    /// root is strong and built from them, so liveness runs from the root to
    /// the parts and cleanup collects nothing.
    #[test]
    fn a_virtual_root_keeps_its_weak_parts_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"));
        let root = graph.insert_strong(Virtual);
        graph.add_dependency(root, part);

        assert!(graph.cleanup().is_empty());
        assert!(graph.get(root).is_some());
        assert!(graph.get(part).is_some());
    }

    /// Removing a virtual root drops only what was built from it — nothing —
    /// so its parts are left as orphans for the next cleanup to collect.
    #[test]
    fn removing_a_virtual_root_orphans_its_parts_for_cleanup() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"));
        let root = graph.insert_strong(Virtual);
        graph.add_dependency(root, part);

        graph.remove_drop(root);
        assert!(
            graph.get(part).is_some(),
            "the removal walk does not reach the parts"
        );

        graph.cleanup_drop();
        assert!(graph.get(part).is_none());
        assert!(graph.is_empty());
    }

    /// A part two roots are built from outlives either root alone: cleanup
    /// keeps it while the other root is still alive.
    #[test]
    fn a_shared_part_survives_while_another_root_is_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"));
        let first = graph.insert_strong(Virtual);
        graph.add_dependency(first, part);
        let second = graph.insert_strong(Virtual);
        graph.add_dependency(second, part);

        graph.remove_drop(first);
        graph.cleanup_drop();
        assert!(
            graph.get(part).is_some(),
            "the surviving root still owns the shared part"
        );
        assert!(graph.get(second).is_some());

        graph.remove_drop(second);
        graph.cleanup_drop();
        assert!(graph.get(part).is_none());
        assert!(graph.is_empty());
    }

    /// Replacing a part marks the virtual root built from it dirty, and the
    /// root follows its part in dependency order.
    #[test]
    fn replacing_a_part_marks_the_virtual_root_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"));
        let root = graph.insert_strong(Virtual);
        graph.add_dependency(root, part);

        graph.replace(part, buffer(&device, "part2"));
        assert!(graph.is_dirty(root));
        assert_eq!(
            graph.dirty().collect::<Vec<_>>(),
            vec![part.erase(), root.erase()]
        );
    }

    /// A virtual node is a graph citizen with no handle: it resolves to
    /// [`Virtual`] and to nothing else.
    #[test]
    fn a_virtual_node_has_no_handle_of_its_own() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert_strong(Virtual);

        assert!(matches!(graph.get(root), Some(Virtual)));
    }

    /// A weak virtual node is collected like any other weak node once nothing
    /// alive is built from it.
    #[test]
    fn cleanup_collects_a_weak_virtual_node_with_no_dependents() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert_weak(Virtual);

        assert_eq!(graph.cleanup().len(), 1);
        assert!(graph.get(root).is_none());
        assert!(graph.is_empty());
    }

    /// A node keeps its kind: replacing a virtual node hands back the previous
    /// virtual value, and the node stays a root its parts hang from.
    #[test]
    fn a_virtual_node_can_be_replaced() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let root = graph.insert_weak(Virtual);
        let dependent = graph.insert_strong(buffer(&device, "dependent"));
        graph.add_dependency(dependent, root);

        let previous = graph
            .replace(root, Virtual)
            .expect("the node is in the graph");
        assert_eq!(previous, Virtual);
        assert!(matches!(graph.get(root), Some(Virtual)));
        assert!(graph.is_dirty(dependent));
        assert_eq!(
            graph.dependencies(dependent).collect::<Vec<_>>(),
            vec![root.erase()]
        );
    }
}
