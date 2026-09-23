//! Dependency-tracked GPU resource graph.
//!
//! The graph is the renderer's single source of truth for the wgpu resources a
//! frame draws with — textures and their views, samplers, buffers and bind
//! groups. Pipeline objects (shader modules, layouts, pipelines) are not
//! tracked: they are immutable once built, and the render pipelines this
//! crate builds are cached by their variant rather than rebuilt from
//! dependencies. Resources are plain wgpu handles — the graph does not wrap
//! them — but it remembers which resource was built from which, so that
//! derived resources can be rebuilt when their inputs change:
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

use smallvec::SmallVec;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::{Dfs, DfsPostOrder, NodeIndexable, Reversed, Topo};

/// Handle to a resource stored in a [`ResourceGraph`].
///
/// While the resource lives, its id resolves through every accessor.
/// Removing the resource invalidates every id to it, but the graph recycles
/// the freed slots: a resource inserted later may answer an id that
/// previously referred to a removed one. Callers that hold ids across
/// removals must drop them themselves — an id can be confirmed dead with
/// [`ResourceGraph::get`] at one point in time, but never assumed to stay
/// dead afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(NodeIndex);

impl ResourceId {
    /// The graph index this id refers to.
    ///
    /// Only meaningful while the resource lives, but stable across the
    /// resource's lifetime: callers can group or sort by it (ordering draws
    /// so consecutive ones share a bind group, for example) without holding
    /// a borrow.
    pub fn index(&self) -> usize {
        self.0.index()
    }
}

/// A wgpu resource owned by the graph.
///
/// Variants are thin: each holds the wgpu handle itself, so callers can keep
/// working with raw wgpu and use the graph purely for bookkeeping.
#[derive(Clone, Debug)]
pub enum Resource {
    /// A buffer (vertex, index, uniform or storage).
    Buffer(wgpu::Buffer),
    /// A texture.
    Texture(wgpu::Texture),
    /// A texture view.
    TextureView(wgpu::TextureView),
    /// A sampler.
    Sampler(wgpu::Sampler),
    /// A bind group.
    BindGroup(wgpu::BindGroup),
}

impl Resource {
    /// The buffer handle, if this resource is a buffer.
    pub fn as_buffer(&self) -> Option<&wgpu::Buffer> {
        match self {
            Self::Buffer(buffer) => Some(buffer),
            _ => None,
        }
    }

    /// The texture handle, if this resource is a texture.
    pub fn as_texture(&self) -> Option<&wgpu::Texture> {
        match self {
            Self::Texture(texture) => Some(texture),
            _ => None,
        }
    }

    /// The texture view handle, if this resource is a texture view.
    pub fn as_texture_view(&self) -> Option<&wgpu::TextureView> {
        match self {
            Self::TextureView(view) => Some(view),
            _ => None,
        }
    }

    /// The sampler handle, if this resource is a sampler.
    pub fn as_sampler(&self) -> Option<&wgpu::Sampler> {
        match self {
            Self::Sampler(sampler) => Some(sampler),
            _ => None,
        }
    }

    /// The bind group handle, if this resource is a bind group.
    pub fn as_bind_group(&self) -> Option<&wgpu::BindGroup> {
        match self {
            Self::BindGroup(bind_group) => Some(bind_group),
            _ => None,
        }
    }
}

impl From<wgpu::Buffer> for Resource {
    fn from(value: wgpu::Buffer) -> Self {
        Self::Buffer(value)
    }
}

impl From<wgpu::Texture> for Resource {
    fn from(value: wgpu::Texture) -> Self {
        Self::Texture(value)
    }
}

impl From<wgpu::TextureView> for Resource {
    fn from(value: wgpu::TextureView) -> Self {
        Self::TextureView(value)
    }
}

impl From<wgpu::Sampler> for Resource {
    fn from(value: wgpu::Sampler) -> Self {
        Self::Sampler(value)
    }
}

