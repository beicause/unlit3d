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
//! * [`ResourceGraph::remove`] marks a resource, together with everything
//!   transitively built from it, for removal.
//! * [`ResourceGraph::maintain`] drops everything marked for removal, collects
//!   the resources nothing alive is built from any more, and rebuilds the
//!   resources marked dirty.
//!
//! Nothing happens at the point of the change: `replace` only flips dirty
//! flags and `remove` only records the root of the removal. Both take effect
//! the next time [`ResourceGraph::maintain`] runs, which is where a frame
//! calls them exactly once — before the resources are read.
//!
//! Updates are therefore lazy and precise: uploading new bytes into an
//! existing buffer does not dirty anything, reallocating it does, and only the
//! resources that actually consumed the old handle are affected.
//!
//! # Rebuilding
//!
//! A resource that can be rebuilt is inserted with a *recipe*:
//! [`insert_strong`](ResourceGraph::insert_strong) takes an optional
//! [`Rebuild`] closure that produces a fresh handle out of the graph. A recipe
//! reads the resources it was built from back out of the graph by id rather
//! than capturing their handles, so it observes the current state of its
//! inputs — a buffer that was reallocated or an array that was replaced — at
//! the moment it runs.
//!
//! [`ResourceGraph::maintain`] runs the recipes of the dirty nodes in
//! dependency order, so a rebuilt resource is always built from
//! already-rebuilt inputs. A dirty node with no recipe is left dirty: the
//! caller changed something the graph cannot rebuild on its own.
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
//! walk from an existing node reaches it. The same [`ResourceGraph::maintain`]
//! pass that drops the marked subtree collects it.
//!
//! Because a node with no dependents is indistinguishable from a resource the
//! caller still uses, every insertion says which one it is:
//! [`ResourceGraph::insert_strong`] for a resource the caller holds and uses
//! directly, and [`ResourceGraph::insert_weak`] for one that exists only to
//! feed a consumer. Collection keeps a strong node, and keeps every resource a
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
//! parts for [`ResourceGraph::maintain`], which collects them unless another
//! live node is still built from them. A part shared by two roots therefore
//! outlives either one alone.

use std::sync::Arc;

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

/// Two views are equal when they are the same view of the same texture.
///
/// Written out rather than derived because the derive expands to a comparison
/// of the bare [`wgpu::TextureView`] in generated code the struct's own
/// `#[expect]` does not cover. Going through [`TextureView::view`] and
/// [`TextureView::format`] keeps the bare type named only in the wrapper, which
/// is the whole point of it.
impl PartialEq for TextureView {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format && self.view() == other.view()
    }
}

impl Eq for TextureView {}

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
    /// A flat array of fixed-size elements, held either in a storage buffer or
    /// in a texture.
    ///
    /// The two are one kind rather than two because they are one thing to a
    /// caller: which of them an array lives in follows from the device, not
    /// from what the array holds. See
    /// [`ArrayHandle`](crate::texel_array::ArrayHandle).
    Array(crate::texel_array::ArrayHandle),
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
    crate::texel_array::ArrayHandle => Array,
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
    crate::texel_array::ArrayHandle => Array,
    wgpu::BindGroup => BindGroup,
}

/// Handle to a resource stored in a [`ResourceGraph`], typed by its kind.
///
/// `R` is the [`ResourceKind`] the id names, which is also what the graph hands
/// back for it: a [`ResourceId<wgpu::Buffer>`](ResourceId) resolves to a
/// buffer, a [`ResourceId<wgpu::TextureView>`](ResourceId) to a view. The
/// default, [`Resource`], is the *erased* kind — an id to a resource whose kind
/// is known only at runtime — which is what graph-wide reads like
/// [`ResourceGraph::dependencies`] hand out.
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
    /// [`ResourceGraph::dependencies`] speaks in, or a stored field a caller
    /// only ever passes back to the graph.
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
    /// Set when this resource or one of its dependencies was replaced, and
    /// cleared once [`ResourceGraph::maintain`] has run its recipe.
    dirty: bool,
    /// Whether the resource survives collection on its own rather than only
    /// through the resources built from it. See [Retention](self).
    strong: bool,
    /// How to build the resource again after one of its inputs was replaced,
    /// or `None` when the graph cannot rebuild it on its own.
    rebuild: Option<Rebuild>,
}

