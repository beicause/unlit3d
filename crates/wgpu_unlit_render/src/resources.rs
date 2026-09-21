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
//!   transitively built from it.
//! * [`ResourceGraph::rebuild_dirty`] walks the dirty resources in dependency
//!   order and lets the caller rebuild each one.
//!
//! Updates are therefore lazy and precise: uploading new bytes into an
//! existing buffer does not dirty anything, reallocating it does, and only the
//! resources that actually consumed the old handle are affected.

use smallvec::SmallVec;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::{Dfs, DfsPostOrder, Topo};

/// Handle to a resource stored in a [`ResourceGraph`].
///
/// Ids stay valid while the resource lives; they are never reused, so a stale
/// id reports [`ResourceGraph::get`] as `None` rather than aliasing a newer
/// resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(NodeIndex);

impl ResourceId {
    /// The graph index this id refers to.
    ///
    /// Ids are never reused, so this is a stable key for resources that are
    /// alive: callers can group or sort by it (ordering draws so consecutive
    /// ones share a bind group, for example) without holding a borrow.
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
    /// Human-readable kind, used in diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Buffer(_) => "buffer",
            Self::Texture(_) => "texture",
            Self::TextureView(_) => "texture view",
            Self::Sampler(_) => "sampler",
            Self::BindGroup(_) => "bind group",
        }
    }

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

    /// Add `resource`, recording that it was built from `dependencies`.
    ///
    /// The new resource starts clean: it reflects the current state of its
    /// dependencies by construction.
    pub fn insert(
        &mut self,
        resource: impl Into<Resource>,
        dependencies: &[ResourceId],
    ) -> Result<ResourceId, NoSuchResource> {
        for dependency in dependencies {
            if self.get(*dependency).is_none() {
                return Err(NoSuchResource(*dependency));
            }
        }
        let id = ResourceId(self.graph.add_node(Node {
            resource: resource.into(),
            dirty: false,
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
    pub fn remove(&mut self, id: ResourceId) -> Vec<Resource> {
        // The doomed set: the resource and everything built from it. Collect
        // the ids first, since the removal mutates the graph the DFS walks.
        let mut doomed: Vec<_> = self.dependents(id).collect();
        doomed.push(id);
        // A post-order DFS from the root yields each doomed node after the
        // nodes it was built from, so removing as we go never leaves a
        // dangling edge mid-removal.
        let mut dfs = DfsPostOrder::new(&self.graph, id.0);
        let mut removed = Vec::with_capacity(doomed.len());
        while let Some(node) = dfs.next(&self.graph) {
            if let Some(node_weight) = self.graph.remove_node(node) {
                removed.push(node_weight.resource);
            }
        }
        removed
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

    /// Every resource transitively built from `id`, excluding `id` itself.
    ///
    /// The iteration order is unspecified; nothing allocates.
    pub fn dependents(&self, id: ResourceId) -> impl Iterator<Item = ResourceId> + '_ {
        let graph = &self.graph;
        let mut dfs = Dfs::new(graph, id.0);
        core::iter::from_fn(move || {
            loop {
                let node = dfs.next(graph)?;
                if node != id.0 {
                    return Some(ResourceId(node));
                }
            }
        })
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
        // Iterate over a materialized dirty list: `rebuild` mutates the
        // graph, which would corrupt a lazy traversal over it.
        let dirty: Vec<_> = self.dirty().collect();
        for id in dirty {
            // Dependency handles are cheap reference-counted clones, so the
            // common case — a handful of dependencies — stays on the stack.
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
            if let Some(node) = self.graph.node_weight_mut(id.0) {
                node.resource = rebuilt;
                node.dirty = false;
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

    fn device() -> wgpu::Device {
        crate::util::test::noop_device().0
    }

    #[test]
    fn replace_marks_dependents_dirty() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph
            .insert(buffer(&device, "base"), &[])
            .expect("insert base");
        let middle = graph
            .insert(buffer(&device, "middle"), &[base])
            .expect("insert middle");
        let leaf = graph
            .insert(buffer(&device, "leaf"), &[middle])
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
        let a = graph.insert(buffer(&device, "a"), &[]).unwrap();
        let b = graph.insert(buffer(&device, "b"), &[]).unwrap();

        graph.replace(a, buffer(&device, "a2"));
        assert!(graph.is_dirty(a));
        assert!(!graph.is_dirty(b));
    }

    #[test]
    fn remove_drops_dependents() {
        let device = device();
        let mut graph = ResourceGraph::new();
        let base = graph.insert(buffer(&device, "base"), &[]).unwrap();
        let dependent = graph.insert(buffer(&device, "dependent"), &[base]).unwrap();
        let unrelated = graph.insert(buffer(&device, "unrelated"), &[]).unwrap();

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
        let base = graph.insert(buffer(&device, "base"), &[]).unwrap();
        let dependent = graph.insert(buffer(&device, "dependent"), &[base]).unwrap();

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
        let base = graph.insert(buffer(&device, "base"), &[]).unwrap();
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
        let id = graph.insert(buffer(&device, "a"), &[]).unwrap();
        graph.remove(id);

        assert_eq!(
            graph.insert(buffer(&device, "b"), &[id]),
            Err(NoSuchResource(id))
        );
    }
}