impl From<wgpu::BindGroup> for Resource {
    fn from(value: wgpu::BindGroup) -> Self {
        Self::BindGroup(value)
    }
}

/// Error returned when a resource id does not resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoSuchResource(pub ResourceId);

impl core::fmt::Display for NoSuchResource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "no resource with id {:?}", self.0)
    }
}

impl std::error::Error for NoSuchResource {}

#[derive(Debug)]
struct Node {
    resource: Resource,
    /// Set when this resource or one of its dependencies was replaced.
    dirty: bool,
    /// Whether the resource survives cleanup on its own rather than only
    /// through the resources built from it. See [Retention](self).
    strong: bool,
    /// Scratch liveness mark [`ResourceGraph::cleanup`] recomputes on every
    /// run, so the pass needs no per-run set allocation.
    alive: bool,
}

/// Direct dependencies `rebuild_dirty` collects on the stack; beyond this,
/// they spill onto the heap.
const MAX_DIRECT_DEPENDENCIES: usize = 8;

/// A directed acyclic graph of wgpu resources.
///
/// Every edge points from a dependency to a resource built from it, so the
/// dependents of a node are exactly the nodes reachable from it.
#[derive(Debug, Default)]
pub struct ResourceGraph {
    graph: StableDiGraph<Node, ()>,
}

