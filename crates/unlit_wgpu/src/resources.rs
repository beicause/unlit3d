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
//! * Dropping the last [`ResHandle`] to a resource makes it collectable.
//! * [`ResourceGraph::maintain`] collects the resources no id and no dependent
//!   holds any more, and rebuilds the resources marked dirty.
//!
//! Nothing happens at the point of the change: `replace` only flips dirty
//! flags, and taking or dropping an id only moves a reference count. Both take
//! effect the next time [`ResourceGraph::maintain`] runs, which is where a
//! frame calls it exactly once — before the resources are read.
//!
//! Updates are therefore lazy and precise: uploading new bytes into an
//! existing buffer does not dirty anything, reallocating it does, and only the
//! resources that actually consumed the old handle are affected.
//!
//! # Rebuilding
//!
//! A resource that can be rebuilt is inserted with a *recipe*:
//! [`insert`](ResourceGraph::insert) takes an optional
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
//! A [`ResHandle`] is typed by the [`ResourceKind`] it names, and the kind is
//! the resource itself: [`ResHandle<wgpu::Buffer>`](ResHandle) resolves to a
//! buffer, [`ResHandle<TextureView>`](ResHandle) to a texture view together
//! with its format, and [`ResHandle<Virtual>`](ResHandle) to a node that
//! holds no handle.
//! [`ResourceGraph::get`] hands that resource back directly, so no caller
//! matches on which variant a node holds and a texture view cannot be read
//! where a buffer is wanted.
//!
//! The kind is a compile-time claim rather than runtime state: the graph stores
//! every resource in one [`Resource`] enum, and a typed id is an index that
//! remembers what it names. [Insertion](ResourceGraph::insert) derives the
//! kind from the resource it is given and
//! [`replace`](ResourceGraph::replace) requires the replacement to have the
//! same kind, so an id never names a resource of another kind. Where the kind
//! is genuinely unknown at compile time — a node's dependencies, a dirty
//! node — the [erased](ResHandle::erase) `ResHandle<Resource>` names it
//! instead.
//!
//! # Retention
//!
//! An id names a strong reference to its resource: cloning one takes another
//! reference, dropping one gives it up, and [`ResourceGraph::maintain`]
//! collects the resource once its last reference is gone. There is no removal
//! call and no handle that goes stale — a resource lives exactly while some id
//! to it does, and letting go of the last id is all it takes to free it.
//!
//! A resource built from another holds a reference to it too: recording an edge
//! with [`add_dependency`](ResourceGraph::add_dependency) takes one reference
//! from the dependency on behalf of the dependent, so a dependency counts its
//! dependents among the references that keep it alive. A resource that exists
//! only to feed a consumer therefore lives exactly while some consumer does,
//! and collecting a consumer releases whatever it was the last holder of in the
//! same pass. Declaring the same edge twice records it once, and so counts
//! once.
//!
//! Insertion is immediate: `insert` adds the node and returns its id, and
//! [`add_dependency`](ResourceGraph::add_dependency) records an edge. Nothing
//! defers, so there is no insertion state to finish and no failure to report —
//! an edge that would close a cycle is a bug in the caller and panics where it
//! is declared.
//!
//! A [`Virtual`] node commonly serves as an *aggregation root* for a group of
//! resources: the root is built from every part, so the root is the group's
//! single lifetime entry point. Dropping the last id to the root releases the
//! parts the root was the last holder of, and [`ResourceGraph::maintain`]
//! collects them. A part shared by two roots therefore outlives either one
//! alone.

use std::sync::{Arc, Weak};

use crate::dag::{Dag, EdgeError, NodeId};

/// A kind of resource that can live in a [`ResourceGraph`], and the resource an
/// id of that kind resolves to.
///
/// Implemented for the wgpu handles the graph stores and for [`Virtual`], the
/// kind that holds no handle. The implementor is not just a tag: it is the
/// payload [`ResourceGraph::get`] hands out, so the kind and the resource read
/// as one thing — a `ResHandle<wgpu::Buffer>` names a buffer, not a mark
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
/// [`ResHandle<Virtual>`](ResHandle) names such a node. It is an ordinary
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
/// [`ResHandle<TextureView>`](ResHandle) names, and
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
/// known at runtime: it is the [erased](ResHandle::erase) [`ResHandle`]'s
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

