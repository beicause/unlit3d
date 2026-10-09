//! A directed acyclic graph with slot handles.
//!
//! This is the structure [`ResourceGraph`](crate::resources::ResourceGraph)
//! keeps its resources in. It is a directed graph plus one rule the rest of the
//! design leans on: an edge that would close a cycle is refused, so the nodes
//! can always be visited in dependency order.
//!
//! # Handles
//!
//! A [`NodeId`] is the slot a node occupies. Removing a node frees its slot for
//! a later insertion, so a handle to a removed node can come to name whatever
//! takes its place. Deciding liveness is the caller's job, not the graph's:
//! [`ResHandle`](crate::resources::ResHandle) carries a strong reference that
//! [`ResourceGraph`](crate::resources::ResourceGraph) collects on, so a node
//! whose handle is still held is never removed in the first place.
//!
//! # Edges
//!
//! An edge points from a node to a node that depends on it: a dependency, then
//! the resource built from it. [`Dag::dependencies`] therefore walks back along
//! the edges, [`Dag::for_each_dependent_mut`] walks forward, and
//! [`Dag::topological_order`] puts every node after the nodes it depends on. An
//! edge is recorded once however often it is declared, which
//! [`Dag::add_edge`] reports: only the declaration that recorded it returns
//! `true`.

use std::vec::Vec;

/// The slot a node occupies in a [`Dag`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId {
    index: u32,
}

impl NodeId {
    /// The slot this handle names.
    ///
    /// Only meaningful while the node lives, but stable across the node's
    /// lifetime, so callers can group or sort by it without holding a borrow.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }
}

/// Why [`Dag::add_edge`] refused an edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeError {
    /// The endpoint does not resolve: its node was removed, or the handle never
    /// came from this graph.
    NoSuchNode(NodeId),
    /// Both endpoints are the same node.
    SelfLoop,
    /// The edge would close a cycle.
    Cycle,
}

/// A slot a node occupies.
#[derive(Debug)]
struct NodeSlot<N> {
    /// The node's weight, or `None` while the slot is vacant.
    weight: Option<N>,
    /// Edges leaving this node, towards the nodes that depend on it.
    dependents: Vec<u32>,
    /// Edges entering this node, from the nodes it depends on.
    dependencies: Vec<u32>,
}

/// An edge, pointing from a dependency to what depends on it.
#[derive(Debug)]
struct EdgeSlot {
    /// The dependency.
    from: u32,
    /// The node built from it.
    to: u32,
}

/// A directed acyclic graph whose edges may not close a cycle.
///
/// Slots are recycled: removing a node frees its slot, and a later insertion
/// reuses it. Neither the node array nor the walk stamps shrink, so a graph that
/// once held many nodes keeps scanning that many: memory is bounded by the peak
/// number of nodes held at once, not by the number held now.
#[derive(Debug)]
pub struct Dag<N> {
    nodes: Vec<NodeSlot<N>>,
    edges: Vec<EdgeSlot>,
    /// Slots holding no node, to be handed out by [`Dag::insert`].
    free_nodes: Vec<u32>,
    /// Edge slots holding no edge, to be handed out by [`Dag::add_edge`].
    free_edges: Vec<u32>,
    /// How many slots hold a node.
    live: usize,
    /// Per-slot stamp of the most recent walk that visited it. A walk takes a
    /// fresh stamp rather than clearing this, so a traversal after the array
    /// has grown allocates nothing.
    visited: Vec<u32>,
    /// The stamp `visited` presently records.
    stamp: u32,
    /// Reused stack of slot indices for the plain depth-first walks.
    stack: Vec<u32>,
    /// Reused stack for the post-order walk, where a slot is pushed once to be
    /// visited and once more — flagged `true` — to be emitted.
    plan_stack: Vec<(u32, bool)>,
    /// Reused list of the slots a plan selected, in the order it selected them.
    order: Vec<u32>,
}