impl ResourceGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of resources in the graph.
    pub fn len(&self) -> usize {
        self.graph.node_count()
    }

    /// Whether the graph holds no resources.
    pub fn is_empty(&self) -> bool {
        self.graph.node_count() == 0
    }

    /// Add `resource`, recording that it was built from `dependencies`, as a
    /// *strong* node that [`Self::cleanup`] keeps whether or not anything
    /// depends on it.
    ///
    /// Use this for a resource the caller holds and uses directly. Use
    /// [`Self::insert_weak`] for one that exists only to feed a consumer.
    ///
    /// The new resource starts clean: it reflects the current state of its
    /// dependencies by construction.
    pub fn insert_strong(
        &mut self,
        resource: impl Into<Resource>,
        dependencies: &[ResourceId],
    ) -> Result<ResourceId, NoSuchResource> {
        self.insert(resource, dependencies, true)
    }

    /// Add `resource`, recording that it was built from `dependencies`, as a
    /// *weak* node that [`Self::cleanup`] collects once nothing alive is built
    /// from it.
    ///
    /// Use this for a resource that only feeds a consumer — a uniform a bind
    /// group reads, an input to a derived resource — so that dropping the
    /// consumer also drops it. Use [`Self::insert_strong`] for a resource the
    /// caller holds itself.
    ///
    /// The new resource starts clean: it reflects the current state of its
    /// dependencies by construction.
    pub fn insert_weak(
        &mut self,
        resource: impl Into<Resource>,
        dependencies: &[ResourceId],
    ) -> Result<ResourceId, NoSuchResource> {
        self.insert(resource, dependencies, false)
    }

    fn insert(
        &mut self,
        resource: impl Into<Resource>,
        dependencies: &[ResourceId],
        strong: bool,
    ) -> Result<ResourceId, NoSuchResource> {
        for dependency in dependencies {
            if self.get(*dependency).is_none() {
                return Err(NoSuchResource(*dependency));
            }
        }
        let id = ResourceId(self.graph.add_node(Node {
            resource: resource.into(),
            dirty: false,
            strong,
            alive: false,
        }));
        for dependency in dependencies {
            // dependency -> dependent
            self.graph.add_edge(dependency.0, id.0, ());
        }
        Ok(id)
    }

    /// Borrow the resource behind `id`.
    pub fn get(&self, id: ResourceId) -> Option<&Resource> {
        self.graph.node_weight(id.0).map(|node| &node.resource)
    }

    /// The bind group behind `id`, or `None` when the id is unknown or the
    /// resource is not a bind group.
    pub fn get_bind_group(&self, id: ResourceId) -> Option<&wgpu::BindGroup> {
        self.get(id)?.as_bind_group()
    }

    /// The buffer behind `id`, or `None` when the id is unknown or the
    /// resource is not a buffer.
    pub fn get_buffer(&self, id: ResourceId) -> Option<&wgpu::Buffer> {
        self.get(id)?.as_buffer()
    }

    /// The texture behind `id`, or `None` when the id is unknown or the
    /// resource is not a texture.
    pub fn get_texture(&self, id: ResourceId) -> Option<&wgpu::Texture> {
        self.get(id)?.as_texture()
    }

    /// The texture view behind `id`, or `None` when the id is unknown or the
    /// resource is not a texture view.
    pub fn get_texture_view(&self, id: ResourceId) -> Option<&wgpu::TextureView> {
        self.get(id)?.as_texture_view()
    }

    /// The sampler behind `id`, or `None` when the id is unknown or the
    /// resource is not a sampler.
    pub fn get_sampler(&self, id: ResourceId) -> Option<&wgpu::Sampler> {
        self.get(id)?.as_sampler()
    }

    /// Replace the resource behind `id` and mark it — and every resource
    /// transitively built from it — dirty.
    ///
    /// Returns the previous handle, or `None` if `id` is unknown.
    pub fn replace(&mut self, id: ResourceId, resource: impl Into<Resource>) -> Option<Resource> {
        let node = self.graph.node_weight_mut(id.0)?;
        let previous = core::mem::replace(&mut node.resource, resource.into());
        node.dirty = true;
        self.mark_dependents_dirty(id);
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
    pub fn remove(&mut self, id: ResourceId) -> Vec<Resource> {
        // A post-order DFS yields a node before the nodes it was built from —
        // dependents first, `id` last. Reversing puts the returned list in
        // dependency order, matching [`Self::rebuild_dirty`]. The order the
        // nodes are dropped in is itself irrelevant: a stable graph removes
        // edges together with their node.
        let mut dfs = DfsPostOrder::new(&self.graph, id.0);
        let mut removed = Vec::new();
        while let Some(node) = dfs.next(&self.graph) {
            if let Some(node_weight) = self.graph.remove_node(node) {
                removed.push(node_weight.resource);
            }
        }
        removed.reverse();
        removed
    }

    /// Remove `id` together with every resource transitively built from it,
    /// dropping everything without returning it.
    ///
    /// This is [`Self::remove`] for callers that do not need the dropped
    /// resources: it avoids allocating the return vector.
    pub fn remove_drop(&mut self, id: ResourceId) {
        // Same post-order walk as [`Self::remove`], but the nodes are dropped
        // in place instead of collected.
        let mut dfs = DfsPostOrder::new(&self.graph, id.0);
        while let Some(node) = dfs.next(&self.graph) {
            self.graph.remove_node(node);
        }
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
        // See [`Self::mark_alive`] for the liveness walk.
        self.mark_alive();

        // Indices are visited by number instead of through an iterator because
        // the body mutates the graph; stable indices never shift, so a single
        // bound covers every node.
        let bound = self.graph.node_bound();
        let mut removed = Vec::new();
        for i in 0..bound {
            let index = NodeIndex::new(i);
            if self
                .graph
                .node_weight(index)
                .is_some_and(|node| !node.alive)
                && let Some(node) = self.graph.remove_node(index)
            {
                removed.push(node.resource);
            }
        }
        removed
    }

    /// Drop every resource nothing alive is built from, without returning it.
    ///
    /// This is [`Self::cleanup`] for callers that do not need the dropped
    /// resources: it avoids allocating the return vector.
    pub fn cleanup_drop(&mut self) {
        // See [`Self::mark_alive`] for the liveness walk.
        self.mark_alive();

        let bound = self.graph.node_bound();
        for i in 0..bound {
            let index = NodeIndex::new(i);
            if self
                .graph
                .node_weight(index)
                .is_some_and(|node| !node.alive)
            {
                self.graph.remove_node(index);
            }
        }
    }

    /// Recompute the per-node liveness flag: a node is alive when it is strong
    /// or when something alive was built from it.
    ///
    /// Aliveness propagates from a dependent to what it was built from, so the
    /// walk goes against the edges — from each strong node to its dependencies.
    /// Liveness lives in a per-node flag rather than a set, so the walk
    /// allocates nothing.
    fn mark_alive(&mut self) {
        for node in self.graph.node_weights_mut() {
            node.alive = false;
        }
        let bound = self.graph.node_bound();
        for i in 0..bound {
            let index = NodeIndex::new(i);
            if self
                .graph
                .node_weight(index)
                .is_some_and(|node| node.strong)
            {
                let mut dfs = Dfs::new(Reversed(&self.graph), index);
                while let Some(node) = dfs.next(Reversed(&self.graph)) {
                    self.graph[node].alive = true;
                }
            }
        }
    }

    /// Whether `id` needs to be rebuilt before it can be used again.
    pub fn is_dirty(&self, id: ResourceId) -> bool {
        self.graph.node_weight(id.0).is_some_and(|node| node.dirty)
    }

    /// Whether any resource in the graph is dirty.
    pub fn any_dirty(&self) -> bool {
        self.graph.node_weights().any(|node| node.dirty)
    }

    /// Mark `id` clean, for example after rebuilding it outside
    /// [`Self::rebuild_dirty`].
    pub fn mark_clean(&mut self, id: ResourceId) {
        if let Some(node) = self.graph.node_weight_mut(id.0) {
            node.dirty = false;
        }
    }

    /// The immediate dependencies recorded for `id`.
    pub fn dependencies(&self, id: ResourceId) -> impl Iterator<Item = ResourceId> + '_ {
        self.graph
            .neighbors_directed(id.0, petgraph::Direction::Incoming)
            .map(ResourceId)
    }

    /// Every dirty resource, in dependency order (a resource always follows
    /// the resources it was built from).
    pub fn dirty(&self) -> impl Iterator<Item = ResourceId> + '_ {
        let graph = &self.graph;
        // The graph is acyclic by construction, so the traversal visits
        // every node and the order is a true topological order.
        let mut topo = Topo::new(graph);
        core::iter::from_fn(move || {
            loop {
                let node = topo.next(graph)?;
                if graph.node_weight(node).is_some_and(|node| node.dirty) {
                    return Some(ResourceId(node));
                }
            }
        })
    }

    /// Rebuild every dirty resource by calling `rebuild` in dependency order.
    ///
    /// `rebuild` receives the dirty resource's id, its current handle, and its
    /// direct dependencies' current handles. Returning `Some` replaces the
    /// resource and clears its dirty flag; returning `None` leaves it dirty,
    /// which is how a caller defers work it cannot do yet.
    ///
    /// Dependents are visited after their dependencies, so a rebuild observes
    /// already-updated inputs.
    pub fn rebuild_dirty<F>(&mut self, mut rebuild: F)
    where
        F: FnMut(ResourceId, &Resource, &[Resource]) -> Option<Resource>,
    {
        // Drive the topological traversal by hand: `Topo` borrows the graph
        // per step, so node weights can be updated between steps without
        // materializing the dirty list. `rebuild` cannot touch the graph —
        // it receives only ids and handles — so the traversal stays valid.
        let mut topo = Topo::new(&self.graph);
        while let Some(node) = topo.next(&self.graph) {
            if !self.graph.node_weight(node).is_some_and(|node| node.dirty) {
                continue;
            }
            // Dependency handles are cheap reference-counted clones, so the
            // common case — a handful of dependencies — stays on the stack.
            let id = ResourceId(node);
            let dependencies: SmallVec<[Resource; MAX_DIRECT_DEPENDENCIES]> = self
                .dependencies(id)
                .filter_map(|dependency| self.get(dependency).cloned())
                .collect();
            let Some(current) = self.get(id) else {
                continue;
            };
            let Some(rebuilt) = rebuild(id, current, &dependencies) else {
                continue;
            };
            if let Some(node_weight) = self.graph.node_weight_mut(node) {
                node_weight.resource = rebuilt;
                node_weight.dirty = false;
            }
        }
    }

    fn mark_dependents_dirty(&mut self, id: ResourceId) {
        let mut dfs = Dfs::new(&self.graph, id.0);
        while let Some(node) = dfs.next(&self.graph) {
            if node != id.0
                && let Some(node_weight) = self.graph.node_weight_mut(node)
            {
                node_weight.dirty = true;
            }
        }
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

    #[test]
    fn replace_marks_dependents_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph
            .insert_strong(buffer(&device, "base"), &[])
            .expect("insert base");
        let middle = graph
            .insert_strong(buffer(&device, "middle"), &[base])
            .expect("insert middle");
        let leaf = graph
            .insert_strong(buffer(&device, "leaf"), &[middle])
            .expect("insert leaf");

        assert!(!graph.any_dirty());

        graph.replace(base, buffer(&device, "base2"));
        assert!(graph.is_dirty(base));
        assert!(graph.is_dirty(middle));
        assert!(graph.is_dirty(leaf));
        // Dependency order: base before middle before leaf.
        assert_eq!(graph.dirty().collect::<Vec<_>>(), vec![base, middle, leaf]);
    }

    #[test]
    fn replace_does_not_dirty_unrelated_resources() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let a = graph.insert_strong(buffer(&device, "a"), &[]).unwrap();
        let b = graph.insert_strong(buffer(&device, "b"), &[]).unwrap();

        graph.replace(a, buffer(&device, "a2"));
        assert!(graph.is_dirty(a));
        assert!(!graph.is_dirty(b));
    }

    #[test]
    fn remove_drops_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(buffer(&device, "base"), &[]).unwrap();
        let dependent = graph
            .insert_strong(buffer(&device, "dependent"), &[base])
            .unwrap();
        let unrelated = graph
            .insert_strong(buffer(&device, "unrelated"), &[])
            .unwrap();

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
        let base = graph.insert_strong(buffer(&device, "base"), &[]).unwrap();
        let dependent = graph
            .insert_strong(buffer(&device, "dependent"), &[base])
            .unwrap();

        graph.replace(base, buffer(&device, "base2"));

        let mut visited = Vec::new();
        graph.rebuild_dirty(|id, _current, dependencies| {
            visited.push(id);
            if id == dependent {
                // The dependency was already rebuilt, so the caller observes
                // the fresh handle rather than the replaced one.
                assert_eq!(dependencies.len(), 1);
            }
            Some(buffer(&device, "rebuilt").into())
        });

        assert_eq!(visited, vec![base, dependent]);
        assert!(!graph.any_dirty());
    }

    #[test]
    fn rebuild_can_defer() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(buffer(&device, "base"), &[]).unwrap();
        graph.replace(base, buffer(&device, "base2"));

        graph.rebuild_dirty(|_, _, _| None);
        assert!(graph.is_dirty(base));

        graph.mark_clean(base);
        assert!(!graph.any_dirty());
    }

    #[test]
    fn insert_rejects_unknown_dependency() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let id = graph.insert_strong(buffer(&device, "a"), &[]).unwrap();
        graph.remove_drop(id);

        assert_eq!(
            graph.insert_strong(buffer(&device, "b"), &[id]),
            Err(NoSuchResource(id))
        );
    }

    #[test]
    fn cleanup_keeps_strong_nodes_with_no_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let strong = graph.insert_strong(buffer(&device, "strong"), &[]).unwrap();

        assert!(graph.cleanup().is_empty());
        assert!(graph.get(strong).is_some());
    }

    #[test]
    fn cleanup_collects_a_weak_node_nothing_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"), &[]).unwrap();

        assert_eq!(graph.cleanup().len(), 1);
        assert!(graph.get(weak).is_none());
        assert!(graph.is_empty());
    }

    #[test]
    fn cleanup_keeps_a_weak_node_a_strong_dependent_is_built_from() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let weak = graph.insert_weak(buffer(&device, "weak"), &[]).unwrap();
        let dependent = graph
            .insert_strong(buffer(&device, "dependent"), &[weak])
            .unwrap();

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
        let weak = graph.insert_weak(buffer(&device, "uniform"), &[]).unwrap();
        let input = graph.insert_strong(buffer(&device, "input"), &[]).unwrap();
        let group = graph
            .insert_strong(buffer(&device, "group"), &[input, weak])
            .unwrap();

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
        let leaf = graph.insert_weak(buffer(&device, "leaf"), &[]).unwrap();
        let middle = graph
            .insert_weak(buffer(&device, "middle"), &[leaf])
            .unwrap();
        let root = graph
            .insert_strong(buffer(&device, "root"), &[middle])
            .unwrap();
        // A second chain whose strong node is removed: the whole chain goes.
        let gone_leaf = graph
            .insert_weak(buffer(&device, "gone_leaf"), &[])
            .unwrap();
        let gone_middle = graph
            .insert_weak(buffer(&device, "gone_middle"), &[gone_leaf])
            .unwrap();
        let gone_root = graph
            .insert_strong(buffer(&device, "gone_root"), &[gone_middle])
            .unwrap();

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
        let leaf = graph.insert_strong(buffer(&device, "leaf"), &[]).unwrap();
        let other = graph.insert_strong(buffer(&device, "other"), &[]).unwrap();

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
        let base = graph.insert_strong(sized_buffer(&device, 64), &[]).unwrap();
        let middle = graph
            .insert_strong(sized_buffer(&device, 128), &[base])
            .unwrap();
        let root = graph
            .insert_strong(sized_buffer(&device, 256), &[middle])
            .unwrap();

        let removed = graph.remove(middle);
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].as_buffer().unwrap().size(), 128);
        assert_eq!(removed[1].as_buffer().unwrap().size(), 256);
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
        let base = graph.insert_strong(sized_buffer(&device, 64), &[]).unwrap();
        let middle = graph
            .insert_strong(sized_buffer(&device, 128), &[base])
            .unwrap();
        let root = graph
            .insert_strong(sized_buffer(&device, 256), &[middle])
            .unwrap();

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
        let weak = graph.insert_weak(buffer(&device, "weak"), &[]).unwrap();

        graph.cleanup_drop();
        assert!(graph.get(weak).is_none());
        assert!(graph.is_empty());
    }

    #[test]
    fn rebuild_observes_the_updated_handles_of_its_dependencies() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert_strong(buffer(&device, "base"), &[]).unwrap();
        let dependent = graph
            .insert_strong(buffer(&device, "dependent"), &[base])
            .unwrap();

        graph.replace(base, sized_buffer(&device, 128));

        let mut observed = Vec::new();
        graph.rebuild_dirty(|id, _, dependencies| {
            if id == dependent {
                observed.push(dependencies[0].as_buffer().unwrap().size());
            }
            // Defer the base itself so the dependent observes the replaced
            // handle rather than a rebuild of it.
            if id == base {
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
        let base = graph.insert_strong(buffer(&device, "base"), &[]).unwrap();
        let left = graph
            .insert_strong(buffer(&device, "left"), &[base])
            .unwrap();
        let right = graph
            .insert_strong(buffer(&device, "right"), &[base])
            .unwrap();
        let join = graph
            .insert_strong(buffer(&device, "join"), &[left, right])
            .unwrap();

        graph.replace(base, buffer(&device, "base2"));

        let mut visited = Vec::new();
        graph.rebuild_dirty(|id, _, dependencies| {
            if id == join {
                assert_eq!(dependencies.len(), 2);
            }
            visited.push(id);
            Some(buffer(&device, "rebuilt").into())
        });

        assert_eq!(visited.len(), 4);
        assert_eq!(visited[0], base);
        assert_eq!(visited[3], join);
    }
}