impl Resource {
    /// The name of this resource's kind.
    ///
    /// The stored form erases the kind, so this is how a caller that only has a
    /// [`Resource`] — a graph-wide read, a debug dump — reports which of the
    /// stored kinds it is looking at. The name matches the [`ResourceKind`]
    /// type it stands for: `"Buffer"`, `"TextureView"`, `"Virtual"`, and so
    /// on.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Buffer(_) => "Buffer",
            Self::Texture(_) => "Texture",
            Self::TextureView(_) => "TextureView",
            Self::Sampler(_) => "Sampler",
            Self::Array(_) => "Array",
            Self::BindGroup(_) => "BindGroup",
            Self::Virtual => "Virtual",
        }
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
/// back for it: a [`ResHandle<wgpu::Buffer>`](ResHandle) resolves to a
/// buffer, a [`ResHandle<wgpu::TextureView>`](ResHandle) to a view. The
/// default, [`Resource`], is the *erased* kind — an id to a resource whose kind
/// is known only at runtime — which is what graph-wide reads like
/// [`ResourceGraph::dependencies`] hand out.
///
/// An id names a strong reference to its resource: cloning one takes another
/// reference, dropping one gives it up, and
/// [`ResourceGraph::maintain`] collects the resource once the last reference is
/// gone. That is what makes an id safe to hold — the resource it names outlives
/// every id to it, and a caller that drops its last id lets the graph free the
/// resource without any further call.
///
/// An id therefore resolves for as long as the caller holds it: the graph never
/// collects a resource some id still names. Letting go of the last id is the
/// only way to give a resource up, and it takes effect at the next
/// [`ResourceGraph::maintain`].
///
/// An id is not [`Copy`]: cloning one adds a reference, which a copy could not
/// tell apart from a move. Pass one to a graph method by reference.
pub struct ResHandle<R = Resource> {
    node: NodeId,
    /// The strong reference this id carries. Its liveness is the token's own
    /// reference count.
    strong: StrongRef,
    kind: core::marker::PhantomData<fn() -> R>,
}

impl<R> ResHandle<R> {
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
    /// erased [`ResHandle<Resource>`](Resource) that
    /// [`ResourceGraph::dependencies`] speaks in, or a stored field a caller
    /// only ever passes back to the graph. The kind is the only thing that
    /// changes: a strong id stays strong through the call, so the erased id
    /// names the same reference and both ids keep the resource alive.
    pub fn erase(&self) -> ResHandle {
        self.shared::<Resource>()
    }
}

impl<R> ResHandle<R> {
    /// A second id to the same resource, taking one more strong reference to it.
    ///
    /// The new id may name any kind: the node is what both ids resolve
    /// through, and the kind only says which accessor fits. Both [`Clone`] and
    /// [`Self::erase`] go through here so that a new id always counts,
    /// whichever way it was made.
    fn shared<K>(&self) -> ResHandle<K> {
        ResHandle {
            node: self.node,
            strong: self.strong.clone(),
            kind: core::marker::PhantomData,
        }
    }
}

// The trait impls are written out rather than derived: a derived bound would
// ask `R` to be comparable, hashable or cloneable, none of which a kind has to
// be. An id is a node handle, and only the handle takes part in any of them.
impl<R> Clone for ResHandle<R> {
    fn clone(&self) -> Self {
        self.shared::<R>()
    }
}

impl<R> PartialEq for ResHandle<R> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<R> Eq for ResHandle<R> {}

impl<R> PartialOrd for ResHandle<R> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for ResHandle<R> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.node.cmp(&other.node)
    }
}

impl<R> core::hash::Hash for ResHandle<R> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.node.hash(state);
    }
}

impl<R> core::fmt::Debug for ResHandle<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ResHandle({})", self.node.index())
    }
}

/// The strong reference count of one node is the number of [`ResHandle`]s that
/// name it plus the number of nodes built from it, which is exactly what
/// [`Arc`] already counts.
///
/// Every node holds a [`Weak`] handle to the [`Arc<()>`](Arc) token its ids
/// carry, so cloning an id clones the token — one more strong reference — and
/// dropping it releases the token, with no counter of our own to keep. A
/// dependent holds one more clone on behalf of each dependency it declared, so
/// each of a resource's dependents counts towards keeping it alive. Cloning the
/// [`Weak`] in the node takes no reference, so [`ResourceGraph::maintain`] can
/// read the live count back without keeping the node alive itself.
///
/// The token is an [`Arc`] and not an `Rc` because an id may cross to another
/// thread even though the graph it names does not: an [`Arc`] makes every id
/// `Send` and `Sync`, so one can be handed to a worker while the graph stays
/// where it is. The graph itself is not `Send`: it holds wgpu handles and runs
/// its recipes, so it stays on the one thread that owns the world it sits in.
type StrongRef = Arc<()>;
type StrongRefs = Weak<()>;

