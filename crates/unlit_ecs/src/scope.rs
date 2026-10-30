//! Entity scopes: which entity an entity lives in, and the tree that makes.
//!
//! An entity's scope is another entity, not a separate identifier space, so the
//! relation between entities is a tree of handles. The tree that results is what
//! a [`Scope`](crate::Scope) sees through: a scope of a scope sees that scope's
//! whole subtree.
//!
//! The tree is the one copy of the relation. It is kept here rather than in a
//! component because visibility is decided on every access, and a component
//! would have to be borrowed to answer it: a caller holding a component of one
//! entity must not make the visibility check of another entity depend on what is
//! borrowed. Only the world writes the tree, so the recorded parent and the
//! archetype's per-scope row counts have one writer rather than several.
//!
//! Two entities are special: the root owns the world, and the shared scope's
//! subtree is visible to every scope.

use crate::entity::Entity;
use crate::error::EntityAccessError;
use crate::hash::EntityHashMap;

/// The parent and child links of every live entity.
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

    /// The parent of `entity`.
    ///
    /// Every live entity is attached to the tree in the same step that gives it
    /// a row, so a handle missing from it names no live entity.
    pub(crate) fn parent(&self, entity: Entity) -> Result<Entity, EntityAccessError> {
        self.parents
            .get(&entity)
            .copied()
            .ok_or(EntityAccessError::NotAlive { entity })
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
