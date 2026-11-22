//! The tree: which entity lives in which, and who may see or move it.
//!
//! A scope is an entity, and where it lives is its parent, so a world is one
//! tree with two roots hung on the world itself: the root, which sees
//! everything, and the shared scope, whose subtree every scope sees. The
//! predicates here are what queries, scopes and structural changes ask before
//! they touch a row.

use crate::entity::Entity;
use crate::error::EntityAccessError;

use super::World;

impl World {
    /// The entity that owns the world, whose scope sees everything.
    #[must_use]
    pub fn root(&self) -> Entity {
        self.root
    }

    /// The entity whose subtree every scope sees.
    #[must_use]
    pub fn shared(&self) -> Entity {
        self.shared
    }

    /// Whether `entity` is in the shared subtree, itself included.
    #[must_use]
    pub fn is_shared(&self, entity: Entity) -> bool {
        entity == self.shared || self.is_descendant(self.shared, entity)
    }

    /// Whether `entity` is one of the tree's two scaffolding entities.
    ///
    /// This is narrower than [`World::is_shared`], which covers the whole
    /// shared subtree: the entities a caller spawns there are content, and a
    /// scope counts and queries them like any other.
    #[must_use]
    pub fn is_scaffolding(&self, entity: Entity) -> bool {
        entity == self.root || entity == self.shared
    }

    /// Whether `entity` is strictly below `ancestor` in the tree.
    #[must_use]
    pub fn is_descendant(&self, ancestor: Entity, entity: Entity) -> bool {
        self.tree.is_descendant(ancestor, entity)
    }

    /// Whether a scope of `caller` sees `candidate`.
    ///
    /// The shared subtree reaches every scope, the root sees everything, and a
    /// scope sees itself and everything below it. A handle that is not alive is
    /// visible from nowhere, the root included: seeing a place is not the same
    /// as there being something in it.
    #[must_use]
    pub fn visible(&self, caller: Entity, candidate: Entity) -> bool {
        if !self.entities_read().contains(candidate) {
            return false;
        }
        // The root is the hot case: it sees everything, so it is answered
        // before the parent-chain walks below.
        caller == self.root
            || candidate == caller
            || self.is_shared(candidate)
            || self.is_descendant(caller, candidate)
    }

    /// Whether a scope of `caller` may despawn or relink `entity`.
    ///
    /// Ownership is stricter than visibility: the shared subtree is readable
    /// from everywhere but only the root may move or remove it.
    #[must_use]
    pub fn owns(&self, caller: Entity, entity: Entity) -> bool {
        self.check_owns(caller, entity).is_ok()
    }

    /// Why a scope of `caller` may not despawn or relink `entity`.
    ///
    /// The same rule as [`World::owns`], answered with the reason instead of
    /// one bit: an entity outside the subtree is another scope's to change, and
    /// one in the shared subtree is the root's alone.
    pub(super) fn check_owns(
        &self,
        caller: Entity,
        entity: Entity,
    ) -> Result<(), EntityAccessError> {
        if caller == self.root {
            return Ok(());
        }
        if self.is_shared(entity) {
            return Err(EntityAccessError::NotMovable {
                entity,
                scope: caller,
            });
        }
        if entity != caller && !self.is_descendant(caller, entity) {
            return Err(EntityAccessError::NotOwned {
                entity,
                scope: caller,
            });
        }
        Ok(())
    }

    /// The parent of a live entity.
    pub fn parent_scope(&self, entity: Entity) -> Result<Entity, EntityAccessError> {
        self.tree.parent(entity)
    }

    /// The direct children of `entity`.
    ///
    /// Answers [`EntityAccessError::NotAlive`] for a handle that names no live
    /// entity, which an empty list would otherwise hide.
    pub fn children(&self, entity: Entity) -> Result<Vec<Entity>, EntityAccessError> {
        self.location(entity)?;
        Ok(self.tree.children(entity).to_vec())
    }

    /// The first row of `archetype` that holds content rather than scaffolding.
    ///
    /// This is where a query starts scanning an archetype, and what a count
    /// leaves out. It is a row boundary rather than a per-row test so that
    /// walking a row costs no identity check: the two roots share their
    /// archetype with every entity spawned bare, which would otherwise need one
    /// comparison per row.
    #[inline]
    #[must_use]
    pub(crate) fn content_start(&self, archetype: u32) -> usize {
        if archetype == self.scaffolding {
            Self::SCAFFOLDING_ROWS
        } else {
            0
        }
    }
}
