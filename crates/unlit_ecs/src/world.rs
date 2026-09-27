//! The world: entities, their components, and the queries over them.
//!
//! Components live in `RefCell`s, so a component can be read or written
//! through a shared `&World`. The world is therefore `!Send`: it stays on the
//! thread that created it, which is where a renderer or any other `!Send`
//! state lives.
//!
//! Reading and writing components needs only `&World`, because every value
//! lives in a cell. Structural changes — spawning and despawning — need
//! `&mut World`. A callback that only has a shared world can queue those
//! changes instead with [`World::queue`] and apply them later with
//! [`World::apply`].
//!
//! An entity's components are fixed when it is spawned; there is no way to add
//! or remove one later. To change the set of components, despawn the entity and
//! spawn a new one.
//!
//! A borrow conflict is a panic, not a compile error: asking for a component
//! that is already borrowed, or fetching the same component as `&mut` twice
//! in one query, reports the component's name and stops. This is the trade for
//! not doing access analysis.

use core::any::TypeId;
use core::cell::{Ref, RefCell, RefMut};

use crate::archetype::{Archetype, Archetypes};
use crate::bundle::Bundle;
use crate::column::AnyColumn;
use crate::column::{CellRef, CellRefMut};
use crate::command::{Command, Commands};
use crate::entity::{Entities, Entity, Location};
use crate::hash::TypeIdHashMap;
use crate::query::{Query, QueryFilter, QueryIter};

/// Entities and their components.
pub struct World {
    entities: RefCell<Entities>,
    archetypes: Archetypes,
    /// Column constructors, one per component type that ever entered the world.
    ctors: TypeIdHashMap<fn() -> Box<dyn AnyColumn>>,
    commands: RefCell<Vec<Box<dyn Command>>>,
}