#[derive(Debug)]
struct Node {
    resource: Resource,
    /// Set when this resource or one of its dependencies was replaced, and
    /// cleared once [`ResourceGraph::maintain`] has run its recipe.
    dirty: bool,
    /// A handle to the strong references this node's ids carry. Its count is
    /// the node's liveness: [`ResourceGraph::maintain`] collects the node once
    /// nothing holds it any more. See [Retention](self).
    strong: StrongRefs,
    /// The strong references this node takes on the resources it was built
    /// from, one per declared dependency, so each of a resource's dependents
    /// counts towards keeping it alive. Dropping the node releases them.
    held: Vec<StrongRef>,
    /// How to build the resource again after one of its inputs was replaced,
    /// or `None` when the graph cannot rebuild it on its own.
    rebuild: Option<Rebuild>,
}

/// A recipe that builds a resource out of the graph it lives in.
///
/// A node that can be rebuilt is inserted with one —
/// [`ResourceGraph::insert`] — and [`ResourceGraph::maintain`]
/// calls it whenever the node is dirty. The recipe reads whatever it was built
/// from back out of the graph by [`ResHandle`], so it always sees the current
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

/// One resource as [`ResourceGraph::nodes`] reports it.
///
/// A borrowed view rather than an owned handle: the index names the node slot
/// and can be resolved back with [`ResourceGraph::id_at`], while the resource
/// itself is borrowed from the graph for as long as the walk runs.
#[derive(Debug)]
pub struct NodeInfo<'a> {
    /// The node's slot index, the handle [`ResHandle::index`] reports.
    pub index: usize,
    /// The stored resource.
    pub resource: &'a Resource,
    /// Whether this resource or one of its dependencies was replaced, and so
    /// whether [`ResourceGraph::maintain`] still has a recipe to run.
    pub dirty: bool,
    /// Whether the graph can rebuild this resource from its inputs on its own.
    pub rebuildable: bool,
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
}

