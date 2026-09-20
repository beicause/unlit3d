//! Queries: picking entities by the components they have.
//!
//! A query is a type describing what to fetch from each matching entity: a
//! shared reference `&T`, an exclusive reference `&mut T`, an optional
//! reference `Option<&T>`, the [`Entity`] itself, or a tuple of those. A
//! component can be required without being fetched with [`With`], and
//! required to be absent with [`Without`].
//!
//! An entity matches when it has every component the query names. Iteration
//! visits archetypes in creation order and rows in storage order, so it is
//! deterministic.
//!
//! `````
//! # use unlit_ecs::{LocalWorld, Query};
//! let mut world = LocalWorld::new();
//! world.spawn((1u32, 10.0f32));
//! world.spawn((2u32, true));
//! let sum: u32 = world.query::<&u32>().map(|(_, value)| *value).sum();
//! assert_eq!(sum, 3);
//! `````

use core::any::TypeId;
use core::marker::PhantomData;

use crate::archetype::Archetype;
use crate::entity::Entity;
use crate::mode::{Cell, CellRef, CellRefMut, Mode};
use crate::world::World;

/// Panics when a matched archetype unexpectedly lacks a component. Only a
/// malformed `matches` implementation can reach this.
fn missing<C: 'static>() -> ! {
    panic!(
        "component @@{}@@ is missing from an archetype the query matched",
        core::any::type_name::<C>(),
    )
}

/// Panics when a component is already borrowed in a conflicting way.
fn borrowed<C: 'static>(kind: &str) -> ! {
    panic!(
        "component @@{}@@ is already borrowed while it is being {}",
        core::any::type_name::<C>(),
        kind,
    )
}

/// What to fetch from every entity a query matches.
pub trait Query {
    /// What one match yields.
    type Item<'a, M: Mode>;

    /// Whether an archetype has the components the query needs.
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool;

    /// Fetch the item of row `row`.
    ///
    /// Panics when the component is already borrowed in a conflicting way, or
    /// when a query asks for the same component as `&mut` twice without
    /// dropping the first item.
    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M>;
}

impl<T: 'static> Query for &T {
    type Item<'a, M: Mode> = CellRef<'a, M, T>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_some()
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        archetype
            .cell::<T>(row)
            .unwrap_or_else(|| missing::<T>())
            .try_read()
            .unwrap_or_else(|| borrowed::<T>("read"))
    }
}

impl<T: 'static> Query for &mut T {
    type Item<'a, M: Mode> = CellRefMut<'a, M, T>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_some()
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        archetype
            .cell::<T>(row)
            .unwrap_or_else(|| missing::<T>())
            .try_write()
            .unwrap_or_else(|| borrowed::<T>("write"))
    }
}

impl<T: 'static> Query for Option<&T> {
    type Item<'a, M: Mode> = Option<CellRef<'a, M, T>>;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        archetype.cell::<T>(row).and_then(|cell| cell.try_read())
    }
}

impl<T: 'static> Query for Option<&mut T> {
    type Item<'a, M: Mode> = Option<CellRefMut<'a, M, T>>;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        archetype.cell::<T>(row).and_then(|cell| cell.try_write())
    }
}

impl Query for Entity {
    type Item<'a, M: Mode> = Entity;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        archetype.entities()[row]
    }
}

/// Requires the components of `R` without fetching them.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Query, With};
/// let mut world = LocalWorld::new();
/// let visible = world.spawn((1u32, true));
/// world.spawn((2u32,));
/// let found: Vec<_> = world
///     .query::<With<&u32, &bool>>()
///     .map(|(entity, _)| entity)
///     .collect();
/// assert_eq!(found, [visible]);
/// `````
pub struct With<Q, R>(PhantomData<fn() -> (Q, R)>);

impl<Q: Query, R: Query> Query for With<Q, R> {
    type Item<'a, M: Mode> = Q::Item<'a, M>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        Q::matches(archetype) && R::matches(archetype)
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        Q::fetch(archetype, row)
    }
}

/// Forbids the components of `R`.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Query, Without};
/// let mut world = LocalWorld::new();
/// let hidden = world.spawn((1u32,));
/// world.spawn((2u32, true));
/// let found: Vec<_> = world
///     .query::<Without<&u32, &bool>>()
///     .map(|(entity, _)| entity)
///     .collect();
/// assert_eq!(found, [hidden]);
/// `````
pub struct Without<Q, R>(PhantomData<fn() -> (Q, R)>);

impl<Q: Query, R: Query> Query for Without<Q, R> {
    type Item<'a, M: Mode> = Q::Item<'a, M>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        Q::matches(archetype) && !R::matches(archetype)
    }

    fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
        Q::fetch(archetype, row)
    }
}

macro_rules! impl_query_tuple {
    ($($name:ident),*) => {
        impl<$($name: Query),*> Query for ($($name,)*) {
            type Item<'a, M: Mode> = ($($name::Item<'a, M>,)*);

            fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
                $($name::matches(archetype))&&*
            }

            fn fetch<M: Mode>(archetype: &Archetype<M>, row: usize) -> Self::Item<'_, M> {
                ($($name::fetch(archetype, row),)*)
            }
        }
    };
}

