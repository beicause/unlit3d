//! The world: entities, their components, and the queries over them.
//!
//! There are two worlds, which differ only in the cells they store components
//! in:
//!
//! - [`LocalWorld`](crate::LocalWorld) (the [`World`] default) stores components in
//!   `RefCell`s and therefore cannot leave its thread. This is the world that
//!   holds a renderer or any other `!Send` state.
//! - [`SendWorld`](crate::SendWorld) stores components in `RwLock`s, so it is
//!   `Send + Sync`
//!   and several threads may read and write it at once.
//!
//! Reading and writing components needs only `&World`, because every value
//! lives in a cell. Structural changes — spawning, despawning, adding or
//! removing components, and reparenting — need `&mut World`. A callback that
//! only has a shared world can queue those changes instead with
//! [`World::queue`] and apply them later with [`World::apply`].
//!
//! A borrow conflict is a panic, not a compile error: asking for a component
//! that is already borrowed, or fetching the same component as `&mut` twice
//! in one query, reports the component's name and stops. This is the trade for
//! not doing access analysis.

use core::any::{Any, TypeId};

use crate::archetype::{Archetype, Archetypes};
use crate::bundle::{ArchetypeBuilder, Bundle};
use crate::command::{Command as _, Commands};
use crate::component::{AddableComponent, InsertError, RemoveError};
use crate::ctx::Ctx;
use crate::entity::{Entities, Entity, Location};
use crate::hash::TypeIdHashMap;
use crate::mode::{Cell, CellRef, CellRefMut, Column, ColumnErase, Mode};
use crate::query::{Query, QueryIter};

/// Entities and their components.
pub struct World<M: Mode> {
    entities: M::Cell<Entities>,
    archetypes: Archetypes<M>,
    /// Column constructors, one per component type that ever entered the world.
    ctors: TypeIdHashMap<fn() -> Box<M::ErasedColumn>>,
    commands: M::Cell<Vec<Box<M::ErasedCommand>>>,
}

/// The exclusive borrow of a world's command queue.
pub(crate) type CommandsGuard<'w, M> =
    <<M as Mode>::Cell<Vec<Box<<M as Mode>::ErasedCommand>>> as Cell<
        Vec<Box<<M as Mode>::ErasedCommand>>,
    >>::RefMut<'w>;

impl<M: Mode> Default for World<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Mode> World<M> {
    /// An empty world holding only the empty archetype.
    pub fn new() -> Self {
        let mut ctors: TypeIdHashMap<fn() -> Box<M::ErasedColumn>> = TypeIdHashMap::default();
        // The hierarchy components are the ones a live entity gains and loses,
        // so their columns must always be constructible.
        ctors.insert(
            TypeId::of::<crate::hierarchy::Children>(),
            <M as Mode>::children_column,
        );
        ctors.insert(
            TypeId::of::<crate::hierarchy::ChildOf>(),
            <M as Mode>::child_of_column,
        );
        Self {
            entities: M::Cell::new(Entities::default()),
            archetypes: Archetypes::new(),
            ctors,
            commands: M::Cell::new(Vec::new()),
        }
    }

    // -- entity bookkeeping ------------------------------------------------

    /// Number of live entities.
    pub fn len(&self) -> usize {
        self.entities_read().len()
    }

    /// Whether there is no live entity.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the handle refers to a live entity.
    pub fn contains(&self, entity: Entity) -> bool {
        self.entities_read().contains(entity)
    }

