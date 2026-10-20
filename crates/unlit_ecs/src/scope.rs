//! Entity scopes: which entity an entity lives in, and the tree that makes.
//!
//! Every entity carries a [`ScopeOf`] component naming its parent entity, so an
//! entity's scope is another entity rather than a separate identifier space.
//! The tree that results is what a [`Scope`](crate::Scope) sees
//! through: a scope of a scope sees that scope's whole subtree.
//!
//! The component is the truth. [`ScopeTree`] is a derived index of the same
//! relation — parents and children as plain handles — kept up to date so that
//! walking the tree never borrows a component cell. A query that holds
//! `&mut ScopeOf` of one entity must not make the visibility check of another
//! panic, and reading the index instead is what guarantees that.
//!
//! Two entities are special: the root owns the world, and the shared scope's
//! subtree is visible to every scope.

use crate::entity::Entity;
use crate::hash::EntityHashMap;

/// The parent entity of the entity that carries it: the scope it lives in.
///
/// The root entity's scope is itself, which is where walking up the tree stops.
///
/// Only the world writes this: a bundle cannot carry it, the field is private,
/// and moving an entity is [`ScopeMut::link`](crate::ScopeMut::link)'s job. The
/// recorded parent, the tree index and the archetype's per-scope row counts have
/// to agree, so they have one writer rather than three.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct ScopeOf(
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::EntityProxy))] Entity,
);

impl ScopeOf {
    /// The component that records `parent` as an entity's parent.
    pub(crate) const fn new(parent: Entity) -> Self {
        Self(parent)
    }

    /// The parent entity.
    #[must_use]
    pub fn parent(self) -> Entity {
        self.0
    }
}

#[cfg(feature = "reflect")]
inventory::submit! { crate::reflect::ComponentEntry::scope() }

/// The parent and child links of every live entity.
///
/// Both directions are written by the world alongside the [`ScopeOf`]
/// component; keeping them here is what lets the visibility check run without
/// borrowing a component cell.
#[derive(Default)]
pub(crate) struct ScopeTree {
    parents: EntityHashMap<Entity>,
    children: EntityHashMap<Vec<Entity>>,
}

impl ScopeTree {
    /// Record that `entity` lives in `parent`, dropping its old link.
    pub(crate) fn attach(&mut self, entity: Entity, parent: Entity) {
        self.detach(entity);
        self.parents.insert(entity, parent);
        if parent != entity {
            self.children.entry(parent).or_default().push(entity);
        }
    }

    /// Drop `entity`'s link to its parent, keeping its children linked to it.
    pub(crate) fn detach(&mut self, entity: Entity) {
        let Some(parent) = self.parents.remove(&entity) else {
            return;
        };
        let Some(siblings) = self.children.get_mut(&parent) else {
            return;
        };
        if let Some(position) = siblings.iter().position(|&child| child == entity) {
            siblings.swap_remove(position);
        }
        if siblings.is_empty() {
            self.children.remove(&parent);
        }
    }

    /// The parent of `entity`, or `None` when it is not in the tree.
    #[must_use]
    pub(crate) fn parent(&self, entity: Entity) -> Option<Entity> {
        self.parents.get(&entity).copied()
    }

    /// The direct children of `entity`.
    pub(crate) fn children(&self, entity: Entity) -> &[Entity] {
        self.children
            .get(&entity)
            .map_or(&[], |children| children.as_slice())
    }

    /// Whether `entity` is strictly below `ancestor` in the tree.
    ///
    /// A walk up the parent chain rather than an interval comparison: the tree
    /// is as deep as a scene nests, which is a handful of levels, while an
    /// interval index would have to be rebuilt over the whole world on every
    /// spawn. The walk is bounded by the number of nodes so that a corrupted
    /// index cannot loop forever.
    #[must_use]
    pub(crate) fn is_descendant(&self, ancestor: Entity, entity: Entity) -> bool {
        let mut current = entity;
        for _ in 0..=self.parents.len() {
            let Some(&parent) = self.parents.get(&current) else {
                return false;
            };
            if parent == current {
                // The root's self-loop: the chain ends without reaching it.
                return false;
            }
            if parent == ancestor {
                return true;
            }
            current = parent;
        }
        false
    }

    /// The subtree rooted at `root`, deepest first.
    ///
    /// Post-order is what a cascade despawn needs: a child is removed before
    /// the parent it lived in, so a child's callback still sees living
    /// ancestors.
    pub(crate) fn subtree_post_order(&self, root: Entity) -> Vec<Entity> {
        let mut ordered = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((entity, expanded)) = stack.pop() {
            if expanded {
                ordered.push(entity);
                continue;
            }
            stack.push((entity, true));
            for &child in self.children(entity) {
                stack.push((child, false));
            }
        }
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(index: u32) -> Entity {
        Entity::from_raw(index, 0)
    }

    /// A chain 0 <- 1 <- 2 and a sibling 3 of 1, both under the root 0.
    fn fixture() -> ScopeTree {
        let mut tree = ScopeTree::default();
        tree.attach(entity(0), entity(0));
        tree.attach(entity(1), entity(0));
        tree.attach(entity(2), entity(1));
        tree.attach(entity(3), entity(0));
        tree
    }

    #[test]
    fn a_descendant_is_every_entity_below_an_ancestor() {
        let tree = fixture();
        assert!(tree.is_descendant(entity(0), entity(2)), "grandchild");
        assert!(tree.is_descendant(entity(1), entity(2)), "child");
        assert!(!tree.is_descendant(entity(0), entity(0)), "not itself");
        assert!(!tree.is_descendant(entity(2), entity(1)), "not an ancestor");
        assert!(!tree.is_descendant(entity(1), entity(3)), "not a sibling");
    }

    #[test]
    fn the_root_walks_up_to_itself_and_stops() {
        let tree = fixture();
        assert!(!tree.is_descendant(entity(0), entity(0)));
        assert_eq!(tree.parent(entity(0)), Some(entity(0)));
    }

    #[test]
    fn detaching_keeps_the_children_of_the_moved_entity() {
        let mut tree = fixture();
        tree.detach(entity(1));
        assert_eq!(tree.parent(entity(1)), None);
        assert_eq!(tree.parent(entity(2)), Some(entity(1)), "the child stays");
        assert!(!tree.children(entity(0)).contains(&entity(1)));
    }

    #[test]
    fn attaching_again_moves_the_entity_to_its_new_parent() {
        let mut tree = fixture();
        tree.attach(entity(2), entity(3));
        assert_eq!(tree.parent(entity(2)), Some(entity(3)));
        assert_eq!(tree.children(entity(1)), &[] as &[Entity]);
        assert_eq!(tree.children(entity(3)), [entity(2)]);
    }

    #[test]
    fn a_subtree_is_ordered_deepest_first() {
        let tree = fixture();
        let ordered = tree.subtree_post_order(entity(0));
        // Children are pushed in insertion order and popped last-first, so a
        // node's subtrees come out in reverse insertion order.
        assert_eq!(ordered, [entity(3), entity(2), entity(1), entity(0)]);
    }

    #[test]
    fn moving_a_node_moves_its_subtree_with_it() {
        let mut tree = fixture();
        // 2 moves from under 1 to under 3, and 1 keeps no children.
        tree.attach(entity(2), entity(3));
        assert_eq!(tree.parent(entity(2)), Some(entity(3)));
        assert_eq!(tree.children(entity(1)), &[] as &[Entity]);
        assert_eq!(tree.children(entity(3)), [entity(2)]);
        assert!(tree.is_descendant(entity(0), entity(2)), "still reachable");
    }
}