/// A recipe that builds a resource out of the graph it lives in.
///
/// A node that can be rebuilt is inserted with one —
/// [`ResourceGraph::insert_strong`] — and [`ResourceGraph::maintain`]
/// calls it whenever the node is dirty. The recipe reads whatever it was built
/// from back out of the graph by [`ResourceId`], so it always sees the current
/// handle of its inputs rather than a copy captured when it was written.
///
/// The closure runs while the graph is borrowed immutably, so it can only read
/// the graph: a recipe returns the handle it built and the graph writes it into
/// the node.
#[derive(Clone)]
pub struct Rebuild(Arc<dyn Fn(&ResourceGraph) -> Resource>);

impl Rebuild {
    /// Wrap `rebuild` as a reusable recipe.
    ///
    /// Only a resource that can be built purely from what the graph holds can
    /// be rebuilt lazily; the closure captures the device and the layout it
    /// needs on its own.
    pub fn new(rebuild: impl Fn(&ResourceGraph) -> Resource + 'static) -> Self {
        Self(Arc::new(rebuild))
    }

    /// Build the resource from the graph's current state.
    ///
    /// [`ResourceGraph::maintain`] calls this for a dirty node, but a caller
    /// can also use it to build the first instance eagerly, when it has to hold
    /// the resource before the next maintain.
    ///
    /// # Panics
    ///
    /// If the recipe builds a resource of a different kind than `R` names. A
    /// recipe only ever builds the kind its node stores, so this is a
    /// programming error.
    pub fn build<R: ResourceKind>(&self, graph: &ResourceGraph) -> R {
        ResourceKind::take_from((self.0)(graph))
            .expect("the recipe builds a resource of the kind its node stores")
    }
}

impl core::fmt::Debug for Rebuild {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The closure has no useful representation, so print the type alone.
        f.debug_struct("Rebuild").finish_non_exhaustive()
    }
}

/// A directed acyclic graph of wgpu resources.
///
/// Every edge points from a dependency to a resource built from it, so the
/// dependents of a node are exactly the nodes reachable from it. The graph
/// refuses an edge that would close a cycle, which is what lets
/// [`Self::maintain`] rebuild its dirty nodes in dependency order.
#[derive(Debug, Default)]
pub struct ResourceGraph {
    graph: Dag<Node>,
    /// Roots marked by [`Self::remove`], waiting for [`Self::maintain`] to drop
    /// them together with everything built from them.
    removed: Vec<NodeId>,
}

