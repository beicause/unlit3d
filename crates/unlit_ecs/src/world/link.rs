//! Moving a live entity from one scope to another.
//!
//! A link puts an entity under a new parent, an unlink takes it out and puts it
//! in a scope of its own choosing. Neither runs a lifecycle callback: the
//! entity's components do not change, only the subtree it sits in.

use crate::entity::Entity;
use crate::error::EntityAccessError;

use super::World;

impl World {
    /// Make `child` a child of `parent` on behalf of `caller`.
    ///
    /// Neither end may be a root or the shared scope itself, both must be
    /// alive, and `parent` may not be inside `child`'s subtree: moving a scope
    /// into its own descendant would cut the tree in two. The shared subtree is
    /// readable from everywhere but it is the root's to rearrange, so only a
    /// root caller may move an entity into or out of it.
    pub(crate) fn link_in(
        &mut self,
        child: Entity,
        parent: Entity,
        caller: Entity,
    ) -> Result<(), EntityAccessError> {
        if self.is_internal(child) {
            return Err(EntityAccessError::Internal { entity: child });
        }
        if self.is_internal(parent) {
            return Err(EntityAccessError::Internal { entity: parent });
        }
        self.location(child)?;
        self.location(parent)?;
        self.check_owns(caller, child)?;
        self.check_owns(caller, parent)?;
        if child == parent || self.is_descendant(child, parent) {
            return Err(EntityAccessError::WouldCycle { child, parent });
        }
        self.write_scope(child, parent);
        Ok(())
    }

    /// Detach `child` from its parent and put it in `scope`, on behalf of
    /// `caller`.
    ///
    /// `child` may not be the scope itself: that would make it its own parent,
    /// which is how the tree says "this is a root". The world's own rows do not move,
    /// and only a root caller may take an entity out of the shared subtree.
    pub(crate) fn unlink_in(
        &mut self,
        child: Entity,
        scope: Entity,
        caller: Entity,
    ) -> Result<(), EntityAccessError> {
        if self.is_internal(child) {
            return Err(EntityAccessError::Internal { entity: child });
        }
        if child == scope {
            // Its own parent is how the tree says "this is a root".
            return Err(EntityAccessError::WouldCycle {
                child,
                parent: scope,
            });
        }
        self.location(child)?;
        self.location(scope)?;
        self.check_owns(caller, child)?;
        if self.parent_scope(child)? == scope {
            return Ok(());
        }
        self.write_scope(child, scope);
        Ok(())
    }

    /// Move a live entity into `scope` and reindex it.
    fn write_scope(&mut self, entity: Entity, scope: Entity) {
        let location = self
            .location(entity)
            .unwrap_or_else(|_| panic!("{entity:?} has no row to move"));
        let old = self
            .tree
            .parent(entity)
            .unwrap_or_else(|_| panic!("{entity:?} is alive but not in the tree"));
        if old == scope {
            return;
        }
        self.archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype")
            .move_scope(location.row(), old, scope);
        self.tree_mut().attach(entity, scope);
    }
}
