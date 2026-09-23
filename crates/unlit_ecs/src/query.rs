//! Queries: picking entities by the components they have.
//!
//! A query has two halves, given as two type parameters: *what to fetch* and
//! *which entities to fetch it from*.
//!
//! The data half is a [`Query`]: a shared reference `&T`, an exclusive
//! reference `&mut T`, an optional reference `Option<&T>`, the [`Entity`]
//! itself, or a tuple of those.
//!
//! The filter half is a [`QueryFilter`]: it narrows the archetypes a query
//! visits without fetching anything. [`With<T>`] requires `T`, [`Without<T>`]
//! forbids it, [`Or`] takes the union of several, and a tuple of them requires
//! all. `()` matches everything, which is what plain `query::<D>()` uses.
//!
//! An entity matches when the data finds what it names and the filter accepts
//! the archetype. Iteration visits archetypes in creation order and rows in
//! storage order, so it is deterministic.
//!
//! `````
//! # use unlit_ecs::{LocalWorld, Query, Without};
//! let mut world = LocalWorld::new();
//! world.spawn((1u32, 10.0f32));
//! world.spawn((2u32, true));
//! let sum: u32 = world.query::<&u32>().map(|(_, value)| *value).sum();
//! assert_eq!(sum, 3);
//! // Only the entity without a `bool`.
//! let sum: u32 = world
//!     .query_filtered::<&u32, Without<bool>>()
//!     .map(|(_, value)| *value)
//!     .sum();
//! assert_eq!(sum, 1);
//! `````

use core::any::TypeId;
use core::marker::PhantomData;

use crate::archetype::Archetype;
use crate::entity::Entity;
use crate::mode::{Cell, CellRef, CellRefMut, Column, Mode};
use crate::world::World;

/// Panics when a matched archetype unexpectedly lacks a component. Only a
/// malformed `matches` implementation can reach this.
fn missing<C: 'static>() -> ! {
    panic!(
        "component `{}` is missing from an archetype the query matched",
        core::any::type_name::<C>(),
    )
}

/// Panics when a component is already borrowed in a conflicting way.
fn borrowed<C: 'static>(kind: &str) -> ! {
    panic!(
        "component `{}` is already borrowed while it is being {}",
        core::any::type_name::<C>(),
        kind,
    )
}

/// What to fetch from every entity a query matches.
///
/// A query resolves the columns it needs once per archetype — that is
/// [`Query::fetch_state`] — and then fetches rows through that state without
/// looking a component type up again.
pub trait Query {
    /// What one match yields.
    type Item<'a, M: Mode>;

    /// The per-archetype state the query resolves once and reuses for every
    /// row of that archetype.
    type Fetch<'a, M: Mode>;

    /// Whether an archetype has the components the query needs.
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool;

    /// Resolve [`Query::Fetch`] for an archetype [`Query::matches`] accepted.
    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M>;

    /// Fetch the item of row `row`.
    ///
    /// Panics when the component is already borrowed in a conflicting way, or
    /// when a query asks for the same component as `&mut` twice without
    /// dropping the first item.
    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M>;
}

impl<T: 'static> Query for &T {
    type Item<'a, M: Mode> = CellRef<'a, M, T>;
    type Fetch<'a, M: Mode> = &'a Column<M, T>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_some()
    }

    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
        state
            .cell(row)
            .unwrap_or_else(|| missing::<T>())
            .try_read()
            .unwrap_or_else(|| borrowed::<T>("read"))
    }
}

impl<T: 'static> Query for &mut T {
    type Item<'a, M: Mode> = CellRefMut<'a, M, T>;
    type Fetch<'a, M: Mode> = &'a Column<M, T>;

    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_some()
    }

    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
        state
            .cell(row)
            .unwrap_or_else(|| missing::<T>())
            .try_write()
            .unwrap_or_else(|| borrowed::<T>("write"))
    }
}