impl ResourceGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of resources in the graph.
    ///
    /// A resource marked by [`Self::remove`] is still counted until the next
    /// [`Self::maintain`] drops it.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    /// Whether the graph holds no resources.
    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    /// Add `resource` as a *strong* node that collection keeps whether or not
    /// anything depends on it.
    ///
    /// Use this for a resource the caller holds and uses directly. Use
    /// [`Self::insert_weak`] for one that exists only to feed a consumer.
    ///
    /// The kind of the returned id is the resource's own:
    /// `graph.insert_strong(buffer, None)` yields a buffer id, and
    /// `insert_strong(Resource::Virtual, None)` — or
    /// `insert_strong(Virtual, None)` — a [virtual](Virtual) one. The node is
    /// added at once; declare what it was built from with
    /// [`Self::add_dependency`].
    ///
    /// Pass a [`Rebuild`] recipe for a derived resource — a bind group, say —
    /// that [`Self::maintain`] rebuilds once one of its inputs is replaced. The
    /// recipe reads its inputs back out of the graph by id, so declare them
    /// with [`Self::add_dependency`] as well: the edges decide when to rebuild,
    /// and the recipe decides what to build from. `None` is a resource the
    /// graph cannot build again on its own.
    pub fn insert_strong<R: ResourceKind>(
        &mut self,
        resource: R,
        rebuild: Option<Rebuild>,
    ) -> ResourceId<R> {
        self.insert(resource, true, rebuild)
    }

    /// Add `resource` as a *weak* node that collection collects once nothing
    /// alive is built from it.
    ///
    /// Use this for a resource that only feeds a consumer — a uniform a bind
    /// group reads, an input to a derived resource — so that dropping the
    /// consumer also drops it. Use [`Self::insert_strong`] for a resource the
    /// caller holds itself.
    ///
    /// See [`Self::insert_strong`] for how the kind of the id follows from the
    /// resource, and for what the optional [`Rebuild`] recipe means. A weak
    /// node with a recipe is a derived resource nothing holds: it survives
    /// while a live node reads it, and is rebuilt on the same terms as a strong
    /// one while it does.
    pub fn insert_weak<R: ResourceKind>(
        &mut self,
        resource: R,
        rebuild: Option<Rebuild>,
    ) -> ResourceId<R> {
        self.insert(resource, false, rebuild)
    }

    fn insert<R: ResourceKind>(
        &mut self,
        resource: R,
        strong: bool,
        rebuild: Option<Rebuild>,
    ) -> ResourceId<R> {
        let node = self.graph.insert(Node {
            resource: resource.into_resource(),
            dirty: false,
            strong,
            rebuild,
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

    /// Mark `id` for removal, together with every resource transitively built
    /// from it.
    ///
    /// Nothing is dropped here: the walk that finds the subtree and the drop
    /// itself both happen once, in [`Self::maintain`]. Until then the ids keep
    /// resolving, so a caller that marks a resource and then reads it back sees
    /// what it marked rather than a freed slot.
    ///
    /// Marking the same subtree twice, or a resource inside an already-marked
    /// one, is harmless: [`Self::maintain`] drops each subtree once and a root
    /// that no longer resolves is skipped.
    ///
    /// An unknown id is ignored, so a caller may mark a resource it has already
    /// seen removed.
    pub fn remove<R>(&mut self, id: ResourceId<R>) {
        if self.graph.get(id.node).is_some() {
            self.removed.push(id.node);
        }
    }

    /// Bring the graph up to date: drop what was marked for removal, collect
    /// the resources nothing alive is built from, and rebuild what is dirty.
    ///
    /// Call this once per frame before reading the resources, so that every
    /// change made since the last call takes effect at a single point rather
    /// than at each change. A frame that changed nothing does no work beyond
    /// the liveness walk.
    ///
    /// The three passes run in a fixed order:
    ///
    /// 1. Each resource marked by [`Self::remove`] is dropped together with
    ///    everything built from it.
    /// 2. Every resource that no live node is built from — a weak node whose
    ///    consumers are gone, or one built only from dropped resources — is
    ///    collected.
    /// 3. Every dirty resource with a [recipe](Rebuild) is rebuilt, in
    ///    dependency order so that a rebuild sees its already-rebuilt inputs.
    ///
    /// Collecting before rebuilding means a resource that is about to be
    /// collected is never rebuilt, and a dirty node without a recipe stays
    /// dirty: the graph cannot build it again by itself.
    pub fn maintain(&mut self) {
        // `drain` empties the vector in place, so the roots a frame marks are
        // dropped here while the buffer holding them is kept for the next one.
        for root in self.removed.drain(..) {
            self.graph.remove_dependents_drop(root);
        }
        let stamp = self.graph.mark_dependencies_where(|node| node.strong);
        self.graph.remove_unmarked_drop(stamp);
        self.rebuild_dirty();
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

    /// Rebuild every dirty resource that has a recipe, in dependency order.
    ///
    /// Dependents are visited after their dependencies, so a rebuild observes
    /// already-updated inputs.
    fn rebuild_dirty(&mut self) {
        // The order is fixed up front, so the rebuilds cannot disturb the walk
        // they are part of even though a recipe runs between its steps. A
        // recipe only reads the graph, so the nodes it names are all still
        // there.
        for node in self.graph.topological_order() {
            let Some(recipe) = self
                .graph
                .get(node)
                .filter(|slot| slot.dirty)
                .and_then(|slot| slot.rebuild.clone())
            else {
                continue;
            };
            let rebuilt = recipe.build::<Resource>(self);
            let Some(slot) = self.graph.get_mut(node) else {
                continue;
            };
            debug_assert!(
                core::mem::discriminant(&slot.resource) == core::mem::discriminant(&rebuilt),
                "a rebuilt resource must be of the same kind as the one it replaces"
            );
            slot.resource = rebuilt;
            slot.dirty = false;
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
    use std::cell::RefCell;
    use std::rc::Rc;

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
        graph.insert_strong(buffer(device, label), None)
    }

    /// The id a [`strong_buffer`] hands back: a buffer id, spelled out so the
    /// tests do not have to infer it.
    type BufferId = ResourceId<wgpu::Buffer>;

    /// Insert a buffer whose recipe records that it ran, under `name`, and
    /// returns a fresh buffer.
    ///
    /// The names accumulate in `order`, which is how a test observes both which
    /// recipes ran and in what order.
    fn rebuildable_buffer(
        graph: &mut ResourceGraph,
        device: &wgpu::Device,
        order: &Rc<RefCell<Vec<&'static str>>>,
        name: &'static str,
    ) -> BufferId {
        let order = Rc::clone(order);
        let device = device.clone();
        graph.insert_strong(
            buffer(&device, name),
            Some(Rebuild::new(move |_graph| {
                order.borrow_mut().push(name);
                buffer(&device, "rebuilt").into()
            })),
        )
    }

    #[test]
    fn maintain_rebuilds_the_dependents_of_a_replaced_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let order = Rc::new(RefCell::new(Vec::new()));
        let middle = rebuildable_buffer(&mut graph, &device, &order, "middle");
        graph.add_dependency(middle, base);
        let leaf = rebuildable_buffer(&mut graph, &device, &order, "leaf");
        graph.add_dependency(leaf, middle);

        graph.replace(base, buffer(&device, "base2"));
        graph.maintain();

        // Dependency order: the middle buffer was rebuilt before the leaf.
        assert_eq!(*order.borrow(), vec!["middle", "leaf"]);
    }

    #[test]
    fn maintain_rebuilds_only_the_dependents_of_the_replaced_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = strong_buffer(&mut graph, &device, "a");
        let b = strong_buffer(&mut graph, &device, "b");
        let order = Rc::new(RefCell::new(Vec::new()));
        let a_dependent = rebuildable_buffer(&mut graph, &device, &order, "a");
        graph.add_dependency(a_dependent, a);
        let b_dependent = rebuildable_buffer(&mut graph, &device, &order, "b");
        graph.add_dependency(b_dependent, b);

        graph.replace(a, buffer(&device, "a2"));
        graph.maintain();

        assert_eq!(*order.borrow(), vec!["a"]);
    }

    /// A dirty node with no recipe is left alone rather than preventing the
    /// rebuild of the nodes built from it.
    #[test]
    fn maintain_leaves_a_dirty_node_without_a_recipe_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let order = Rc::new(RefCell::new(Vec::new()));
        let dependent = rebuildable_buffer(&mut graph, &device, &order, "dependent");
        graph.add_dependency(dependent, base);

        graph.replace(base, buffer(&device, "base2"));
        graph.maintain();
        graph.maintain();

        // The dependent is rebuilt once; the recipe-less base stays dirty and
        // is skipped on every later pass.
        assert_eq!(*order.borrow(), vec!["dependent"]);
    }

    /// The old handle is handed back in the id's own kind, so a caller
    /// replacing a buffer never sees another kind of resource.
    #[test]
    fn replace_returns_the_previous_handle() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(sized_buffer(&device, 64), None);

        let previous = graph.replace(id, sized_buffer(&device, 128)).unwrap();
        assert_eq!(previous.size(), 64);
        assert_eq!(graph.get(id).unwrap().size(), 128);
    }

    #[test]
    fn remove_marks_a_subtree_that_maintain_drops() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"), None);
        graph.add_dependency(dependent, base);
        let unrelated = strong_buffer(&mut graph, &device, "unrelated");

        graph.remove(base);
        // Marking drops nothing, so both marked nodes still resolve.
        assert!(graph.get(base).is_some());
        assert!(graph.get(dependent).is_some());

        graph.maintain();
        assert!(graph.get(base).is_none());
        assert!(graph.get(dependent).is_none());
        assert!(graph.get(unrelated).is_some());
        assert_eq!(graph.len(), 1);
    }

    /// A root marked twice, or marked inside an already-marked subtree, drops
    /// the subtree once.
    #[test]
    fn maintain_drops_an_overlapping_removal_once() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"), None);
        graph.add_dependency(dependent, base);

        graph.remove(base);
        graph.remove(dependent);
        graph.remove(base);
        graph.maintain();

        assert!(graph.is_empty());
    }

    #[test]
    #[should_panic(expected = "is not in the graph")]
    fn add_dependency_rejects_a_removed_dependency() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let dependent = graph.insert_strong(buffer(&device, "dependent"), None);
        let stale = graph.insert_strong(buffer(&device, "stale"), None);
        graph.remove(stale);
        graph.maintain();
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
        graph.remove(id);
        graph.maintain();
        // The freed slot is handed straight back to the next node, so a stale
        // handle and the live one share an index. The id also records the
        // generation it was handed out with, so the two are still told apart:
        // the stale id resolves to nothing rather than to the node that took
        // its slot.
        let dependent = graph.insert_strong(buffer(&device, "b"), None);
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
        graph.remove(stale);
        graph.maintain();

        let fresh = strong_buffer(&mut graph, &device, "fresh");
        assert_eq!(fresh.index(), stale.index(), "the slot is reused");
        assert!(graph.get(fresh).is_some());
        assert!(
            graph.get(stale).is_none(),
            "the stale id resolves to nothing"
        );
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
        let dependent = graph.insert_strong(buffer(&device, "b"), None);
        graph.remove(dependent);
        graph.maintain();

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
        let b = graph.insert_strong(buffer(&device, "b"), None);
        graph.add_dependency(b, a);
        // `a` already reaches `b`, so the reverse edge would close a cycle.
        graph.add_dependency(a, b);
    }

    #[test]
    fn maintain_keeps_strong_nodes_with_no_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let strong = strong_buffer(&mut graph, &device, "strong");

        graph.maintain();
        assert!(graph.get(strong).is_some());
    }

    #[test]
    fn maintain_collects_a_weak_node_nothing_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"), None);

        graph.maintain();
        assert!(graph.get(weak).is_none());
        assert!(graph.is_empty());
    }

    #[test]
    fn maintain_keeps_a_weak_node_a_strong_dependent_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"), None);
        let dependent = graph.insert_strong(buffer(&device, "dependent"), None);
        graph.add_dependency(dependent, weak);

        graph.maintain();
        assert!(graph.get(weak).is_some());
        assert!(graph.get(dependent).is_some());
    }

    /// The case the graph exists for: a uniform feeding a bind group is
    /// reached by no removal walk from the group's inputs, so only the orphan
    /// pass collects it.
    #[test]
    fn maintain_collects_a_weak_node_orphaned_by_removing_its_dependent() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "uniform"), None);
        let input = strong_buffer(&mut graph, &device, "input");
        let group = graph.insert_strong(buffer(&device, "group"), None);
        graph.add_dependency(group, input);
        graph.add_dependency(group, weak);

        graph.remove(group);
        graph.maintain();
        assert!(graph.get(weak).is_none());
        assert!(
            graph.get(input).is_some(),
            "a strong node outlives the group"
        );
    }

    #[test]
    fn maintain_collects_only_what_is_unreachable_from_a_strong_node() {
        let device = device();
        let mut graph = ResourceGraph::new();
        // A weak chain feeding a strong node: every link is kept, because the
        // strong node was built from it.
        let leaf = graph.insert_weak(buffer(&device, "leaf"), None);
        let middle = graph.insert_weak(buffer(&device, "middle"), None);
        graph.add_dependency(middle, leaf);
        let root = graph.insert_strong(buffer(&device, "root"), None);
        graph.add_dependency(root, middle);
        // A second chain whose strong node is removed: the whole chain goes.
        let gone_leaf = graph.insert_weak(buffer(&device, "gone_leaf"), None);
        let gone_middle = graph.insert_weak(buffer(&device, "gone_middle"), None);
        graph.add_dependency(gone_middle, gone_leaf);
        let gone_root = graph.insert_strong(buffer(&device, "gone_root"), None);
        graph.add_dependency(gone_root, gone_middle);

        graph.remove(gone_root);
        graph.maintain();

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

        graph.remove(leaf);
        graph.maintain();
        assert!(graph.get(leaf).is_none());
        assert!(graph.get(other).is_some());
        assert_eq!(graph.len(), 1);
    }

    /// Removing a resource inside a chain drops everything built from it and
    /// leaves the resource it was built from alive.
    #[test]
    fn remove_drops_the_dependents_of_the_marked_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(sized_buffer(&device, 64), None);
        let middle = graph.insert_strong(sized_buffer(&device, 128), None);
        graph.add_dependency(middle, base);
        let root = graph.insert_strong(sized_buffer(&device, 256), None);
        graph.add_dependency(root, middle);

        graph.remove(middle);
        graph.maintain();
        assert!(graph.get(base).is_some());
        assert!(graph.get(middle).is_none());
        assert!(graph.get(root).is_none());
        assert_eq!(graph.len(), 1);
    }

    /// A recipe reads its inputs back out of the graph, so it sees the handles
    /// the graph holds when it runs — the replaced ones, not the originals.
    #[test]
    fn rebuild_observes_the_updated_handles_of_its_dependencies() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = strong_buffer(&mut graph, &device, "base");
        let observed = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&observed);
        let rebuilt = device.clone();
        let dependent = graph.insert_strong(
            buffer(&device, "dependent"),
            Some(Rebuild::new(move |graph| {
                let size = graph
                    .get(base)
                    .expect("the dependency outlives the rebuild")
                    .size();
                seen.borrow_mut().push(size);
                buffer(&rebuilt, "rebuilt").into()
            })),
        );
        graph.add_dependency(dependent, base);

        graph.replace(base, sized_buffer(&device, 128));
        graph.maintain();

        assert_eq!(*observed.borrow(), vec![128]);
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
        let order = Rc::new(RefCell::new(Vec::new()));
        let base = rebuildable_buffer(&mut graph, &device, &order, "base");
        let left = rebuildable_buffer(&mut graph, &device, &order, "left");
        graph.add_dependency(left, base);
        let right = rebuildable_buffer(&mut graph, &device, &order, "right");
        graph.add_dependency(right, base);
        let join = rebuildable_buffer(&mut graph, &device, &order, "join");
        graph.add_dependency(join, left);
        graph.add_dependency(join, right);

        graph.replace(base, buffer(&device, "base2"));
        graph.maintain();

        // The base and both middle nodes are rebuilt before the join, which
        // observes both of them.
        let order = order.borrow().clone();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], "base");
        assert_eq!(order[3], "join");
    }

    // -- kinds -------------------------------------------------------------

    /// A typed id resolves to the resource itself, without the caller
    /// knowing which variant the node holds.
    #[test]
    fn a_typed_id_resolves_to_its_own_kind() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(sized_buffer(&device, 64), None);

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
        graph.replace(id, sized_buffer(&device, 128));
        let Resource::Buffer(buffer) = graph.get(erased).expect("the erased id still resolves")
        else {
            panic!("the node holds a buffer");
        };
        assert_eq!(
            buffer.size(),
            128,
            "the erased id reads the node's new resource"
        );
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
        let default = graph.insert_strong(
            TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        assert_eq!(
            graph.get(default).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8Unorm)
        );

        let srgb = graph.insert_strong(
            TextureExt::create_view(
                &texture,
                &wgpu::TextureViewDescriptor {
                    format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                    ..Default::default()
                },
            ),
            None,
        );
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
        let id = graph.insert_strong(
            TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
            None,
        );

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
    }

    // -- virtual nodes -----------------------------------------------------

    /// The aggregation-root pattern: parts are weak and dependency-free, the
    /// root is strong and built from them, so liveness runs from the root to
    /// the parts and the orphan pass collects nothing.
    #[test]
    fn a_virtual_root_keeps_its_weak_parts_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"), None);
        let root = graph.insert_strong(Virtual, None);
        graph.add_dependency(root, part);

        graph.maintain();
        assert!(graph.get(root).is_some());
        assert!(graph.get(part).is_some());
    }

    /// Removing a virtual root drops it, and the parts it was the only live
    /// consumer of are collected by the same `maintain`.
    #[test]
    fn removing_a_virtual_root_orphans_its_parts() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"), None);
        let root = graph.insert_strong(Virtual, None);
        graph.add_dependency(root, part);

        graph.remove(root);
        graph.maintain();
        assert!(graph.get(part).is_none());
        assert!(graph.is_empty());
    }

    /// A part two roots are built from outlives either root alone: the orphan
    /// pass keeps it while the other root is still alive.
    #[test]
    fn a_shared_part_survives_while_another_root_is_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"), None);
        let first = graph.insert_strong(Virtual, None);
        graph.add_dependency(first, part);
        let second = graph.insert_strong(Virtual, None);
        graph.add_dependency(second, part);

        graph.remove(first);
        graph.maintain();
        assert!(
            graph.get(part).is_some(),
            "the surviving root still owns the shared part"
        );
        assert!(graph.get(second).is_some());

        graph.remove(second);
        graph.maintain();
        assert!(graph.get(part).is_none());
        assert!(graph.is_empty());
    }

    /// Replacing a part marks the virtual root built from it dirty, and the
    /// root's recipe follows its part in dependency order.
    #[test]
    fn replacing_a_part_rebuilds_the_virtual_root() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert_weak(buffer(&device, "part"), None);
        let order = Rc::new(RefCell::new(Vec::new()));
        let root_order = Rc::clone(&order);
        let root = graph.insert_strong(
            Virtual,
            Some(Rebuild::new(move |_graph| {
                root_order.borrow_mut().push("root");
                Resource::Virtual
            })),
        );
        graph.add_dependency(root, part);

        graph.replace(part, buffer(&device, "part2"));
        graph.maintain();
        assert_eq!(*order.borrow(), vec!["root"]);
    }

    /// A virtual node is a graph citizen with no handle: it resolves to
    /// [`Virtual`] and to nothing else.
    #[test]
    fn a_virtual_node_has_no_handle_of_its_own() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert_strong(Virtual, None);

        assert!(matches!(graph.get(root), Some(Virtual)));
    }

    /// A weak virtual node is collected like any other weak node once nothing
    /// alive is built from it.
    #[test]
    fn maintain_collects_a_weak_virtual_node_with_no_dependents() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert_weak(Virtual, None);

        graph.maintain();
        assert!(graph.get(root).is_none());
        assert!(graph.is_empty());
    }

    /// A node keeps its kind: replacing a virtual node hands back the previous
    /// virtual value, and the node stays a root its parts hang from.
    #[test]
    fn a_virtual_node_can_be_replaced() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let root = graph.insert_weak(Virtual, None);
        let dependent = graph.insert_strong(buffer(&device, "dependent"), None);
        graph.add_dependency(dependent, root);

        let previous = graph
            .replace(root, Virtual)
            .expect("the node is in the graph");
        assert_eq!(previous, Virtual);
        assert!(matches!(graph.get(root), Some(Virtual)));
        assert_eq!(
            graph.dependencies(dependent).collect::<Vec<_>>(),
            vec![root.erase()]
        );
    }
}
