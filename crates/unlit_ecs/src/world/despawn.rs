//! Taking rows out of the world.
//!
//! A despawn is a cascade: the entity's whole subtree goes, children before
//! the parents they lived in, and callbacks may put more entities back in
//! while it runs.

use crate::entity::{Entity, Location};
use crate::error::EntityAccessError;
use crate::hash::EntityHashSet;

use super::World;

/// How many passes a cascade may take before it gives up.
///
/// Each entity runs its callback at most once per cascade, so an ordinary
/// cascade finishes in one pass, or two when a callback puts a child under an
/// entity the first pass had to leave standing. Hitting this bound therefore
/// means the callbacks keep finding new children, not that the cascade is slow.
const MAX_CASCADE_PASSES: usize = 64;

impl World {
    /// Despawn `entity` and its whole subtree, on behalf of `caller`.
    ///
    /// Children are removed before the parent they lived in, so a child's
    /// despawn callback still sees living ancestors.
    ///
    /// The subtree is swept rather than walked once, because a callback runs
    /// while the tree is still its to change: whatever it puts back in is taken
    /// too, instead of being left in a scope whose row is about to go. Each
    /// entity runs its callback once per cascade, so a callback that adds a
    /// child does not hold its own row back. A cascade that has not emptied the
    /// subtree after [`MAX_CASCADE_PASSES`] passes stops with
    /// [`EntityAccessError::CascadeOverflow`]: the entities it removed stay
    /// removed, and the rest of the subtree, `entity` included, stands.
    pub(crate) fn despawn_in(
        &mut self,
        entity: Entity,
        caller: Entity,
    ) -> Result<(), EntityAccessError> {
        if self.is_scaffolding(entity) {
            return Err(EntityAccessError::Scaffolding { entity });
        }
        self.location(entity)?;
        self.check_owns(caller, entity)?;
        // Which entities have already had their callback in this cascade. A
        // row is held back until its children are gone, so the entity that put
        // one back is visited again; without this it would run its callback
        // again, and one that puts a child back on every run would never let
        // the cascade finish. The set is this call's own, so a panic drops it
        // with the stack rather than leaving a world marked as dying.
        let mut called = EntityHashSet::default();
        let mut doomed = Vec::new();
        for _ in 0..MAX_CASCADE_PASSES {
            // Read the subtree every pass: it is the callbacks' to change, and
            // a pass that had to leave a parent standing leaves work behind.
            self.tree.subtree_post_order(entity, &mut doomed);
            for victim in &doomed {
                self.remove_entity(*victim, &mut called);
            }
            // The row of `entity` going is what ends the cascade. A pass that
            // took nothing has not necessarily finished: a callback can put a
            // child under an entity this pass had to leave standing, and the
            // next pass is what takes it.
            if self.location(entity).is_err() {
                return Ok(());
            }
        }
        Err(EntityAccessError::CascadeOverflow {
            entity,
            passes: MAX_CASCADE_PASSES,
        })
    }

    /// Run the despawn callback of `entity` and drop its row and handle.
    ///
    /// A row goes only once its entity has no children left: dropping it
    /// earlier leaves their rows in a scope the tree no longer has a link to,
    /// which is nowhere at all. A callback that puts a child in therefore keeps
    /// the row for the cascade to come back to.
    ///
    /// `called` holds the entities that have already run their callback in this
    /// cascade, so coming back for a held-back row does not run it a second
    /// time. Re-entry from inside a callback needs no such guard: the callback
    /// is out of the row while it runs, so the row it would find holds none.
    fn remove_entity(&mut self, entity: Entity, called: &mut EntityHashSet) {
        if self.location(entity).is_err() {
            return;
        }
        // The callback runs on the first visit, before the children are looked
        // at: a child it puts in has to be swept by a later pass, and the row
        // stays until then.
        if called.insert(entity) {
            self.run_lifecycle::<crate::OnDespawn>(entity);
        }
        // The callback may have changed the tree, including despawning this
        // entity itself. Re-check before touching a row that may be gone, and
        // before dropping one it has just put a child into.
        let Ok(location) = self.location(entity) else {
            return;
        };
        if !self.tree.children(entity).is_empty() {
            return;
        }
        self.tree_mut().detach(entity);
        self.remove_row(location);
        self.release_singleton(entity);
        self.free_entity(entity);
    }

    /// Drop the row at `location`, keeping every other location correct.
    pub(super) fn remove_row(&mut self, location: Location) {
        let archetype = self
            .archetypes
            .get_mut(location.archetype())
            .expect("a live entity's location names a registered archetype");
        if let Some(moved) = archetype.swap_remove(location.row()) {
            self.entities_write().set_location(
                moved,
                Location {
                    archetype: location.archetype(),
                    row: location.row() as u32,
                },
            );
        }
    }

    /// Release the singleton key `entity` holds, if it holds one.
    ///
    /// Called once the row is gone, which is after the entity's `OnDespawn` has
    /// run: the key stays held through the callback, so nothing can claim it
    /// while its holder is still being removed.
    pub(super) fn release_singleton(&mut self, entity: Entity) {
        if let Some(type_id) = self.singleton_keys.remove(&entity) {
            self.singleton_holders.remove(&type_id);
        }
    }

    pub(super) fn free_entity(&mut self, entity: Entity) -> bool {
        self.entities_write().free(entity)
    }
}