impl<T: 'static> Query for Option<&T> {
    type Item<'a, M: Mode> = Option<CellRef<'a, M, T>>;
    type Fetch<'a, M: Mode> = Option<&'a Column<M, T>>;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
        archetype.column::<T>()
    }

    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
        let column: Option<&'a Column<M, T>> = *state;
        column
            .and_then(|column| column.cell(row))
            .and_then(Cell::try_read)
    }
}

impl<T: 'static> Query for Option<&mut T> {
    type Item<'a, M: Mode> = Option<CellRefMut<'a, M, T>>;
    type Fetch<'a, M: Mode> = Option<&'a Column<M, T>>;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
        archetype.column::<T>()
    }

    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
        let column: Option<&'a Column<M, T>> = *state;
        column
            .and_then(|column| column.cell(row))
            .and_then(Cell::try_write)
    }
}

impl Query for Entity {
    type Item<'a, M: Mode> = Entity;
    type Fetch<'a, M: Mode> = &'a Archetype<M>;

    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }

    fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
        archetype
    }

    fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
        state.entities()[row]
    }
}

/// Narrows the entities a query visits without fetching anything from them.
///
/// A filter is answered from the archetype alone: an archetype either has a
/// component or it does not, so a filter needs no per-row work and no borrow
/// of the world's cells. `Query::matches` answers the data half of a query and
/// this answers the rest.
///
/// Implemented for [`With`], [`Without`], [`Or`], and tuples of them up to
/// arity eight. `()` accepts every archetype.
///
/// The trait is sealed: the set of filters is fixed, because a filter is only
/// sound while it stays a pure function of the archetype's component set.
pub trait QueryFilter: sealed::Sealed {
    /// Whether entities of `archetype` pass this filter.
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool;
}

mod sealed {
    pub trait Sealed {}
}

/// Requires the component `T`, without fetching it.
///
/// `````
/// # use unlit_ecs::{LocalWorld, With};
/// let mut world = LocalWorld::new();
/// let flagged = world.spawn((1u32, true));
/// world.spawn((2u32,));
/// let found: Vec<_> = world.query_filtered::<&u32, With<bool>>().collect();
/// assert_eq!(found.len(), 1);
/// `````
pub struct With<T>(PhantomData<fn() -> T>);

impl<T: 'static> sealed::Sealed for With<T> {}

impl<T: 'static> QueryFilter for With<T> {
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_some()
    }
}

/// Forbids the component `T`.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Without};
/// let mut world = LocalWorld::new();
/// let plain = world.spawn((1u32,));
/// world.spawn((2u32, true));
/// let found: Vec<_> = world.query_filtered::<&u32, Without<bool>>().collect();
/// assert_eq!(found.len(), 1);
/// `````
pub struct Without<T>(PhantomData<fn() -> T>);

impl<T: 'static> sealed::Sealed for Without<T> {}

impl<T: 'static> QueryFilter for Without<T> {
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        archetype.column_index(TypeId::of::<T>()).is_none()
    }
}

/// Accepts an entity that passes at least one of `T`'s filters.
///
/// The union of the filters, as opposed to a tuple's intersection.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Or, With};
/// let mut world = LocalWorld::new();
/// world.spawn((1u32, true));
/// world.spawn((2u32, 1i64));
/// world.spawn((3u32,));
/// let found: Vec<_> = world
///     .query_filtered::<&u32, Or<(With<bool>, With<i64>)>>()
///     .collect();
/// assert_eq!(found.len(), 2);
/// `````
pub struct Or<T>(PhantomData<fn() -> T>);

impl<T> sealed::Sealed for Or<T> {}

/// Whether at least one of `T`'s filters accepts the archetype.
///
/// The disjunction a tuple of filters cannot express, since a tuple is the
/// conjunction. It is what [`Or`] accepts.
pub trait AnyMatch: sealed::Sealed {
    /// Whether any filter in `T` accepts `archetype`.
    fn any_matches<M: Mode>(archetype: &Archetype<M>) -> bool;
}

/// The empty disjunction: no filter to accept an archetype, so nothing passes.
impl AnyMatch for () {
    fn any_matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        false
    }
}

