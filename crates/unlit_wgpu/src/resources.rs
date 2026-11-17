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

mod graph;
#[cfg(test)]
mod tests;

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

/// A resource's identity as a plain value.
///
/// The slot index is the low 32 bits and the generation the slot was in the
/// high 32 — the same layout as `unlit_ecs::Entity`. A [`ResHandle`] carries
/// one, and it is the form a handle takes when it has to travel somewhere that
/// cannot hold the typed handle: a report, a transport, a stored argument.
/// Resolve it back with [`ResourceGraph::resolve`], which reports `None` once
/// the slot has been recycled.
///
/// It is [`Copy`], unlike a [`ResHandle`], because it holds no reference: it
/// names the resource but does not keep it alive.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
#[cfg_attr(feature = "reflect", facet(transparent))]
pub struct ResId(u64);

impl ResId {
    /// The id as bits.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Rebuild an id from the bits [`Self::to_bits`] produced.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The slot index in the low 32 bits.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as u32 as usize
    }

    /// The slot handle this id names.
    fn node(self) -> NodeId {
        NodeId::from_bits(self.0)
    }

    /// The plain id naming `node`.
    fn from_node(node: NodeId) -> Self {
        Self(node.to_bits())
    }
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
    /// a borrow. The index alone does not identify the resource across a
    /// removal and reuse of its slot; [`ResHandle::id`] does.
    pub fn index(&self) -> usize {
        self.node.index()
    }

    /// The resource's [`ResId`], for an API that stores ids but does not know
    /// the type — an inspector that reports one, or a transport that carries
    /// it.
    ///
    /// The id names the resource but holds no reference to it, so it is
    /// [`Copy`]. Resolve it back through the graph with
    /// [`ResourceGraph::resolve`], which reports `None` once the slot has
    /// been recycled.
    #[must_use]
    pub fn id(&self) -> ResId {
        ResId::from_node(self.node)
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
/// A borrowed view rather than an owned handle: the [`id`](NodeInfo::id) names
/// the node and can be resolved back with [`ResourceGraph::resolve`], while the
/// resource itself is borrowed from the graph for as long as the walk runs.
#[derive(Debug)]
pub struct NodeInfo<'a> {
    /// The node's [`ResId`], the identity [`ResHandle::id`] reports.
    pub id: ResId,
    /// The stored resource.
    pub resource: &'a Resource,
    /// Whether this resource or one of its dependencies was replaced, and so
    /// whether [`ResourceGraph::maintain`] still has a recipe to run.
    pub dirty: bool,
    /// Whether the graph can rebuild this resource from its inputs on its own.
    pub rebuildable: bool,
}

/// One resource slot of a [`ResourceGraph`], as plain data.
///
/// The serialisable projection of [`NodeInfo`]: where that borrows the
/// stored resource, this reports the handle and the shape around it, so a
/// caller that only inspects the graph needs no access to the wgpu handles.
///
/// No bare slot index is kept: the [id](ResourceInfo::id) carries it, and the
/// slot order is already the order [`ResourceGraph::resource_infos`] returns.
/// Note that the id's high 32 bits are the generation, so sorting by id orders
/// by generation rather than by slot; [`ResId::index`] is the slot.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ResourceInfo {
    /// The resource's [`ResId`]. Resolve it with [`ResourceGraph::resolve`].
    pub id: ResId,
    /// The resource kind, as [`Resource::kind_name`] reports it.
    pub kind: &'static str,
    /// Whether this resource or one of its dependencies was replaced, so
    /// [`ResourceGraph::maintain`] still has work to do.
    pub dirty: bool,
    /// Whether the graph can rebuild this resource from its inputs on its own.
    pub rebuildable: bool,
}

/// How many resources of one kind a [`ResourceGraph`] holds.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ResourceKindCount {
    /// The resource kind, as [`Resource::kind_name`] reports it.
    pub kind: &'static str,
    /// How many slots hold a resource of this kind.
    pub count: usize,
}

/// The shape of a [`ResourceGraph`], as plain data.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ResourceGraphInfo {
    /// Occupied slots, that is [`ResourceGraph::len`]; the `kinds` counts
    /// sum to it.
    ///
    /// A resource nothing holds any more is still counted until the next
    /// [`ResourceGraph::maintain`] collects it. There is no `empty` field:
    /// `resources == 0` is it.
    pub resources: usize,
    /// Slots that are dirty: replaced themselves, or built from something that
    /// was, so [`ResourceGraph::maintain`] still has a recipe to run.
    pub dirty: usize,
    /// Slots carrying a rebuild recipe, which [`ResourceGraph::maintain`] can
    /// rebuild on its own.
    pub rebuildable: usize,
    /// Occupied slots grouped by kind, sorted by kind name so the report is
    /// stable.
    pub kinds: Vec<ResourceKindCount>,
}

/// What one [`ResourceGraph::maintain`] pass changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ResourceGraphMaintain {
    /// Occupied slots before the pass, that is [`ResourceGraph::len`].
    pub before: usize,
    /// Occupied slots after the pass: what survived the collection and the
    /// rebuild.
    pub after: usize,
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
    /// Reused dependency order, so a rebuild pass does not allocate one.
    ///
    /// Lifted out of the graph for the duration of a pass, because the recipes
    /// it names rebuild against the graph itself.
    order: Vec<NodeId>,
    /// How many nodes are dirty and carry a rebuild recipe — the work
    /// [`Self::maintain`]'s rebuild pass exists for. Kept beside the graph so
    /// a pass with none can skip the topological walk entirely.
    dirty_rebuilds: usize,
}