/// The exclusive borrow of a world's command queue.
pub(crate) type CommandsGuard<'w> = RefMut<'w, Vec<Box<dyn Command>>>;

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// An empty world holding only the empty archetype.
    pub fn new() -> Self {
        Self {
            entities: RefCell::new(Entities::default()),
            archetypes: Archetypes::new(),
            ctors: TypeIdHashMap::default(),
            commands: RefCell::new(Vec::new()),
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
    #[inline]
    pub fn get<C: 'static>(&self, entity: Entity) -> Option<CellRef<'_, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype)
            .cell::<C>(location.row())?;
        Some(
            cell.try_borrow()
                .unwrap_or_else(|_| borrow_conflict::<C>("read")),
        )
    }

    /// The component of `entity`, exclusively.
    ///
    /// Panics when the component is already borrowed, shared or exclusive.
    #[must_use]
    #[inline]
    pub fn get_mut<C: 'static>(&self, entity: Entity) -> Option<CellRefMut<'_, C>> {
        let location = self.entities_read().location(entity)?;
        let cell = self
            .archetypes
            .get(location.archetype)
            .cell::<C>(location.row())?;
        Some(
            cell.try_borrow_mut()
                .unwrap_or_else(|_| borrow_conflict::<C>("write")),
        )
    }

    /// Run `f` on the component of `entity`.
    ///
    /// This is the scoped form of [`World::get_mut`]: the borrow ends with the
    /// closure, so nothing else can trip over it. It is also how a behaviour
    /// component is driven: the closure is the behaviour, and the caller
    /// decides which entities to run it on.
    ///
    /// `````
    /// # use unlit_ecs::World;
    /// let mut world = World::new();
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
    /// # use unlit_ecs::World;
    /// let mut world = World::new();
    /// let entity = world.spawn((1u32, true));
    /// assert!(world.has::<u32>(entity));
    /// `````
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
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
    pub fn spawn_at<B: Bundle>(&mut self, entity: Entity, bundle: B) {
        assert!(
            !self.entities_read().contains(entity),
            "{entity:?} is already spawned"
        );
        let (types, values, ctors) = bundle.into_builder().finish();
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
        self.entities_write().free(entity)
    }

    /// Despawn `entity`.
    ///
    /// Returns `false` when the entity was not alive. Only the named entity is
    /// removed; handles to it that the caller kept elsewhere resolve to nothing
    /// afterwards, and it is the caller's job to notice.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        let Some(location) = self.location(entity) else {
            return false;
        };
        self.remove_row(location);
        self.free_entity(entity)
    }

    // -- queries -----------------------------------------------------------

    /// Iterate the entities matching `Q`.
    ///
    /// `````
    /// # use unlit_ecs::{Query, World};
    /// let mut world = World::new();
    /// world.spawn((1u32,));
    /// world.spawn((2u32, true));
    /// let values: Vec<u32> = world
    ///     .query::<(&u32, &bool)>()
    ///     .map(|(_, (value, _))| *value)
    ///     .collect();
    /// assert_eq!(values, [2]);
    /// `````
    ///
    /// This is [`World::query_filtered`] with an empty filter; use that one to
    /// narrow the entities visited without fetching anything more.
    pub fn query<Q: Query>(&self) -> QueryIter<'_, Q> {
        QueryIter::new(self)
    }

    /// Iterate the entities matching `Q` that pass the filter `F`.
    ///
    /// The filter is a [`QueryFilter`]: it narrows the archetypes visited and
    /// fetches nothing, so [`With`](crate::With) and [`Without`](crate::Without) cost no borrow and nothing
    /// appears in the item.
    ///
    /// `````
    /// # use unlit_ecs::{Query, Without, World};
    /// let mut world = World::new();
    /// let plain = world.spawn((1u32,));
    /// world.spawn((2u32, true));
    /// let values: Vec<u32> = world
    ///     .query_filtered::<&u32, Without<bool>>()
    ///     .map(|(_, value)| *value)
    ///     .collect();
    /// assert_eq!(values, [1]);
    /// `````
    pub fn query_filtered<Q: Query, F: QueryFilter>(&self) -> QueryIter<'_, Q, F> {
        QueryIter::new(self)
    }

    /// Run `f` for every entity matching `Q`.
    ///
    /// Each item is fetched and dropped before the next one, so a query may ask
    /// for `&mut` components. Use [`World::query`] when two items must be
    /// alive at the same time.
    ///
    /// This is [`World::for_each_filtered`] with an empty filter.
    pub fn for_each<Q: Query, F>(&self, f: F)
    where
        F: FnMut(Q::Item<'_>),
    {
        self.for_each_filtered::<Q, (), F>(f);
    }

    /// Run `f` for every entity matching `Q` that passes the filter `T`.
    ///
    /// The filter narrows the archetypes visited and fetches nothing, so it
    /// costs no borrow and nothing appears in the item.
    pub fn for_each_filtered<Q: Query, T: QueryFilter, F>(&self, mut f: F)
    where
        F: FnMut(Q::Item<'_>),
    {
        for archetype in self.archetypes.iter() {
            if !Q::matches(archetype) || !T::matches(archetype) {
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
    pub fn archetypes(&self) -> impl Iterator<Item = &Archetype> {
        self.archetypes.iter()
    }

    /// The archetypes, including the empty one, as a slice.
    pub(crate) fn archetypes_slice(&self) -> &[Archetype] {
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
    pub fn queue(&self) -> Commands<'_> {
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

    // -- internals ---------------------------------------------------------

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

    pub(crate) fn commands_write(&self) -> CommandsGuard<'_> {
        self.commands
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the command queue is already borrowed"))
    }

    /// Append a command to the deferred queue.
    pub(crate) fn push_command(&self, command: Box<dyn Command>) {
        self.commands_write().push(command);
    }

    /// The archetype with exactly `types` (sorted), creating it when it is
    /// new. Every type must have a registered column constructor.
    fn archetype_for_types(&mut self, types: &[TypeId]) -> u32 {
        if let Some(id) = self.archetypes.find(types) {
            return id;
        }
        let columns: Vec<Box<dyn AnyColumn>> = types
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

    /// The archetype with this id, or `None` when there is no such archetype.
    ///
    /// An id comes from [`Location::archetype`]. Several entities can share
    /// one, which is what lets a caller holding many entities resolve an
    /// archetype's columns once for all of them instead of once per entity.
    #[inline]
    pub fn archetype(&self, id: u32) -> Option<&Archetype> {
        self.archetypes.as_slice().get(id as usize)
    }

    /// Where a live entity's components live, or `None` when it is not alive.
    ///
    /// The location is what [`World::archetype`] and
    /// [`Archetype::entities`] together turn into a component.
    #[inline]
    pub fn location(&self, entity: Entity) -> Option<Location> {
        self.entities_read().location(entity)
    }

    /// Drop the row at `location`, keeping every other location correct.
    pub(crate) fn remove_row(&mut self, location: Location) {
        let archetype = self.archetypes.get_mut(location.archetype);
        if let Some(moved) = archetype.swap_remove(location.row()) {
            self.entities_write().set_location(
                moved,
                Location {
                    archetype: location.archetype,
                    row: location.row,
                },
            );
        }
    }

    pub(crate) fn free_entity(&mut self, entity: Entity) -> bool {
        self.entities_write().free(entity)
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
    use crate::World;
    use crate::tests_common::{Marker, Name};

    #[test]
    fn spawn_makes_an_entity_with_its_components() {
        let mut world = World::new();
        let entity = world.spawn((Marker(1), Name("a")));
        assert!(world.contains(entity));
        assert_eq!(world.len(), 1);
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 1);
        assert_eq!(world.get::<Name>(entity).unwrap().0, "a");
    }

    #[test]
    fn spawn_empty_has_no_components() {
        let mut world = World::new();
        let entity = world.spawn_empty();
        assert!(world.contains(entity));
        assert!(!world.has::<Marker>(entity));
        assert!(world.get::<Marker>(entity).is_none());
    }

    #[test]
    fn despawning_frees_the_handle() {
        let mut world = World::new();
        let entity = world.spawn((Marker(1),));
        assert!(world.despawn(entity));
        assert!(!world.contains(entity));
        assert!(!world.despawn(entity), "the second despawn is a no-op");
        assert_eq!(world.len(), 0);
    }

    #[test]
    fn despawn_keeps_other_entities_addressable() {
        let mut world = World::new();
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
        let mut world = World::new();
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
        let mut world = World::new();
        let entity = world.spawn((Marker(1),));
        assert!(world.with_mut::<Name, _>(entity, |_| ()).is_none());
    }

    #[test]
    fn spawn_at_puts_a_bundle_on_a_reserved_entity() {
        let mut world = World::new();
        let entity = world.reserve_entity();
        assert!(!world.contains(entity));
        world.spawn_at(entity, (Marker(5),));
        assert!(world.contains(entity));
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "is already spawned")]
    fn spawn_at_on_a_live_entity_panics() {
        let mut world = World::new();
        let entity = world.spawn((Marker(1),));
        world.spawn_at(entity, (Marker(2),));
    }

    #[test]
    fn releasing_a_reserved_handle_returns_its_index_to_the_pool() {
        let mut world = World::new();
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
    fn archetypes_are_reused_for_the_same_component_set() {
        let mut world = World::new();
        world.spawn((Marker(1), Name("a")));
        let before = world.archetype_count();
        world.spawn((Marker(2), Name("b")));
        assert_eq!(world.archetype_count(), before, "same set, same archetype");
        world.spawn((Marker(3),));
        assert_eq!(world.archetype_count(), before + 1);
    }
}