impl<T: AnyMatch> QueryFilter for Or<T> {
    fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        T::any_matches(archetype)
    }
}

/// A single filter: its own disjunction.
impl<F: QueryFilter> AnyMatch for Or<F> {
    fn any_matches<M: Mode>(archetype: &Archetype<M>) -> bool {
        F::matches(archetype)
    }
}

/// Every archetype passes: the filter a query with no filtering uses.
impl QueryFilter for () {
    fn matches<M: Mode>(_archetype: &Archetype<M>) -> bool {
        true
    }
}

impl sealed::Sealed for () {}

macro_rules! impl_filter_tuple {
    ($($name:ident),+) => {
        impl<$($name: QueryFilter),+> sealed::Sealed for ($($name,)+) {}

        impl<$($name: QueryFilter),+> QueryFilter for ($($name,)+) {
            fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
                $($name::matches(archetype))&&+
            }
        }

        impl<$($name: QueryFilter),+> AnyMatch for ($($name,)+) {
            fn any_matches<M: Mode>(archetype: &Archetype<M>) -> bool {
                $($name::matches(archetype))||+
            }
        }
    };
}

impl_filter_tuple!(A);
impl_filter_tuple!(A, B);
impl_filter_tuple!(A, B, C);
impl_filter_tuple!(A, B, C, D);
impl_filter_tuple!(A, B, C, D, E);
impl_filter_tuple!(A, B, C, D, E, F);
impl_filter_tuple!(A, B, C, D, E, F, G);
impl_filter_tuple!(A, B, C, D, E, F, G, H);

macro_rules! impl_query_tuple {
    ($($name:ident),*) => {
        impl<$($name: Query),*> Query for ($($name,)*) {
            type Item<'a, M: Mode> = ($($name::Item<'a, M>,)*);
            type Fetch<'a, M: Mode> = ($($name::Fetch<'a, M>,)*);

            fn matches<M: Mode>(archetype: &Archetype<M>) -> bool {
                $($name::matches(archetype))&&*
            }

            fn fetch_state<'a, M: Mode>(archetype: &'a Archetype<M>) -> Self::Fetch<'a, M> {
                ($($name::fetch_state(archetype),)*)
            }

            #[expect(non_snake_case, reason = "the macro names bindings after the type parameters")]
            fn fetch<'a, M: Mode>(state: &Self::Fetch<'a, M>, row: usize) -> Self::Item<'a, M> {
                let ($($name,)*) = state;
                ($($name::fetch($name, row),)*)
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
pub struct QueryIter<'w, M: Mode, Q: Query, F: QueryFilter = ()> {
    world: &'w World<M>,
    archetype: usize,
    row: usize,
    /// The columns of the archetype currently being visited, resolved once in
    /// [`Query::fetch_state`].
    state: Option<Q::Fetch<'w, M>>,
    _query: PhantomData<fn() -> (Q, F)>,
}

impl<'w, M: Mode, Q: Query, F: QueryFilter> QueryIter<'w, M, Q, F> {
    pub(crate) fn new(world: &'w World<M>) -> Self {
        Self {
            world,
            archetype: 0,
            row: 0,
            state: None,
            _query: PhantomData,
        }
    }

    /// Whether the archetype at `self.archetype` is one this query visits.
    fn archetype_matches(&self) -> bool {
        let Some(archetype) = self.world.archetypes_slice().get(self.archetype) else {
            return false;
        };
        Q::matches(archetype) && F::matches(archetype)
    }
}

