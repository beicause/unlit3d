use super::*;

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

    /// The graph's shape as plain data.
    ///
    /// Counts what [`Self::len`] counts: a resource nothing holds any more
    /// is still included until the next [`Self::maintain`] collects it.
    pub fn info(&self) -> ResourceGraphInfo {
        let mut kinds: Vec<ResourceKindCount> = Vec::new();
        let mut dirty = 0;
        let mut rebuildable = 0;
        for (_, node) in self.graph.iter() {
            dirty += usize::from(node.dirty);
            rebuildable += usize::from(node.rebuild.is_some());
            let kind = node.resource.kind_name();
            match kinds.iter_mut().find(|count| count.kind == kind) {
                Some(count) => count.count += 1,
                None => kinds.push(ResourceKindCount { kind, count: 1 }),
            }
        }
        kinds.sort_unstable_by_key(|count| count.kind);
        ResourceGraphInfo {
            resources: self.graph.len(),
            dirty,
            rebuildable,
            kinds,
        }
    }

    /// Every resource slot, in node-slot order, as plain data.
    ///
    /// The serialisable counterpart of [`Self::nodes`]: same order and same
    /// count, but reporting the [id](ResourceInfo::id) rather than borrowing
    /// the wgpu resource.
    pub fn resource_infos(&self) -> Vec<ResourceInfo> {
        self.graph
            .iter()
            .map(|(id, node)| ResourceInfo {
                id: ResId::from_node(id),
                kind: node.resource.kind_name(),
                dirty: node.dirty,
                rebuildable: node.rebuild.is_some(),
            })
            .collect()
    }

    /// Every resource in the graph, in node-slot order.
    ///
    /// Each node's [`id`](NodeInfo::id) is stable while the resource lives, so
    /// a caller can report it, sort by it, and resolve it back with
    /// [`Self::resolve`]. The walk covers every slot the graph has ever used,
    /// so it costs the peak node count rather than the current one.
    ///
    /// A resource no id holds any more is still listed until the next
    /// [`Self::maintain`] collects it, exactly as [`Self::len`] counts it.
    pub fn nodes(&self) -> impl Iterator<Item = NodeInfo<'_>> {
        self.graph.iter().map(|(id, node)| NodeInfo {
            id: ResId::from_node(id),
            resource: &node.resource,
            dirty: node.dirty,
            rebuildable: node.rebuild.is_some(),
        })
    }

    /// The handle a [`ResId`] names, or `None` when no resource is there or
    /// nothing holds it any more.
    ///
    /// The handle is erased — its kind is only known from the resource — and
    /// holds one strong reference, so resolving an id keeps the resource alive
    /// like any other handle does. An id whose slot has been recycled no longer
    /// resolves, which is what keeps a stale id from naming a later resource; a
    /// resource nothing holds is on its way out at the next [`Self::maintain`]
    /// and cannot be named again, so the call reports `None` rather than
    /// inventing a reference to it.
    #[must_use]
    pub fn resolve(&self, id: ResId) -> Option<ResHandle> {
        let node = id.node();
        let strong = self.graph.get(node)?.strong.upgrade()?;
        Some(ResHandle {
            node,
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
        // there, and the buffer is held outside the graph for the same reason:
        // a recipe builds against the graph while the walk's list is in flight.
        let mut order = core::mem::take(&mut self.order);
        self.graph.topological_order_into(&mut order);
        for &node in &order {
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
        self.order = order;
    }

    /// Mark `node` and every node built from it dirty.
    fn mark_dependents_dirty(&mut self, node: NodeId) {
        self.graph.for_each_dependent_mut(node, |slot| {
            slot.dirty = true;
        });
    }
}