    /// Whether the entity is alive and has component `C`.
    pub fn has<C: 'static>(&self, entity: Entity) -> bool {
        self.get::<C>(entity).is_some()
    }

    /// The component of `entity`, or `None` when it is missing or the
    /// entity is not alive.
    ///
    /// Panics when the component is already exclusively borrowed.
    #[must_use]
    pub fn get<C: 'static>(&self, entity: Entity) -> Option<CellRef<'_, M, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype)
            .cell::<C>(location.row())?;
        Some(
            cell.try_read()
                .unwrap_or_else(|| borrow_conflict::<C>("read")),
        )
    }

    /// The component of `entity`, exclusively.
    ///
    /// Panics when the component is already borrowed, shared or exclusive.
    #[must_use]
    pub fn get_mut<C: 'static>(&self, entity: Entity) -> Option<CellRefMut<'_, M, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype)
            .cell::<C>(location.row())?;
        Some(
            cell.try_write()
                .unwrap_or_else(|| borrow_conflict::<C>("write")),
        )
    }

    /// Run `f` on the component of `entity`.
    ///
    /// This is the scoped form of [`World::get_mut`]: the borrow ends with the
    /// closure, so nothing else can trip over it.
    ///
    /// `````
    /// # use unlit_ecs::LocalWorld;
    /// let mut world = LocalWorld::new();
    /// let entity = world.spawn((1u32,));
    /// world.with_mut::<u32, _>(entity, |value| *value += 10).unwrap();
    /// assert_eq!(world.with_mut::<u32, _>(entity, |value| *value).unwrap(), 11);
    /// `````
    #[must_use]
    pub fn with_mut<C: 'static, R>(
        &self,
        entity: Entity,
        f: impl FnOnce(&mut C) -> R,
    ) -> Option<R> {
        let mut guard = self.get_mut::<C>(entity)?;
        Some(f(&mut guard))
    }

    // -- structural changes ------------------------------------------------

    /// Spawn an entity with `bundle`.
    ///
    /// `````
    /// # use unlit_ecs::LocalWorld;
    /// let mut world = LocalWorld::new();
    /// let entity = world.spawn((1u32, true));
    /// assert!(world.has::<u32>(entity));
    /// `````
    pub fn spawn<B: Bundle<M>>(&mut self, bundle: B) -> Entity {
        let entity = self.entities_write().alloc();
        self.spawn_at(entity, bundle);
        entity
    }

    /// Spawn an entity with no components.
    pub fn spawn_empty(&mut self) -> Entity {
        self.spawn(())
    }

    /// Spawn `bundle` on a handle from [`World::reserve_entity`].
    ///
    /// Panics when the handle already refers to a live entity. This is a hard
    /// check rather than a debug assertion: a second spawn would leave the
    /// entity with two rows, one of them unreachable and impossible to free.
    pub fn spawn_at<B: Bundle<M>>(&mut self, entity: Entity, bundle: B) {
        assert!(
            !self.entities_read().contains(entity),
            "{entity:?} is already spawned"
        );
        let mut builder = ArchetypeBuilder::new();
        bundle.put_into(&mut builder);
        let (types, values, ctors) = builder.finish();
        for (type_id, ctor) in ctors {
            self.ctors.entry(type_id).or_insert(ctor);
        }
        let archetype = self.archetype_for_types(&types);
        let target = self.archetypes.get_mut(archetype);
        for (type_id, value) in values {
            target.push_erased(type_id, value);
        }
        target.push_entity(entity);
        let row = (target.len() - 1) as u32;
        self.entities_write().take_reserved(entity);
        self.entities_write()
            .set_location(entity, Location { archetype, row });
    }

    /// A handle to an entity that does not exist yet.
    ///
    /// The handle becomes live when a bundle is put on it, either with
    /// [`World::spawn_at`] or by applying a queued [`Commands::spawn`].
    pub fn reserve_entity(&self) -> Entity {
        self.entities_write().reserve()
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
        self.entities_write().free(entity).is_ok()
    }

    /// Despawn `entity` and every descendant of it.
    ///
    /// Returns `false` when the entity was not alive. Despawning cascades, so
    /// a subtree leaves as a unit.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        if !self.contains(entity) {
            return false;
        }
        self.despawn_subtree(entity);
        true
    }

    /// Add `component` to a live entity.
    ///
    /// Only a component that implements [`AddableComponent`] may be added at
    /// run time; an entity's other components are fixed when it is spawned.
    ///
    /// `````
    /// # use unlit_ecs::{AddableComponent, LocalWorld};
    /// #[derive(Debug, PartialEq)]
    /// struct Health(u32);
    /// impl AddableComponent for Health {}
    ///
    /// let mut world = LocalWorld::new();
    /// let entity = world.spawn(());
    /// world.insert(entity, Health(3)).unwrap();
    /// assert!(world.has::<Health>(entity));
    /// `````
    pub fn insert<C: AddableComponent>(
        &mut self,
        entity: Entity,
        component: C,
    ) -> Result<(), InsertError>
    where
        Column<M, C>: ColumnErase<M>,
    {
        if self.get::<C>(entity).is_some() {
            return Err(InsertError::AlreadyPresent {
                component: core::any::type_name::<C>(),
            });
        }
        let location = self
            .entities_read()
            .location(entity)
            .ok_or(InsertError::NoSuchEntity)?;
        self.ctors
            .entry(TypeId::of::<C>())
            .or_insert(Column::<M, C>::eraser());
        let target = self.archetype_with_added(location.archetype, TypeId::of::<C>());
        self.move_row(
            location,
            target,
            Some((TypeId::of::<C>(), Box::new(component))),
        );
        Ok(())
    }

    /// Remove component `C` from a live entity and return it.
    ///
    /// Only a component that implements [`AddableComponent`] may be removed
    /// at run time.
    pub fn remove<C: AddableComponent>(&mut self, entity: Entity) -> Result<C, RemoveError>
    where
        Column<M, C>: ColumnErase<M>,
    {
        let location = self
            .entities_read()
            .location(entity)
            .ok_or(RemoveError::NoSuchEntity)?;
        if self.get::<C>(entity).is_none() {
            return Err(RemoveError::Missing {
                component: core::any::type_name::<C>(),
            });
        }
        let target = self.archetype_without(location.archetype, TypeId::of::<C>());
        let removed = self.move_row(location, target, None);
        Ok(*removed
            .expect("a removed component is always returned")
            .downcast::<C>()
            .expect("the removed component has its own type"))
    }

    /// Add an already boxed component of component type `type_id`.
    ///
    /// This is the form the hierarchy uses: it knows its components by type id,
    /// and their column constructors are always registered.
    pub(crate) fn insert_erased(
        &mut self,
        entity: Entity,
        type_id: TypeId,
        value: Box<dyn Any>,
    ) -> Result<(), InsertError> {
        let location = self
            .entities_read()
            .location(entity)
            .ok_or(InsertError::NoSuchEntity)?;
        if self
            .archetypes
            .get(location.archetype)
            .column_index(type_id)
            .is_some()
        {
            return Err(InsertError::AlreadyPresent {
                component: "a component",
            });
        }
        let target = self.archetype_with_added(location.archetype, type_id);
        self.move_row(location, target, Some((type_id, value)));
        Ok(())
    }

    /// Remove the component of component type `type_id` and return it boxed.
    pub(crate) fn remove_erased(
        &mut self,
        entity: Entity,
        type_id: TypeId,
    ) -> Result<Box<dyn Any>, RemoveError> {
        let location = self
            .entities_read()
            .location(entity)
            .ok_or(RemoveError::NoSuchEntity)?;
        if self
            .archetypes
            .get(location.archetype)
            .column_index(type_id)
            .is_none()
        {
            return Err(RemoveError::Missing {
                component: "a component",
            });
        }
        let target = self.archetype_without(location.archetype, type_id);
        self.move_row(location, target, None)
            .ok_or(RemoveError::Missing {
                component: "a component",
            })
    }

    // -- queries -----------------------------------------------------------

    /// Iterate the entities matching `Q`.
    ///
    /// `````
    /// # use unlit_ecs::{LocalWorld, Query};
    /// let mut world = LocalWorld::new();
    /// world.spawn((1u32,));
    /// world.spawn((2u32, true));
    /// let values: Vec<u32> = world
    ///     .query::<(&u32, &bool)>()
    ///     .map(|(_, (value, _))| *value)
    ///     .collect();
    /// assert_eq!(values, [2]);
    /// `````
    pub fn query<Q: Query>(&self) -> QueryIter<'_, M, Q> {
        QueryIter::new(self)
    }

    /// Run `f` for every entity matching `Q`.
    ///
    /// Each item is fetched and dropped before the next one, so a query may ask
    /// for `&mut` components. Use [`World::query`] when two items must be
    /// alive at the same time.
    pub fn for_each<Q: Query, F>(&self, mut f: F)
    where
        F: FnMut(Q::Item<'_, M>),
    {
        for archetype in self.archetypes.iter() {
            if !Q::matches(archetype) {
                continue;
            }
            // Resolve the query's columns once, then walk the rows.
            let state = Q::fetch_state(archetype);
            for row in 0..archetype.len() {
                f(Q::fetch(&state, row));
            }
        }
    }

    /// Iterate the archetypes, including the empty one.
    pub fn archetypes(&self) -> impl Iterator<Item = &Archetype<M>> {
        self.archetypes.iter()
    }

    /// The archetypes, including the empty one, as a slice.
    pub(crate) fn archetypes_slice(&self) -> &[Archetype<M>] {
        self.archetypes.as_slice()
    }

    /// The number of archetypes, including the empty one.
    pub fn archetype_count(&self) -> usize {
        self.archetypes.len()
    }

    // -- deferred structural changes ---------------------------------------

    /// A queue of structural changes to apply later.
    ///
    /// This is the form a callback that only has `&World` can use: queue the
    /// change, then call [`World::apply`] once the callback has returned.
    pub fn queue(&self) -> Commands<'_, M> {
        Commands::new(self)
    }

    /// Apply every queued command, in order.
    ///
    /// Commands queued while applying are applied as well, after the ones
    /// already in the queue.
    pub fn apply(&mut self) {
        loop {
            let mut batch = std::mem::take(&mut *self.commands_write());
            if batch.is_empty() {
                return;
            }
            for command in batch.drain(..) {
                command.apply(self);
            }
        }
    }

    /// Panic unless the handle refers to a live entity.
    pub(crate) fn expect_alive(&self, entity: Entity) {
        assert!(self.contains(entity), "{entity:?} is not a live entity");
    }

    /// A scope for invoking a behaviour component on `entity` and its
    /// descendants, with the data isolation enforced.
    pub fn ctx(&self, entity: Entity) -> Ctx<'_, M> {
        Ctx::new(self, entity)
    }

    /// Run behaviour component `B` of one entity, if it has one, with a
    /// scope the entity is allowed to see.
    ///
    /// It says nothing about which entity to call: a caller that wants to reach
    /// a subtree walks [`World::descendants`](Self::descendants) and calls
    /// each one.
    pub fn call<B: 'static, R>(
        &self,
        entity: Entity,
        f: impl FnOnce(&mut B, Ctx<'_, M>) -> R,
    ) -> Option<R> {
        let mut component = self.get_mut::<B>(entity)?;
        let ctx = Ctx::new(self, entity);
        Some(f(&mut component, ctx))
    }

    // -- internals ---------------------------------------------------------

    pub(crate) fn entities_read(&self) -> <M::Cell<Entities> as Cell<Entities>>::Ref<'_> {
        self.entities
            .try_read()
            .unwrap_or_else(|| panic!("the entity table is already exclusively borrowed"))
    }

    pub(crate) fn entities_write(&self) -> <M::Cell<Entities> as Cell<Entities>>::RefMut<'_> {
        self.entities
            .try_write()
            .unwrap_or_else(|| panic!("the entity table is already borrowed"))
    }

    pub(crate) fn commands_write(&self) -> CommandsGuard<'_, M> {
        self.commands
            .try_write()
            .unwrap_or_else(|| panic!("the command queue is already borrowed"))
    }

    /// Append a command to the deferred queue.
    pub(crate) fn push_command(&self, command: Box<M::ErasedCommand>) {
        self.commands_write().push(command);
    }

    /// The archetype with exactly `types` (sorted), creating it when it is
    /// new. Every type must have a registered column constructor.
    fn archetype_for_types(&mut self, types: &[TypeId]) -> u32 {
        if let Some(id) = self.archetypes.find(types) {
            return id;
        }
        let columns: Vec<Box<M::ErasedColumn>> = types
            .iter()
            .map(|type_id| {
                let ctor = self
                    .ctors
                    .get(type_id)
                    .expect("every component type is registered before its archetype exists");
                ctor()
            })
            .collect();
        self.archetypes
            .register(Archetype::new(types.into(), columns.into_boxed_slice()))
    }

    /// The archetype of `source` plus the component `added`.
    fn archetype_with_added(&mut self, source: u32, added: TypeId) -> u32 {
        if self.archetypes.get(source).column_index(added).is_some() {
            return source;
        }
        if let Some(target) = self.archetypes.add_edge(source, added) {
            return target;
        }
        // First time this transition is taken: build the component list, which
        // is the only allocation left on a structural change.
        let mut types = self.archetypes.get(source).types().to_vec();
        let index = match types.binary_search(&added) {
            // The early return above already handled the present case.
            Ok(_) => return source,
            Err(index) => index,
        };
        types.insert(index, added);
        let target = self.archetype_for_types(&types);
        self.archetypes.set_add_edge(source, added, target);
        target
    }

    /// The archetype of `source` without the component `removed`.
    fn archetype_without(&mut self, source: u32, removed: TypeId) -> u32 {
        if self.archetypes.get(source).column_index(removed).is_none() {
            return source;
        }
        if let Some(target) = self.archetypes.remove_edge(source, removed) {
            return target;
        }
        let mut types = self.archetypes.get(source).types().to_vec();
        let index = match types.binary_search(&removed) {
            Ok(index) => index,
            // The early return above already handled the absent case.
            Err(_) => return source,
        };
        types.remove(index);
        let target = self.archetype_for_types(&types);
        self.archetypes.set_remove_edge(source, removed, target);
        target
    }

    /// Move the row at `location` into archetype `target`, optionally
    /// attaching `extra`, and return the component that did not fit.
    fn move_row(
        &mut self,
        location: Location,
        target: u32,
        extra: Option<(TypeId, Box<dyn Any>)>,
    ) -> Option<Box<dyn Any>> {
        let entity = self
            .archetypes
            .get(location.archetype)
            .entity_at(location.row());
        let (source, target_archetype) = self.archetypes.split_mut(location.archetype, target);
        let (removed, swapped) = source.move_row(location.row(), target_archetype, extra);
        let target_row = (target_archetype.len() - 1) as u32;
        if let Some(swapped) = swapped {
            self.entities_write().set_location(
                swapped,
                Location {
                    archetype: location.archetype,
                    row: location.row,
                },
            );
        }
        self.entities_write().set_location(
            entity,
            Location {
                archetype: target,
                row: target_row,
            },
        );
        removed
    }

    /// Detach the entity from its parent's child list.
    pub(crate) fn detach_from_parent(&mut self, parent: Entity, child: Entity) {
        let _ = self.with_mut::<crate::hierarchy::Children, _>(parent, |children| {
            children.remove(child);
        });
    }

    pub(crate) fn archetype_of(&self, entity: Entity) -> Option<Location> {
        self.entities_read().location(entity)
    }

    /// Drop the row at `location`, keeping every other location correct.
    pub(crate) fn remove_row(&mut self, location: Location) -> bool {
        let archetype = self.archetypes.get_mut(location.archetype);
        match archetype.swap_remove(location.row()) {
            Some(moved) => {
                self.entities_write().set_location(
                    moved,
                    Location {
                        archetype: location.archetype,
                        row: location.row,
                    },
                );
                true
            }
            None => false,
        }
    }

    pub(crate) fn free_entity(&mut self, entity: Entity) -> bool {
        self.entities_write().free(entity).is_ok()
    }

    /// Depth-first despawning of a subtree, children before the parent.
    ///
    /// The walk keeps its own stack instead of recursing, so a deep chain of
    /// entities cannot overflow the call stack.
    fn despawn_subtree(&mut self, entity: Entity) {
        let mut stack = vec![entity];
        while let Some(current) = stack.pop() {
            // Despawning a child detaches it from its parent, which would
            // invalidate a borrow of the list; move the list out instead of
            // copying it, leaving the component empty for the detach to find.
            if let Some(children) = self.with_mut::<crate::hierarchy::Children, _>(
                current,
                crate::hierarchy::Children::take,
            ) {
                stack.extend(children);
            }
            if let Some(parent) = self
                .get::<crate::hierarchy::ChildOf>(current)
                .map(|child| child.0)
            {
                self.detach_from_parent(parent, current);
            }
            if let Some(location) = self.archetype_of(current) {
                self.remove_row(location);
            }
            self.free_entity(current);
        }
    }
}