impl_query_tuple!(A);
impl_query_tuple!(A, B);
impl_query_tuple!(A, B, C);
impl_query_tuple!(A, B, C, D);
impl_query_tuple!(A, B, C, D, E);
impl_query_tuple!(A, B, C, D, E, F);
impl_query_tuple!(A, B, C, D, E, F, G);
impl_query_tuple!(A, B, C, D, E, F, G, H);

/// Iterates the entities matching `Q`.
///
/// An item borrows the world, so two items from one iterator may be alive at the
/// same time. Asking for `&mut` components twice without dropping the first
/// item panics; [`World::for_each`] is the form that guarantees each item is
/// dropped before the next is fetched.
pub struct QueryIter<'w, M: Mode, Q: Query> {
    world: &'w World<M>,
    archetype: u32,
    row: usize,
    _query: PhantomData<fn() -> Q>,
}

impl<'w, M: Mode, Q: Query> QueryIter<'w, M, Q> {
    pub(crate) fn new(world: &'w World<M>) -> Self {
        Self {
            world,
            archetype: 0,
            row: 0,
            _query: PhantomData,
        }
    }
}

impl<'w, M: Mode, Q: Query> Iterator for QueryIter<'w, M, Q> {
    type Item = (Entity, Q::Item<'w, M>);

    fn next(&mut self) -> Option<Self::Item> {
        while (self.archetype as usize) < self.world.archetype_count() {
            let archetype = self.world.archetypes().nth(self.archetype as usize)?;
            if Q::matches(archetype) && self.row < archetype.len() {
                let row = self.row;
                self.row += 1;
                let entity = archetype.entities()[row];
                return Some((entity, Q::fetch(archetype, row)));
            }
            self.archetype += 1;
            self.row = 0;
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_common::Marker;

    #[test]
    fn a_single_component_query_visits_every_holder() {
        let mut world = crate::LocalWorld::new();
        world.spawn((Marker(1),));
        world.spawn((Marker(2), true));
        let mut values: Vec<u32> = world.query::<&Marker>().map(|(_, m)| m.0).collect();
        values.sort_unstable();
        assert_eq!(values, [1, 2]);
    }

    #[test]
    fn a_tuple_query_requires_every_component() {
        let mut world = crate::LocalWorld::new();
        let both = world.spawn((Marker(1), 9u32));
        world.spawn((Marker(2),));
        let found: Vec<Entity> = world.query::<(&Marker, &u32)>().map(|(e, _)| e).collect();
        assert_eq!(found, [both]);
    }

    #[test]
    fn mutable_components_are_written_through_the_query() {
        let mut world = crate::LocalWorld::new();
        world.spawn((Marker(1),));
        world.spawn((Marker(2),));
        world.for_each::<(&mut Marker,), _>(|(mut marker,)| marker.0 *= 10);
        let mut values: Vec<u32> = world.query::<&Marker>().map(|(_, m)| m.0).collect();
        values.sort_unstable();
        assert_eq!(values, [10, 20]);
    }

    #[test]
    fn optional_components_are_none_when_missing() {
        let mut world = crate::LocalWorld::new();
        let with = world.spawn((Marker(1), 5u32));
        let without = world.spawn((Marker(2),));
        let mut found: Vec<(Entity, Option<u32>)> = world
            .query::<(&Marker, Option<&u32>)>()
            .map(|(e, (_, value))| (e, value.map(|v| *v)))
            .collect();
        found.sort_by_key(|(_, value)| *value);
        assert_eq!(found, [(without, None), (with, Some(5))]);
    }

    #[test]
    fn with_and_without_filter_by_presence() {
        let mut world = crate::LocalWorld::new();
        let plain = world.spawn((Marker(1),));
        let flagged = world.spawn((Marker(2), true));
        let with: Vec<Entity> = world
            .query::<With<&Marker, &bool>>()
            .map(|(e, _)| e)
            .collect();
        let without: Vec<Entity> = world
            .query::<Without<&Marker, &bool>>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(with, [flagged]);
        assert_eq!(without, [plain]);
    }

    #[test]
    fn the_entity_query_yields_handles() {
        let mut world = crate::LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        assert_eq!(world.query::<Entity>().next(), Some((entity, entity)));
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn asking_for_the_same_component_twice_panics() {
        let mut world = crate::LocalWorld::new();
        world.spawn((Marker(1),));
        // Both items stay alive, so the second exclusive borrow must fail.
        let items: Vec<_> = world.query::<(&mut Marker, &mut Marker)>().collect();
        drop(items);
    }

    #[test]
    fn iterating_archetypes_directly_is_possible() {
        let mut world = crate::LocalWorld::new();
        world.spawn((Marker(1),));
        world.spawn((Marker(2), true));
        let total: usize = world.archetypes().map(|archetype| archetype.len()).sum();
        assert_eq!(total, 2);
    }
}
