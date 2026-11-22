//! Taking rows out of the world.
//!
//! A despawn is a cascade: the entity's whole subtree goes, children before
//! the parents they lived in, and callbacks may put more entities back in
//! while it runs.

use crate::entity::{Entity, Location};
use crate::error::EntityAccessError;
use crate::hash::EntityHashSet;

use super::World;

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
    /// child does not hold its own row back. A callback that adds a fresh
    /// entity on every pass keeps the cascade going the way a command that
    /// queues another keeps [`World::apply`] going: neither is bounded.
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
        loop {
            // Read the subtree every pass: it is the callbacks' to change, and
            // a pass that had to leave a parent standing leaves work behind.
            let doomed = self.tree.subtree_post_order(entity);
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
    /// is out of the row while it runs, so the row it would find holds the
    /// placeholder.
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

    pub(super) fn free_entity(&mut self, entity: Entity) -> bool {
        self.entities_write().free(entity)
    }
}