/// The borrow-conflict panic.
fn borrow_conflict<C: 'static>(kind: &str) -> ! {
    panic!(
        "component `{}` is already borrowed while it is being {}",
        core::any::type_name::<C>(),
        kind,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalWorld;
    use crate::tests_common::{Addable, Marker, Name};

    #[test]
    fn spawn_makes_an_entity_with_its_components() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1), Name("a")));
        assert!(world.contains(entity));
        assert_eq!(world.len(), 1);
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 1);
        assert_eq!(world.get::<Name>(entity).unwrap().0, "a");
    }

    #[test]
    fn spawn_empty_has_no_components() {
        let mut world = LocalWorld::new();
        let entity = world.spawn_empty();
        assert!(world.contains(entity));
        assert!(!world.has::<Marker>(entity));
        assert!(world.get::<Marker>(entity).is_none());
    }

    #[test]
    fn despawning_frees_the_handle() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        assert!(world.despawn(entity));
        assert!(!world.contains(entity));
        assert!(!world.despawn(entity), "the second despawn is a no-op");
        assert_eq!(world.len(), 0);
    }

    #[test]
    fn despawn_keeps_other_entities_addressable() {
        let mut world = LocalWorld::new();
        let first = world.spawn((Marker(1),));
        let second = world.spawn((Marker(2),));
        let third = world.spawn((Marker(3),));
        world.despawn(second);
        // The last row moved into the hole; its location must still be right.
        assert_eq!(world.get::<Marker>(first).unwrap().0, 1);
        assert_eq!(world.get::<Marker>(third).unwrap().0, 3);
        assert_eq!(world.len(), 2);
    }

    #[test]
    fn components_are_isolated_by_entity() {
        let mut world = LocalWorld::new();
        let first = world.spawn((Marker(1),));
        let second = world.spawn((Marker(2),));
        world
            .with_mut::<Marker, _>(first, |marker| marker.0 = 10)
            .unwrap();
        assert_eq!(world.get::<Marker>(first).unwrap().0, 10);
        assert_eq!(world.get::<Marker>(second).unwrap().0, 2);
    }

    #[test]
    fn with_mut_is_none_without_the_component() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        assert!(world.with_mut::<Name, _>(entity, |_| ()).is_none());
    }

    #[test]
    fn addable_components_can_be_added_and_removed_while_running() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        world.insert(entity, Addable(7)).unwrap();
        assert!(world.has::<Addable>(entity));
        assert_eq!(world.get::<Addable>(entity).unwrap().0, 7);
        assert_eq!(
            world.get::<Marker>(entity).unwrap().0,
            1,
            "other components survive"
        );

        assert_eq!(world.remove::<Addable>(entity).unwrap(), Addable(7));
        assert!(!world.has::<Addable>(entity));
        assert!(world.get::<Marker>(entity).is_some());
    }

    #[test]
    fn adding_a_component_twice_is_an_error() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Addable(1),));
        let error = world.insert(entity, Addable(2)).unwrap_err();
        assert!(matches!(error, InsertError::AlreadyPresent { .. }));
    }

    #[test]
    fn removing_a_missing_component_is_an_error() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        let error = world.remove::<Addable>(entity).unwrap_err();
        assert!(matches!(error, RemoveError::Missing { .. }));
    }

    #[test]
    fn structures_on_a_dead_entity_error() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        world.despawn(entity);
        assert!(matches!(
            world.insert(entity, Addable(1)),
            Err(InsertError::NoSuchEntity)
        ));
        assert!(matches!(
            world.remove::<Addable>(entity),
            Err(RemoveError::NoSuchEntity)
        ));
    }

    #[test]
    fn spawn_at_puts_a_bundle_on_a_reserved_entity() {
        let mut world = LocalWorld::new();
        let entity = world.reserve_entity();
        assert!(!world.contains(entity));
        world.spawn_at(entity, (Marker(5),));
        assert!(world.contains(entity));
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "is already spawned")]
    fn spawn_at_on_a_live_entity_panics() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        world.spawn_at(entity, (Marker(2),));
    }

    #[test]
    fn releasing_a_reserved_handle_returns_its_index_to_the_pool() {
        let mut world = LocalWorld::new();
        let reserved = world.reserve_entity();
        assert!(!world.contains(reserved));
        assert_eq!(world.len(), 0);
        assert!(world.release_entity(reserved));
        assert!(!world.release_entity(reserved), "already released");
        let fresh = world.spawn((Marker(1),));
        assert_eq!(fresh.index(), reserved.index(), "the index was reused");
        assert_ne!(fresh.generation(), reserved.generation());
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn adding_and_removing_a_component_reuses_one_archetype() {
        let mut world = LocalWorld::new();
        let entities: Vec<_> = (0..4u32)
            .map(|value| world.spawn((Marker(value),)))
            .collect();
        for entity in &entities {
            world.insert(*entity, Addable(1)).unwrap();
        }
        let with_component = world.archetype_count();
        for entity in &entities {
            world.remove::<Addable>(*entity).unwrap();
        }
        for entity in &entities {
            world.insert(*entity, Addable(2)).unwrap();
        }
        for entity in &entities {
            world.remove::<Addable>(*entity).unwrap();
        }
        assert_eq!(
            world.archetype_count(),
            with_component,
            "every round trip reuses the two archetypes"
        );
    }

    #[test]
    fn archetypes_are_reused_for_the_same_component_set() {
        let mut world = LocalWorld::new();
        world.spawn((Marker(1), Name("a")));
        let before = world.archetype_count();
        world.spawn((Marker(2), Name("b")));
        assert_eq!(world.archetype_count(), before, "same set, same archetype");
        world.spawn((Marker(3),));
        assert_eq!(world.archetype_count(), before + 1);
    }
}
#[cfg(test)]
mod send_tests {
    use crate::SendWorld;
    use crate::tests_common::Marker;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn the_send_world_is_send_and_sync() {
        assert_send_sync::<SendWorld>();
    }

    #[test]
    fn several_threads_may_write_the_same_world() {
        let mut world = SendWorld::new();
        let handles: Vec<_> = (0..4u32)
            .map(|value| world.spawn((Marker(value), 0u32)))
            .collect();
        let world_ref = &world;
        let handles_ref = &handles;
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(move || {
                    for handle in handles_ref {
                        world_ref
                            .with_mut::<u32, _>(*handle, |count| *count += 1)
                            .unwrap();
                    }
                });
            }
        });
        for handle in &handles {
            assert_eq!(*world.get::<u32>(*handle).unwrap(), 2);
        }
    }

    #[test]
    fn the_local_world_stays_on_one_thread() {
        use crate::LocalWorld;
        // LocalWorld is not Send; on its own thread it behaves as usual.
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        world
            .with_mut::<Marker, _>(entity, |marker| marker.0 = 2)
            .unwrap();
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 2);
    }
}