impl<N> Default for Dag<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N> Dag<N> {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            free_nodes: Vec::new(),
            free_edges: Vec::new(),
            live: 0,
            visited: Vec::new(),
            stamp: 0,
            stack: Vec::new(),
            plan_stack: Vec::new(),
            order: Vec::new(),
        }
    }

    /// Number of nodes in the graph.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the graph holds no nodes.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Add `weight` as a node and return a handle to it.
    pub fn insert(&mut self, weight: N) -> NodeId {
        let index = match self.free_nodes.pop() {
            Some(index) => {
                let slot = &mut self.nodes[index as usize];
                debug_assert!(slot.weight.is_none(), "a free slot holds no node");
                slot.weight = Some(weight);
                slot.dependents.clear();
                slot.dependencies.clear();
                index
            }
            None => {
                self.nodes.push(NodeSlot {
                    weight: Some(weight),
                    dependents: Vec::new(),
                    dependencies: Vec::new(),
                });
                (self.nodes.len() - 1) as u32
            }
        };
        self.live += 1;
        self.node_id(index)
    }

    /// Every live node, as its handle and its weight, in slot order.
    ///
    /// Slots are recycled, so the order is by slot rather than by insertion: a
    /// node that reused a freed slot appears where that slot is. The walk
    /// covers the whole node array, so it costs the peak number of nodes the
    /// graph ever held rather than the number it holds now.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &N)> {
        self.nodes.iter().enumerate().filter_map(|(index, slot)| {
            slot.weight
                .as_ref()
                .map(|weight| (self.node_id(index as u32), weight))
        })
    }

    /// The handle of the node in slot `index`, or `None` when that slot is
    /// vacant or out of range.
    ///
    /// A caller that kept only a node's [`index`](NodeId::index) — an index
    /// survives where a handle cannot be stored — resolves it back through
    /// this.
    pub fn id_at(&self, index: usize) -> Option<NodeId> {
        self.nodes
            .get(index)
            .filter(|slot| slot.weight.is_some())
            .map(|_| self.node_id(index as u32))
    }

    /// Borrow the weight of the node `id` names.
    pub fn get(&self, id: NodeId) -> Option<&N> {
        let index = self.resolve(id)?;
        self.nodes[index].weight.as_ref()
    }

    /// Mutably borrow the weight of the node `id` names.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut N> {
        let index = self.resolve(id)?;
        self.nodes[index].weight.as_mut()
    }

    /// Record that `to` depends on `from`, and report whether this call
    /// recorded it.
    ///
    /// Refused when either handle does not resolve, when both name the same
    /// node, or when the edge would close a cycle. Declaring an existing
    /// dependency again is a no-op rather than a second edge, so
    /// [`Dag::dependencies`] never yields the same node twice; such a repeat
    /// returns `Ok(false)`, and only the call that recorded the edge returns
    /// `Ok(true)`.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId) -> Result<bool, EdgeError> {
        let Some(from_index) = self.resolve(from) else {
            return Err(EdgeError::NoSuchNode(from));
        };
        let Some(to_index) = self.resolve(to) else {
            return Err(EdgeError::NoSuchNode(to));
        };
        if from_index == to_index {
            return Err(EdgeError::SelfLoop);
        }
        // `from -> to` closes a cycle exactly when `to` already reaches `from`.
        if self.reaches(to_index, from_index) {
            return Err(EdgeError::Cycle);
        }
        if self.has_edge(from_index, to_index) {
            return Ok(false);
        }
        let edge = match self.free_edges.pop() {
            Some(edge) => {
                self.edges[edge as usize] = EdgeSlot {
                    from: from_index as u32,
                    to: to_index as u32,
                };
                edge
            }
            None => {
                self.edges.push(EdgeSlot {
                    from: from_index as u32,
                    to: to_index as u32,
                });
                (self.edges.len() - 1) as u32
            }
        };
        self.nodes[from_index].dependents.push(edge);
        self.nodes[to_index].dependencies.push(edge);
        Ok(true)
    }

    /// The nodes `id` depends on, in unspecified order.
    ///
    /// Empty when the handle does not resolve.
    pub fn dependencies(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.incident(id, |slot| &slot.dependencies)
            .map(|edge| self.node_id(self.edges[edge as usize].from))
    }

    /// Apply `f` to `id` and to every node reachable from it by following
    /// dependents.
    ///
    /// Only the reachable nodes are visited. A handle that does not resolve
    /// visits nothing.
    pub fn for_each_dependent_mut(&mut self, id: NodeId, mut f: impl FnMut(&mut N)) {
        self.plan_dependents(id);
        let order = core::mem::take(&mut self.order);
        for &index in &order {
            if let Some(weight) = self.nodes[index as usize].weight.as_mut() {
                f(weight);
            }
        }
        self.order = order;
    }

    /// Remove every node `is_dead` reports, and return how many were removed.
    ///
    /// A node is dropped with the weights of the nodes that depend on it still
    /// in place, so a weight whose drop releases something another node was
    /// waiting for only shows up on the next call: a caller that has to reach a
    /// fixpoint sweeps until this reports `0`.
    pub fn remove_where_drop(&mut self, is_dead: impl Fn(&N) -> bool) -> usize {
        self.order.clear();
        self.order.extend(
            (0..self.nodes.len())
                .filter(|&index| self.nodes[index].weight.as_ref().is_some_and(&is_dead))
                .map(|index| index as u32),
        );
        let order = core::mem::take(&mut self.order);
        let removed = order.len();
        for &index in &order {
            let id = self.node_id(index);
            self.remove(id);
        }
        self.order = order;
        removed
    }

    /// Every node, ordered so that a node always follows the nodes it depends
    /// on.
    ///
    /// Nodes that do not depend on one another come in an unspecified order.
    /// The graph refuses every edge that would close a cycle, so every node is
    /// listed.
    pub fn topological_order(&self) -> Vec<NodeId> {
        // How many dependencies each node is still waiting for, and the nodes
        // waiting for none.
        let mut pending: Vec<u32> = self
            .nodes
            .iter()
            .map(|slot| {
                if slot.weight.is_some() {
                    slot.dependencies.len() as u32
                } else {
                    0
                }
            })
            .collect();
        let mut ready: Vec<u32> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                (slot.weight.is_some() && slot.dependencies.is_empty()).then_some(index as u32)
            })
            .collect();
        let mut order = Vec::with_capacity(self.live);
        while let Some(index) = ready.pop() {
            order.push(self.node_id(index));
            for &edge in &self.nodes[index as usize].dependents {
                let to = self.edges[edge as usize].to;
                pending[to as usize] -= 1;
                if pending[to as usize] == 0 {
                    ready.push(to);
                }
            }
        }
        debug_assert_eq!(order.len(), self.live, "the graph is acyclic");
        order
    }

    /// Remove the node `id` names together with every edge touching it, and
    /// return its weight.
    ///
    /// Internal: callers go through [`Dag::remove_where_drop`], which selects
    /// what to remove without needing a handle the caller may no longer hold.
    fn remove(&mut self, id: NodeId) -> Option<N> {
        let index = self.resolve(id)?;
        let slot = &mut self.nodes[index];
        let weight = slot.weight.take()?;
        // The incident edges are unlinked from the node at their far end, and
        // this slot's own lists are emptied for the insertion that reuses it.
        let dependents = core::mem::take(&mut slot.dependents);
        let dependencies = core::mem::take(&mut slot.dependencies);
        self.live -= 1;
        self.free_nodes.push(index as u32);
        for edge in dependents {
            let to = self.edges[edge as usize].to;
            self.nodes[to as usize].dependencies.retain(|&e| e != edge);
            self.free_edges.push(edge);
        }
        for edge in dependencies {
            let from = self.edges[edge as usize].from;
            self.nodes[from as usize].dependents.retain(|&e| e != edge);
            self.free_edges.push(edge);
        }
        Some(weight)
    }

    /// Resolve a handle to a slot index, or `None` when it names no node.
    ///
    /// A handle is only ever used while its node lives — the caller holding it
    /// is what keeps the node alive — so a vacant slot means the handle never
    /// came from this graph.
    fn resolve(&self, id: NodeId) -> Option<usize> {
        let index = id.index as usize;
        self.nodes
            .get(index)
            .filter(|slot| slot.weight.is_some())
            .map(|_| index)
    }

    /// Build the handle for a live slot.
    fn node_id(&self, index: u32) -> NodeId {
        NodeId { index }
    }

    /// Whether `to` depends on `from`, both already resolved to slot indices.
    fn has_edge(&self, from: usize, to: usize) -> bool {
        self.nodes[from]
            .dependents
            .iter()
            .any(|&edge| self.edges[edge as usize].to as usize == to)
    }

    /// The edge list `slot` selects, or an empty one when `id` does not
    /// resolve.
    fn incident<'a>(
        &'a self,
        id: NodeId,
        select: impl Fn(&'a NodeSlot<N>) -> &'a Vec<u32>,
    ) -> impl Iterator<Item = u32> + 'a {
        self.resolve(id)
            .map_or(&[][..], |index| select(&self.nodes[index]).as_slice())
            .iter()
            .copied()
    }

    /// Whether following dependents from `start` reaches `target`.
    fn reaches(&mut self, start: usize, target: usize) -> bool {
        if start == target {
            return true;
        }
        let stamp = self.next_stamp();
        let mut stack = core::mem::take(&mut self.stack);
        stack.clear();
        stack.push(start as u32);
        self.visited[start] = stamp;
        let mut found = false;
        while let Some(index) = stack.pop() {
            for &edge in &self.nodes[index as usize].dependents {
                let to = self.edges[edge as usize].to as usize;
                if to == target {
                    found = true;
                    break;
                }
                if self.visited[to] != stamp {
                    self.visited[to] = stamp;
                    stack.push(to as u32);
                }
            }
            if found {
                break;
            }
        }
        self.stack = stack;
        found
    }

    /// Fill [`Dag::order`] with `root` and every node reachable from it by
    /// following dependents, in dependency order.
    ///
    /// The list is built before the caller acts on it, so a weight mutated as
    /// the walk proceeds cannot disturb it.
    fn plan_dependents(&mut self, root: NodeId) {
        self.order.clear();
        let Some(root_index) = self.resolve(root) else {
            return;
        };
        // Post-order: a slot is pushed twice — once to visit it and once to
        // emit it — so it is emitted only after everything reachable from it.
        let stamp = self.next_stamp();
        let mut stack = core::mem::take(&mut self.plan_stack);
        let mut order = core::mem::take(&mut self.order);
        stack.clear();
        stack.push((root_index as u32, false));
        self.visited[root_index] = stamp;
        while let Some((index, expanded)) = stack.pop() {
            if expanded {
                order.push(index);
                continue;
            }
            stack.push((index, true));
            for &edge in &self.nodes[index as usize].dependents {
                let to = self.edges[edge as usize].to;
                if self.visited[to as usize] != stamp {
                    self.visited[to as usize] = stamp;
                    stack.push((to, false));
                }
            }
        }
        self.plan_stack = stack;
        // Post-order emits a node after everything reachable from it, so
        // reversing puts dependencies first — the order a removal returns.
        order.reverse();
        self.order = order;
    }

    /// Take a stamp that no mark in `visited` carries yet, growing the array to
    /// cover the nodes if needed.
    fn next_stamp(&mut self) -> u32 {
        if self.visited.len() < self.nodes.len() {
            self.visited.resize(self.nodes.len(), 0);
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            // The stamps wrapped, so every mark is stale: clear them and start
            // again. Reached once every four billion walks.
            self.visited.fill(0);
            self.stamp = 1;
        }
        self.stamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_slot_is_reused() {
        let mut dag = Dag::new();
        let first = dag.insert("first");
        assert_eq!(first.index(), 0);
        assert_eq!(dag.get(first), Some(&"first"));

        assert_eq!(dag.remove(first), Some("first"));
        assert_eq!(dag.get(first), None, "the slot is vacant");

        // The handle is the slot, so the next node to occupy it answers to the
        // same handle. A caller only ever holds a handle while its node lives,
        // which is what keeps this from being a stale-handle hazard.
        let second = dag.insert("second");
        assert_eq!(second.index(), first.index(), "the slot is reused");
        assert_eq!(dag.get(second), Some(&"second"));
    }

    #[test]
    fn removing_a_node_removes_only_its_edges() {
        let mut dag = Dag::new();
        let base = dag.insert("base");
        let middle = dag.insert("middle");
        let leaf = dag.insert("leaf");
        dag.add_edge(base, middle).unwrap();
        dag.add_edge(middle, leaf).unwrap();

        dag.remove(middle);
        assert_eq!(dag.dependencies(base).count(), 0);
        assert_eq!(
            dag.dependencies(leaf).count(),
            0,
            "the far end of the edge was unlinked too"
        );
        assert_eq!(dag.len(), 2);
    }

    #[test]
    fn an_edge_that_would_close_a_cycle_is_refused() {
        let mut dag = Dag::new();
        let a = dag.insert("a");
        let b = dag.insert("b");
        let c = dag.insert("c");
        dag.add_edge(a, b).unwrap();
        dag.add_edge(b, c).unwrap();

        assert_eq!(dag.add_edge(c, a), Err(EdgeError::Cycle));
        assert_eq!(dag.add_edge(a, a), Err(EdgeError::SelfLoop));
        assert_eq!(dag.len(), 3, "nothing was removed");
        assert_eq!(
            dag.dependencies(b).collect::<Vec<_>>(),
            vec![a],
            "the refused edges were not recorded"
        );
    }

    #[test]
    fn declaring_the_same_edge_twice_records_it_once() {
        let mut dag = Dag::new();
        let base = dag.insert("base");
        let dependent = dag.insert("dependent");
        dag.add_edge(base, dependent).unwrap();
        dag.add_edge(base, dependent).unwrap();

        assert_eq!(dag.dependencies(dependent).collect::<Vec<_>>(), vec![base]);
    }

    #[test]
    fn an_edge_with_a_stale_endpoint_names_the_node_it_refused() {
        let mut dag = Dag::new();
        let base = dag.insert("base");
        let removed = dag.insert("removed");
        dag.remove(removed);

        assert_eq!(
            dag.add_edge(base, removed),
            Err(EdgeError::NoSuchNode(removed))
        );
    }

    #[test]
    fn the_topological_order_puts_every_dependency_first() {
        let mut dag = Dag::new();
        //     base
        //    /    \
        //  left   right
        //    \    /
        //     join
        let base = dag.insert("base");
        let left = dag.insert("left");
        let right = dag.insert("right");
        let join = dag.insert("join");
        dag.add_edge(base, left).unwrap();
        dag.add_edge(base, right).unwrap();
        dag.add_edge(left, join).unwrap();
        dag.add_edge(right, join).unwrap();

        let order = dag.topological_order();
        let position = |id: NodeId| order.iter().position(|&node| node == id).unwrap();
        assert_eq!(order.len(), 4);
        assert!(position(base) < position(left));
        assert!(position(base) < position(right));
        assert!(position(left) < position(join));
        assert!(position(right) < position(join));
    }

    #[test]
    fn the_topological_order_lists_isolated_nodes() {
        let mut dag = Dag::new();
        dag.insert("a");
        dag.insert("b");
        assert_eq!(dag.topological_order().len(), 2);
    }

    #[test]
    fn applying_to_dependents_visits_the_subtree_and_nothing_else() {
        let mut dag = Dag::new();
        let base = dag.insert(0u32);
        let middle = dag.insert(0);
        let leaf = dag.insert(0);
        let unrelated = dag.insert(0);
        dag.add_edge(base, middle).unwrap();
        dag.add_edge(middle, leaf).unwrap();

        dag.for_each_dependent_mut(middle, |count| *count += 1);
        assert_eq!(dag.get(base), Some(&0), "the walk only goes forwards");
        assert_eq!(dag.get(middle), Some(&1));
        assert_eq!(dag.get(leaf), Some(&1));
        assert_eq!(dag.get(unrelated), Some(&0));
    }

    #[test]
    fn removing_where_leaves_the_nodes_that_were_not_selected() {
        let mut dag = Dag::new();
        let base = dag.insert("base");
        let middle = dag.insert("middle");
        let root = dag.insert("root");
        let outside = dag.insert("outside");
        dag.add_edge(base, middle).unwrap();
        dag.add_edge(middle, root).unwrap();

        let removed = dag.remove_where_drop(|&name| name == "middle");
        assert_eq!(removed, 1);
        assert_eq!(dag.get(base), Some(&"base"), "the node it depends on stays");
        assert_eq!(dag.get(outside), Some(&"outside"));
        assert_eq!(dag.get(middle), None);
        assert_eq!(
            dag.get(root),
            Some(&"root"),
            "a dependent is not removed with its dependency"
        );
        assert_eq!(dag.len(), 3);
    }

    /// A sweep selects from the weights present when it starts, so a weight
    /// whose drop changes what is collectable is only seen by the next call.
    #[test]
    fn removing_where_reports_the_count_so_a_caller_can_sweep_to_a_fixpoint() {
        let mut dag = Dag::new();
        // `first` and `second` are both selected; `third` is not, but the
        // closure counts how many sweeps it takes to reach one that removes
        // nothing.
        dag.insert("first");
        dag.insert("second");
        dag.insert("keep");
        let mut sweeps = 0;
        loop {
            sweeps += 1;
            let removed = dag.remove_where_drop(|&name| name != "keep");
            if removed == 0 {
                break;
            }
        }

        assert_eq!(sweeps, 2, "one sweep removed the two, the next found none");
        assert_eq!(dag.len(), 1);
    }
}