impl<'w, M: Mode, Q: Query, F: QueryFilter> Iterator for QueryIter<'w, M, Q, F> {
    type Item = (Entity, Q::Item<'w, M>);

    fn next(&mut self) -> Option<Self::Item> {
        let archetypes = self.world.archetypes_slice();
        loop {
            let archetype = archetypes.get(self.archetype)?;
            if self.row >= archetype.len() {
                // Done with this archetype; move to the next and resolve its
                // columns lazily.
                self.archetype += 1;
                self.row = 0;
                self.state = None;
                continue;
            }
            if self.state.is_none() {
                if !self.archetype_matches() {
                    self.archetype += 1;
                    self.row = 0;
                    continue;
                }
                self.state = Some(Q::fetch_state(archetype));
            }
            let row = self.row;
            self.row += 1;
            let entity = archetype.entities()[row];
            let state = self.state.as_ref().expect("the state was just resolved");
            return Some((entity, Q::fetch(state, row)));
        }
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

    /// An optional component is a query on its own: it matches every
    /// archetype, so archetypes that lack it still yield a `None` item.
    #[test]
    fn an_optional_component_alone_matches_every_archetype() {
        let mut world = crate::LocalWorld::new();
        let with = world.spawn((7u32,));
        let without = world.spawn((true,));
        let mut found: Vec<(Entity, Option<u32>)> = world
            .query::<Option<&u32>>()
            .map(|(e, value)| (e, value.map(|v| *v)))
            .collect();
        found.sort_by_key(|(_, value)| *value);
        assert_eq!(found, [(without, None), (with, Some(7))]);
    }

    /// An optional exclusive reference writes where the component is present
    /// and leaves archetypes without it alone.
    #[test]
    fn an_optional_mutable_component_is_written_where_present() {
        let mut world = crate::LocalWorld::new();
        let with = world.spawn((Marker(1), 5u32));
        let without = world.spawn((Marker(2),));
        world.for_each::<(Option<&mut u32>,), _>(|(value,)| {
            if let Some(mut value) = value {
                *value += 1;
            }
        });
        assert_eq!(world.get::<u32>(with).map(|value| *value), Some(6));
        assert!(world.get::<u32>(without).is_none());
    }

    #[test]
    fn with_and_without_filter_by_presence() {
        let mut world = crate::LocalWorld::new();
        let plain = world.spawn((Marker(1),));
        let flagged = world.spawn((Marker(2), true));
        let with: Vec<Entity> = world
            .query_filtered::<&Marker, With<bool>>()
            .map(|(e, _)| e)
            .collect();
        let without: Vec<Entity> = world
            .query_filtered::<&Marker, Without<bool>>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(with, [flagged]);
        assert_eq!(without, [plain]);
    }

    /// A tuple of filters is the conjunction, [`Or`] the disjunction: the two
    /// compose into "has a `bool` and one of `u8` or `i64`".
    #[test]
    fn filters_compose_by_conjunction_and_disjunction() {
        let mut world = crate::LocalWorld::new();
        let both = world.spawn((Marker(1), true, 7u8));
        world.spawn((Marker(2), true, 7i64));
        world.spawn((Marker(3), true));
        world.spawn((Marker(4), 7u8));

        let conjunctive: Vec<Entity> = world
            .query_filtered::<&Marker, (With<bool>, With<u8>)>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(conjunctive, [both]);

        let disjunctive: Vec<Entity> = world
            .query_filtered::<&Marker, (With<bool>, Or<(With<u8>, With<i64>)>)>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(
            disjunctive.len(),
            2,
            "the two entities with bool and u8/i64"
        );

        let nested: Vec<Entity> = world
            .query_filtered::<&Marker, Or<(With<u8>, With<i64>)>>()
            .map(|(e, _)| e)
            .collect();
        assert_eq!(nested.len(), 3, "every entity with a u8 or an i64");
    }

    /// A filter is answered from the archetype alone: it fetches nothing, so
    /// naming components in it costs no borrow of them.
    #[test]
    fn a_filter_borrows_nothing() {
        let mut world = crate::LocalWorld::new();
        world.spawn((Marker(1), true));

        // Hold an exclusive borrow of `Marker`, then query `bool` with a
        // filter that names `Marker`. It works: the filter is answered from
        // the archetype's component set and never touches a cell, so it asks
        // for no borrow at all.
        let held = world.query::<&mut Marker>().next().expect("one entity");
        let count = world
            .query_filtered::<&bool, (With<Marker>, With<bool>)>()
            .count();
        assert_eq!(count, 1);
        drop(held);
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
