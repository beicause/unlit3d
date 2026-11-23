//! Plain access to what the world stores: the entity table, the tree, and the
//! archetype pool.

use core::any::TypeId;
use core::cell::{Ref, RefMut};

use crate::archetype::Archetypes;
use crate::entity::{Entities, Entity, Location};
use crate::error::EntityAccessError;
use crate::scope::ScopeTree;

use super::World;

impl World {
    /// A handle to an entity that does not exist yet, named `name`.
    ///
    /// The handle becomes live when a bundle is put on it, either with a
    /// scope's spawn or by applying a queued [`crate::Commands::spawn`].
    /// Reserving is where the caller says what the entity is, so the bundle
    /// put on it later does not rename it.
    pub fn reserve_entity(&self, name: &'static str) -> Entity {
        self.entities_write().reserve(name)
    }

    /// Release a handle from [`World::reserve_entity`] that will never be
    /// spawned.
    ///
    /// A reserved handle that is dropped without ever being spawned keeps its
    /// index out of the pool; releasing it puts the index back, so a later
    /// spawn reuses it. Returns whether `entity` was such a handle.
    pub fn release_entity(&mut self, entity: Entity) -> bool {
        if !self.entities_read().is_reserved(entity) {
            return false;
        }
        self.entities_write().free(entity)
    }

    /// The name of a component type that has entered the world, as
    /// [`core::any::type_name`] reported it, or `None` when no entity has ever
    /// carried it.
    #[must_use]
    pub fn type_name(&self, type_id: TypeId) -> Option<&'static str> {
        self.names.get(&type_id).copied()
    }

    /// Where a live entity's components live.
    #[inline]
    pub(crate) fn location(&self, entity: Entity) -> Result<Location, EntityAccessError> {
        self.entities_read().location(entity)
    }

    /// Whether `entity` is one of the rows the world holds for itself.
    ///
    /// The root and the shared scope are such rows: they are addressable and
    /// they can carry components, but they are neither counted, queried, moved
    /// nor despawned. A handle that names nothing is not internal — asking
    /// about a dead row is how a caller finds out it is dead.
    pub(crate) fn is_internal(&self, entity: Entity) -> bool {
        let Ok(location) = self.location(entity) else {
            return false;
        };
        self.archetypes
            .get(location.archetype())
            .is_ok_and(|archetype| archetype.is_internal())
    }

    /// What a live entity is called, read from the row that stores it.
    ///
    /// This is the only durable copy of the label: the allocator keeps no name,
    /// and a handle rebuilt from bare bits carries none.
    pub fn entity_name(&self, entity: Entity) -> Result<&'static str, EntityAccessError> {
        let location = self.location(entity)?;
        Ok(self.archetypes.get(location.archetype())?.entities()[location.row()].name())
    }

    /// The live entity holding the singleton key `type_id`, if any.
    #[must_use]
    pub(crate) fn singleton_holder(&self, type_id: TypeId) -> Option<Entity> {
        self.singleton_holders.get(&type_id).copied()
    }

    /// The archetypes, including the empty one.
    ///
    /// This is the storage itself, not a view of one scope: it holds every row
    /// of every scope. Reading it through a [`crate::Scope`] is what filters by
    /// visibility.
    #[must_use]
    pub fn archetypes(&self) -> &Archetypes {
        &self.archetypes
    }

    #[inline]
    pub(crate) fn entities_read(&self) -> Ref<'_, Entities> {
        self.entities
            .try_borrow()
            .unwrap_or_else(|_| panic!("the entity table is already exclusively borrowed"))
    }

    pub(crate) fn entities_write(&self) -> RefMut<'_, Entities> {
        self.entities
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the entity table is already borrowed"))
    }

    #[inline]
    pub(super) fn tree_mut(&mut self) -> &mut ScopeTree {
        &mut self.tree
    }
}