impl ResourceGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of resources in the graph.
    ///
    /// A resource no id holds any more is still counted until the next
    /// [`Self::maintain`] collects it.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    /// Whether the graph holds no resources.
    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    /// Every resource in the graph, in node-slot order.
    ///
    /// The node's slot [`index`](ResHandle::index) is the handle: it is stable
    /// while the resource lives, so a caller can report it, sort by it, and
    /// resolve it back with [`Self::id_at`]. The walk covers every slot the
    /// graph has ever used, so it costs the peak node count rather than the
    /// current one.
    ///
    /// A resource no id holds any more is still listed until the next
    /// [`Self::maintain`] collects it, exactly as [`Self::len`] counts it.
    pub fn nodes(&self) -> impl Iterator<Item = NodeInfo<'_>> {
        self.graph.iter().map(|(id, node)| NodeInfo {
            index: id.index(),
            resource: &node.resource,
            dirty: node.dirty,
            rebuildable: node.rebuild.is_some(),
        })
    }

    /// The id of the resource in node slot `index`, or `None` when no resource
    /// is there or nothing holds it any more.
    ///
    /// The id is erased — its kind is only known from the resource — and holds
    /// one strong reference, so resolving an index keeps the resource alive
    /// like any other id does. A resource nothing holds is on its way out at
    /// the next [`Self::maintain`] and cannot be named again, which is why the
    /// call reports `None` rather than inventing a reference to it.
    #[must_use]
    pub fn id_at(&self, index: usize) -> Option<ResHandle> {
        let id = self.graph.id_at(index)?;
        let strong = self.graph.get(id)?.strong.upgrade()?;
        Some(ResHandle {
            node: id,
            strong,
            kind: core::marker::PhantomData,
        })
    }

    /// Add `resource` and return an id holding one strong reference to it.
    ///
    /// The resource lives while any id to it does, and while any node built
    /// from it does; [`Self::maintain`] collects it once both are gone. So a
    /// caller that keeps the returned id keeps the resource, and one that drops
    /// it — without a dependent — gives the resource up.
    ///
    /// The kind of the returned id is the resource's own:
    /// `graph.insert(buffer, None)` yields a buffer id, and
    /// `insert(Virtual, None)` a [virtual](Virtual) one. The node is added at
    /// once; declare what it was built from with [`Self::add_dependency`].
    ///
    /// Pass a [`Rebuild`] recipe for a derived resource — a bind group, say —
    /// that [`Self::maintain`] rebuilds once one of its inputs is replaced. The
    /// recipe reads its inputs back out of the graph by id, so declare them
    /// with [`Self::add_dependency`] as well: the edges decide when to rebuild,
    /// and the recipe decides what to build from. `None` is a resource the
    /// graph cannot build again on its own.
    pub fn insert<R: ResourceKind>(
        &mut self,
        resource: R,
        rebuild: Option<Rebuild>,
    ) -> ResHandle<R> {
        let strong = Arc::new(());
        let node = self.graph.insert(Node {
            resource: resource.into_resource(),
            dirty: false,
            strong: Arc::downgrade(&strong),
            held: Vec::new(),
            rebuild,
        });
        ResHandle {
            node,
            strong,
            kind: core::marker::PhantomData,
        }
    }

    /// Record that the resource behind `dependent` was built from the one
    /// behind `dependency`, so replacing or rebuilding the latter reaches the
    /// former.
    ///
    /// The dependency may be of any kind: one bind group is built from buffers,
    /// a view and a sampler all at once. Call this once per input; declaring
    /// the same pair twice records one edge, since a node depends on another
    /// once or not at all.
    ///
    /// The dependent holds one strong reference to the dependency on behalf of
    /// the edge, so a resource counts its dependents among the references that
    /// keep it alive: a resource nothing else holds lives exactly while
    /// something is built from it. Declaring the same edge twice records it
    /// once, and so takes one reference, not two.
    ///
    /// An optional input is the caller's to skip — `if let Some(id) = ...`
    /// around the call, which reads the way the id itself does — rather than
    /// something this method silently ignores: an absent dependency is a
    /// decision about the caller's graph, not an edge.
    ///
    /// # Panics
    ///
    /// Panics if the edge would close a cycle. A cycle means the declared
    /// dependencies are not a build order at all, so it is refused where it is
    /// declared rather than corrupting a later walk.
    pub fn add_dependency<D, R>(&mut self, dependent: &ResHandle<R>, dependency: &ResHandle<D>) {
        match self.graph.add_edge(dependency.node, dependent.node) {
            Ok(false) => {}
            Ok(true) => {
                let slot = self
                    .graph
                    .get_mut(dependent.node)
                    .expect("the edge was recorded, so the dependent is in the graph");
                slot.held.push(dependency.strong.clone());
            }
            Err(EdgeError::NoSuchNode(_)) => panic!(
                "add_dependency: {dependent:?} cannot depend on {dependency:?}, \
                 because one of the two is not in the graph"
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
    /// sampler or bind group itself; the [erased](ResHandle::erase)
    /// `ResHandle<Resource>` is how a caller reads a resource whose kind it
    /// does not know. `None` when the id is unknown — nothing else can make it
    /// fail, since an id only ever names a resource of its own kind.
    pub fn get<R: ResourceKind>(&self, id: &ResHandle<R>) -> Option<&R> {
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
        id: &ResHandle<R>,
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

    /// Bring the graph up to date: collect the resources nothing holds any
    /// more, and rebuild what is dirty.
    ///
    /// Call this once per frame before reading the resources, so that every
    /// change made since the last call takes effect at a single point rather
    /// than at each change.
    ///
    /// A resource is collected once nothing holds it: no [`ResHandle`] names
    /// it and no node is built from it. Dropping the last id to a resource — or
    /// to a node built from it — is what gives the resource up, and this pass
    /// is where that takes effect. There is no removal call: a resource some id
    /// or some dependent still holds stays, and one nothing holds goes.
    ///
    /// Collecting a node releases the references it held on its own
    /// dependencies, which may leave one of those with nothing holding it
    /// either. The sweep therefore repeats until a pass collects nothing, so a
    /// chain of resources that only a dropped consumer kept alive goes in one
    /// call.
    ///
    /// The passes run in a fixed order:
    ///
    /// 1. Every resource nothing holds is collected, repeating until nothing
    ///    more is.
    /// 2. Every dirty resource with a [recipe](Rebuild) is rebuilt, in
    ///    dependency order so that a rebuild sees its already-rebuilt inputs.
    ///
    /// Collecting before rebuilding means a resource that is about to be
    /// collected is never rebuilt, and a dirty node without a recipe stays
    /// dirty: the graph cannot build it again by itself.
    pub fn maintain(&mut self) {
        while self
            .graph
            .remove_where_drop(|node| node.strong.strong_count() == 0)
            > 0
        {}
        self.rebuild_dirty();
    }

    /// The immediate dependencies recorded for `id`.
    ///
    /// A node's inputs may be of any kind, so the ids come back
    /// [erased](ResHandle::erase). Each id is a strong reference in its own
    /// right: holding one keeps the input alive, exactly as holding any other
    /// id does.
    pub fn dependencies<R>(&self, id: &ResHandle<R>) -> impl Iterator<Item = ResHandle> + '_ {
        self.graph
            .dependencies(id.node)
            .map(|node| self.id_for(node))
    }

    /// An id to `node`, taking one more strong reference to it.
    ///
    /// The caller asks about the dependency of a node it holds an id to, so the
    /// dependent's own reference to that dependency is still in place and the
    /// upgrade cannot fail.
    fn id_for(&self, node: NodeId) -> ResHandle {
        let strong = self
            .graph
            .get(node)
            .and_then(|slot| Weak::upgrade(&slot.strong))
            .expect("a dependency of a live node is still held by that node");
        ResHandle {
            node,
            strong,
            kind: core::marker::PhantomData,
        }
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

    /// Insert a buffer and hand back the id that keeps it alive.
    fn buffer_id(graph: &mut ResourceGraph, device: &wgpu::Device, label: &str) -> BufferId {
        graph.insert(buffer(device, label), None)
    }

    /// The id a [`buffer_id`] hands back: a buffer id, spelled out so the
    /// tests do not have to infer it.
    type BufferId = ResHandle<wgpu::Buffer>;

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
        graph.insert(
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
        let base = buffer_id(&mut graph, &device, "base");
        let order = Rc::new(RefCell::new(Vec::new()));
        let middle = rebuildable_buffer(&mut graph, &device, &order, "middle");
        graph.add_dependency(&middle, &base);
        let leaf = rebuildable_buffer(&mut graph, &device, &order, "leaf");
        graph.add_dependency(&leaf, &middle);

        graph.replace(&base, buffer(&device, "base2"));
        graph.maintain();

        // Dependency order: the middle buffer was rebuilt before the leaf.
        assert_eq!(*order.borrow(), vec!["middle", "leaf"]);
    }

    #[test]
    fn maintain_rebuilds_only_the_dependents_of_the_replaced_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = buffer_id(&mut graph, &device, "a");
        let b = buffer_id(&mut graph, &device, "b");
        let order = Rc::new(RefCell::new(Vec::new()));
        let a_dependent = rebuildable_buffer(&mut graph, &device, &order, "a");
        graph.add_dependency(&a_dependent, &a);
        let b_dependent = rebuildable_buffer(&mut graph, &device, &order, "b");
        graph.add_dependency(&b_dependent, &b);

        graph.replace(&a, buffer(&device, "a2"));
        graph.maintain();

        assert_eq!(*order.borrow(), vec!["a"]);
    }

    /// A dirty node with no recipe is left alone rather than preventing the
    /// rebuild of the nodes built from it.
    #[test]
    fn maintain_leaves_a_dirty_node_without_a_recipe_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = buffer_id(&mut graph, &device, "base");
        let order = Rc::new(RefCell::new(Vec::new()));
        let dependent = rebuildable_buffer(&mut graph, &device, &order, "dependent");
        graph.add_dependency(&dependent, &base);

        graph.replace(&base, buffer(&device, "base2"));
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
        let id = graph.insert(sized_buffer(&device, 64), None);

        let previous = graph.replace(&id, sized_buffer(&device, 128)).unwrap();
        assert_eq!(previous.size(), 64);
        assert_eq!(graph.get(&id).unwrap().size(), 128);
    }

    /// A resource nothing names any more is collected by the next `maintain`,
    /// with no removal call in between.
    #[test]
    fn dropping_the_last_id_makes_a_resource_collectable() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = buffer_id(&mut graph, &device, "gone");

        graph.maintain();
        assert!(graph.get(&id).is_some(), "the id still names it");

        drop(id);
        graph.maintain();
        assert!(graph.is_empty(), "the last reference went with the id");
    }

    /// A resource a node is built from outlives the id it was inserted with:
    /// the dependent's edge holds a reference of its own.
    #[test]
    fn a_dependent_keeps_a_resource_alive_after_its_id_is_dropped() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let dependent = graph.insert(buffer(&device, "dependent"), None);
        graph.add_dependency(&dependent, &part);

        drop(part);
        graph.maintain();
        assert_eq!(graph.len(), 2, "the dependent still holds the part");
    }

    /// The case the graph exists for: dropping a consumer frees the resource it
    /// was the last holder of, in the same `maintain`, even though no id to
    /// that resource was ever dropped.
    #[test]
    fn collecting_a_consumer_releases_the_dependency_it_was_the_last_holder_of() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let uniform = graph.insert(buffer(&device, "uniform"), None);
        let group = graph.insert(buffer(&device, "group"), None);
        graph.add_dependency(&group, &uniform);

        drop(group);
        drop(uniform);
        graph.maintain();
        assert!(graph.is_empty(), "both went in one pass");
    }

    /// The chain is followed to a fixpoint: a resource freed only because
    /// another was freed in the same pass is collected too.
    #[test]
    fn a_chain_of_only_consumers_goes_in_one_maintain() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let leaf = graph.insert(buffer(&device, "leaf"), None);
        let middle = graph.insert(buffer(&device, "middle"), None);
        graph.add_dependency(&middle, &leaf);
        let root = graph.insert(buffer(&device, "root"), None);
        graph.add_dependency(&root, &middle);

        drop(root);
        drop(middle);
        drop(leaf);
        graph.maintain();
        assert!(graph.is_empty());
    }

    /// A resource two nodes are built from outlives either one alone: the
    /// second dependent's edge still holds it.
    #[test]
    fn a_shared_dependency_survives_while_another_dependent_holds_it() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let shared = graph.insert(buffer(&device, "shared"), None);
        let first = graph.insert(buffer(&device, "first"), None);
        graph.add_dependency(&first, &shared);
        let second = graph.insert(buffer(&device, "second"), None);
        graph.add_dependency(&second, &shared);

        drop(shared);
        drop(first);
        graph.maintain();
        assert!(
            graph.get(&second).is_some(),
            "the surviving dependent still owns the shared resource"
        );
        assert_eq!(graph.len(), 2);

        drop(second);
        graph.maintain();
        assert!(graph.is_empty());
    }

    /// Declaring the same edge twice counts one reference, not two: dropping
    /// the dependency's own id once is enough for the dependent to be the only
    /// holder.
    #[test]
    fn a_repeated_edge_counts_one_reference() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let dependent = graph.insert(buffer(&device, "dependent"), None);
        graph.add_dependency(&dependent, &part);
        graph.add_dependency(&dependent, &part);

        let held = graph
            .graph
            .get(part.node)
            .expect("the part is in the graph")
            .strong
            .strong_count();
        assert_eq!(held, 2, "the id and the one edge, not the id and two");
    }

    /// A cycle is refused where it is declared. Without that, a length-2 or
    /// longer cycle would make the topological order silently drop the nodes
    /// in it, and a walk would stop rebuilding them without saying so.
    #[test]
    #[should_panic(expected = "would close a cycle")]
    fn add_dependency_rejects_a_cycle() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = buffer_id(&mut graph, &device, "a");
        let b = graph.insert(buffer(&device, "b"), None);
        graph.add_dependency(&b, &a);
        // `a` already reaches `b`, so the reverse edge would close a cycle.
        graph.add_dependency(&a, &b);
    }

    #[test]
    #[should_panic(expected = "cannot depend on itself")]
    fn add_dependency_rejects_a_self_loop() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = buffer_id(&mut graph, &device, "a");

        graph.add_dependency(&id, &id);
    }

    /// A node is collected once the graph cannot reach it from anything that
    /// still holds it, and a node that is still held is not.
    #[test]
    fn maintain_keeps_a_node_an_id_still_names() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let held = buffer_id(&mut graph, &device, "held");

        graph.maintain();
        assert!(graph.get(&held).is_some());
    }

    /// A recipe reads its inputs back out of the graph, so it sees the handles
    /// the graph holds when it runs — the replaced ones, not the originals.
    #[test]
    fn rebuild_observes_the_updated_handles_of_its_dependencies() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = buffer_id(&mut graph, &device, "base");
        let observed = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&observed);
        let rebuilt = device.clone();
        let observed_base = base.clone();
        let dependent = graph.insert(
            buffer(&device, "dependent"),
            Some(Rebuild::new(move |graph| {
                let size = graph
                    .get(&observed_base)
                    .expect("the dependency outlives the rebuild")
                    .size();
                seen.borrow_mut().push(size);
                buffer(&rebuilt, "rebuilt").into()
            })),
        );
        graph.add_dependency(&dependent, &base);

        graph.replace(&base, sized_buffer(&device, 128));
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
        graph.add_dependency(&left, &base);
        let right = rebuildable_buffer(&mut graph, &device, &order, "right");
        graph.add_dependency(&right, &base);
        let join = rebuildable_buffer(&mut graph, &device, &order, "join");
        graph.add_dependency(&join, &left);
        graph.add_dependency(&join, &right);

        graph.replace(&base, buffer(&device, "base2"));
        graph.maintain();

        // The base and both middle nodes are rebuilt before the join, which
        // observes both of them.
        let order = order.borrow().clone();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], "base");
        assert_eq!(order[3], "join");
    }

    // -- kinds -------------------------------------------------------------

    /// An id may cross to another thread even though the graph it names may
    /// not: the strong reference it carries is an [`Arc`], so an id is `Send`
    /// and `Sync` whatever its kind.
    #[test]
    fn an_id_crosses_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ResHandle>();
        assert_send_sync::<ResHandle<wgpu::Buffer>>();
    }

    /// A typed id resolves to the resource itself, without the caller
    /// knowing which variant the node holds.
    #[test]
    fn a_typed_id_resolves_to_its_own_kind() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert(sized_buffer(&device, 64), None);

        assert_eq!(graph.get(&id).unwrap().size(), 64);
        assert!(graph.get(&id.erase()).is_some());
    }

    /// Erasing an id keeps it pointing at the same node, so the erased id is
    /// what a graph-wide read hands back.
    #[test]
    fn erasing_keeps_the_node() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = buffer_id(&mut graph, &device, "a");
        let erased = id.erase();

        assert_eq!(erased.index(), id.index());
        assert!(matches!(graph.get(&erased), Some(Resource::Buffer(_))));
        graph.replace(&id, sized_buffer(&device, 128));
        let Resource::Buffer(buffer) = graph.get(&erased).expect("the erased id still resolves")
        else {
            panic!("the node holds a buffer");
        };
        assert_eq!(
            buffer.size(),
            128,
            "the erased id reads the node's new resource"
        );
    }

    // -- inspection --------------------------------------------------------

    /// Every resource is listed with its slot, kind, dirtiness and whether the
    /// graph can rebuild it.
    #[test]
    fn nodes_reports_every_resource_with_its_kind() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert(buffer(&device, "base"), None);
        let rebuilt = device.clone();
        let derived = graph.insert(
            buffer(&device, "derived"),
            Some(Rebuild::new(move |_| buffer(&rebuilt, "rebuilt").into())),
        );
        graph.add_dependency(&derived, &base);

        let nodes: Vec<_> = graph.nodes().collect();
        assert_eq!(nodes.len(), 2, "both resources are listed");
        assert_eq!(nodes[0].index, base.index());
        assert_eq!(nodes[0].resource.kind_name(), "Buffer");
        assert!(!nodes[0].dirty);
        assert!(!nodes[0].rebuildable);
        assert!(nodes[1].rebuildable, "the derived resource has a recipe");
    }

    /// A resource that is dirty is reported as such until maintain runs its
    /// recipe.
    #[test]
    fn nodes_reports_dirtiness() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert(buffer(&device, "base"), None);
        let rebuilt = device.clone();
        let derived = graph.insert(
            buffer(&device, "derived"),
            Some(Rebuild::new(move |_| buffer(&rebuilt, "rebuilt").into())),
        );
        graph.add_dependency(&derived, &base);

        graph.replace(&base, sized_buffer(&device, 128));
        let dirty: Vec<_> = graph.nodes().filter(|node| node.dirty).collect();
        assert_eq!(dirty.len(), 2, "the base and its dependent are dirty");

        graph.maintain();
        let still_dirty: Vec<_> = graph.nodes().filter(|node| node.dirty).collect();
        assert_eq!(
            still_dirty.len(),
            1,
            "only the recipe-less node stays dirty"
        );
        assert_eq!(still_dirty[0].index, base.index());
    }

    /// The kinds a stored resource can be are named, and the name is the
    /// erased kind a graph-wide read reports.
    #[test]
    fn a_resource_names_its_kind() {
        assert_eq!(Resource::Virtual.kind_name(), "Virtual");
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert(buffer(&device, "buffer"), None);
        let erased = id.erase();
        assert_eq!(graph.get(&erased).unwrap().kind_name(), "Buffer");
    }

    /// A slot index resolves back to an id to the same resource.
    #[test]
    fn an_index_resolves_back_to_its_resource() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = buffer_id(&mut graph, &device, "a");
        let index = id.index();

        let resolved = graph.id_at(index).expect("the slot holds a resource");
        assert_eq!(resolved.index(), index);
        assert!(matches!(graph.get(&resolved), Some(Resource::Buffer(_))));
        assert_eq!(graph.id_at(index + 1), None, "the next slot is vacant");
    }

    /// An id from a slot keeps the resource alive like any other, so resolving
    /// one does not race the next maintain.
    #[test]
    fn an_index_resolves_only_while_the_resource_is_held() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = buffer_id(&mut graph, &device, "a");
        let index = id.index();
        drop(id);

        assert!(
            graph.id_at(index).is_none(),
            "a resource nothing holds cannot be named again",
        );
        graph.maintain();
        assert!(graph.is_empty());
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
        let default = graph.insert(
            TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        assert_eq!(
            graph.get(&default).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8Unorm)
        );

        let srgb = graph.insert(
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
            graph.get(&srgb).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
        // The view really is an sRGB one over a non-sRGB texture, which wgpu
        // itself cannot report back.
        assert_eq!(
            graph.get(&srgb).unwrap().view().texture().format(),
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
        let id = graph.insert(
            TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
            None,
        );

        graph.replace(
            &id,
            TextureExt::create_view(
                &texture,
                &wgpu::TextureViewDescriptor {
                    format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                    ..Default::default()
                },
            ),
        );
        assert_eq!(
            graph.get(&id).map(TextureView::format),
            Some(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
    }

    // -- virtual nodes -----------------------------------------------------

    /// The aggregation-root pattern: the root is built from its parts, so the
    /// root's edge is what keeps each part alive once the caller's own id to it
    /// is gone.
    #[test]
    fn a_virtual_root_keeps_its_parts_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let root = graph.insert(Virtual, None);
        graph.add_dependency(&root, &part);

        drop(part);
        graph.maintain();
        assert!(graph.get(&root).is_some());
        assert_eq!(graph.len(), 2, "the root's edge still holds the part");
    }

    /// Dropping a virtual root frees the parts it was the last holder of, in
    /// the same `maintain`.
    #[test]
    fn dropping_a_virtual_root_frees_its_parts() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let root = graph.insert(Virtual, None);
        graph.add_dependency(&root, &part);

        drop(part);
        drop(root);
        graph.maintain();
        assert!(graph.is_empty());
    }

    /// A part two roots are built from outlives either root alone: the other
    /// root's edge still holds it.
    #[test]
    fn a_shared_part_survives_while_another_root_is_alive() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let first = graph.insert(Virtual, None);
        graph.add_dependency(&first, &part);
        let second = graph.insert(Virtual, None);
        graph.add_dependency(&second, &part);

        drop(part);
        drop(first);
        graph.maintain();
        assert!(
            graph.get(&second).is_some(),
            "the surviving root still owns the shared part"
        );
        assert_eq!(graph.len(), 2);

        drop(second);
        graph.maintain();
        assert!(graph.is_empty());
    }

    /// Replacing a part marks the virtual root built from it dirty, and the
    /// root's recipe follows its part in dependency order.
    #[test]
    fn replacing_a_part_rebuilds_the_virtual_root() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let part = graph.insert(buffer(&device, "part"), None);
        let order = Rc::new(RefCell::new(Vec::new()));
        let root_order = Rc::clone(&order);
        let root = graph.insert(
            Virtual,
            Some(Rebuild::new(move |_graph| {
                root_order.borrow_mut().push("root");
                Resource::Virtual
            })),
        );
        graph.add_dependency(&root, &part);

        graph.replace(&part, buffer(&device, "part2"));
        graph.maintain();
        assert_eq!(*order.borrow(), vec!["root"]);
    }

    /// A virtual node is a graph citizen with no handle: it resolves to
    /// [`Virtual`] and to nothing else.
    #[test]
    fn a_virtual_node_has_no_handle_of_its_own() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert(Virtual, None);

        assert!(matches!(graph.get(&root), Some(Virtual)));
    }

    /// A virtual node nothing holds is collected like any other node.
    #[test]
    fn maintain_collects_a_virtual_node_with_no_holders() {
        let mut graph = ResourceGraph::new();
        let root = graph.insert(Virtual, None);

        drop(root);
        graph.maintain();
        assert!(graph.is_empty());
    }

    /// A node keeps its kind: replacing a virtual node hands back the previous
    /// virtual value, and the node stays a root its parts hang from.
    #[test]
    fn a_virtual_node_can_be_replaced() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let root = graph.insert(Virtual, None);
        let dependent = graph.insert(buffer(&device, "dependent"), None);
        graph.add_dependency(&dependent, &root);

        let previous = graph
            .replace(&root, Virtual)
            .expect("the node is in the graph");
        assert_eq!(previous, Virtual);
        assert!(matches!(graph.get(&root), Some(Virtual)));
        assert_eq!(
            graph.dependencies(&dependent).collect::<Vec<_>>(),
            vec![root.erase()]
        );
    }
}
