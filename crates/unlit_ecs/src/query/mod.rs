//! Queries: picking entities by the components they carry.
//!
//! A query is a type, not a value. The components to hand out are written in the
//! turbofish, so the compiler checks them, and the iterator resolves them once
//! per archetype instead of once per row.
//!
//! | Item | Yields |
//! | --- | --- |
//! | `&T` | the component, shared; only archetypes carrying it match |
//! | `&mut T` | the component, exclusive; only archetypes carrying it match |
//! | `Option<&T>` | `Some` when the archetype carries the component |
//! | `Option<&mut T>` | `Some` when it carries it, exclusive |
//! | `Entity` | the entity itself |
//! | [`Has<T>`] | whether the archetype carries `T` |
//!
//! A tuple of items asks for all of them at once and yields a tuple of their
//! items. Items do not nest: `&(a, b)` is one tuple component, `(&a, &b)` is
//! two components, and an item outside the forms above fails to compile.
//!
//! ```rust
//! # use unlit_ecs::{Entity, Has, World, bundle};
//! let mut world = World::new();
//! world.root_scope_mut().spawn(bundle!(1u32, 10.0f32));
//! world.root_scope_mut().spawn(bundle!(2u32, true));
//!
//! let sum: u32 = world.root_scope().query::<&u32>().map(|value| *value).sum();
//! assert_eq!(sum, 3);
//!
//! // The entity, its component, and whether it also carries a `bool`.
//! for (entity, value, has_bool) in world.root_scope().query::<(Entity, &u32, Has<bool>)>() {
//!     println!("{entity:?} has {value}, and a bool: {has_bool}");
//! }
//! ```
//!
//! A query visits archetypes in creation order and rows in storage order, so
//! iteration is deterministic. It skips the rows the scope cannot see and the
//! tree's two scaffolding entities.
//!
//! Narrowing which archetypes a query visits, including by a component set only
//! known at run time, is [`QueryIter::by`].

use core::any::TypeId;
use core::marker::PhantomData;

use crate::archetype::Archetype;
use crate::column::{CellRef, CellRefMut, Column};
use crate::entity::Entity;

mod iter;

pub use iter::QueryIter;

/// Panics when an archetype a query matched unexpectedly lacks a component.
///
/// Only a hand-written [`Query`] whose `matches` disagrees with its
/// `fetch_state` can reach this.
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
/// The type is the query: it says which components to hand out and which
/// archetypes to visit. An iterator resolves a query's per-archetype state once
/// and then reads every row through it, so a query costs one component lookup
/// per archetype rather than one per row.
///
/// The impls cover the forms listed in the [module documentation](self). A caller
/// with a shape none of them expresses implements this trait itself: `matches`
/// says which archetypes it visits, `fetch_state` resolves what it needs from
/// one of them, and `fetch` reads one row.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a query",
    label = "a query is `&C`, `&mut C`, `Option<&C>`, `Option<&mut C>`, `Entity`, `Has<C>`, or a tuple of those",
    note = "`&(a, b)` is one tuple component and `(&a, &b)` is two; items do not nest"
)]
pub trait Query {
    /// What one matched row yields.
    type Item<'a>;

    /// What is resolved once per archetype and then read once per row.
    type Fetch<'a>;

    /// Whether an archetype carries the components this query requires.
    fn matches(archetype: &Archetype) -> bool;

    /// Resolve the per-archetype state.
    ///
    /// Only called for an archetype [`Query::matches`] accepted.
    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_>;

    /// Read the row at `row` through the resolved state.
    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a>;

    /// Report the components this query names, for the development-time check
    /// that none of them is named twice.
    ///
    /// Items that borrow nothing, such as [`Has<T>`] and `Entity`, report
    /// nothing: naming one beside an item that borrows the same component is not
    /// a conflict.
    #[doc(hidden)]
    fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str));
}

/// A query that can be one element of a query tuple.
///
/// The tuple impls require this rather than [`Query`], which is what keeps
/// items from nesting: `(&a, &b)` is two components, `&(a, b)` is one, and
/// `(&a, (&b, &c))` does not compile.
#[diagnostic::on_unimplemented(
    message = "not a query item",
    label = "an item is `&C`, `&mut C`, `Option<&C>`, `Option<&mut C>`, `Entity`, or `Has<C>`",
    note = "`&(a, b)` is one tuple component and `(&a, &b)` is two; items do not nest"
)]
pub trait QueryItem: Query {}

/// Asks for `T` shared.
impl<T: 'static> Query for &T {
    type Item<'a> = CellRef<'a, T>;
    type Fetch<'a> = &'a Column<T>;

    fn matches(archetype: &Archetype) -> bool {
        archetype.has::<T>()
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        state
            .cell(row)
            .unwrap_or_else(|| missing::<T>())
            .try_borrow()
            .unwrap_or_else(|_| borrowed::<T>("read"))
    }

    fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>());
    }
}

impl<T: 'static> QueryItem for &T {}

/// Asks for `T` exclusive.
impl<T: 'static> Query for &mut T {
    type Item<'a> = CellRefMut<'a, T>;
    type Fetch<'a> = &'a Column<T>;

    fn matches(archetype: &Archetype) -> bool {
        archetype.has::<T>()
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        state
            .cell(row)
            .unwrap_or_else(|| missing::<T>())
            .try_borrow_mut()
            .unwrap_or_else(|_| borrowed::<T>("write"))
    }

    fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>());
    }
}

impl<T: 'static> QueryItem for &mut T {}

/// Asks for `T` shared when the archetype carries it.
impl<T: 'static> Query for Option<&T> {
    type Item<'a> = Option<CellRef<'a, T>>;
    type Fetch<'a> = Option<&'a Column<T>>;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>()
    }

    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = (*state)?.cell(row)?;
        Some(cell.try_borrow().unwrap_or_else(|_| borrowed::<T>("read")))
    }

    fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>());
    }
}

impl<T: 'static> QueryItem for Option<&T> {}

/// Asks for `T` exclusive when the archetype carries it.
impl<T: 'static> Query for Option<&mut T> {
    type Item<'a> = Option<CellRefMut<'a, T>>;
    type Fetch<'a> = Option<&'a Column<T>>;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>()
    }

    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = (*state)?.cell(row)?;
        Some(
            cell.try_borrow_mut()
                .unwrap_or_else(|_| borrowed::<T>("write")),
        )
    }

    fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>());
    }
}

impl<T: 'static> QueryItem for Option<&mut T> {}

/// Asks for the entity itself.
///
/// It is an item like any other, so it can be written first, last, or on its
/// own: a query over `Entity` alone yields every entity the scope sees.
impl Query for Entity {
    type Item<'a> = Entity;
    type Fetch<'a> = &'a Archetype;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype
    }

    fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        state.entities()[row]
    }

    fn type_ids(_: &mut dyn FnMut(TypeId, &'static str)) {}
}

impl QueryItem for Entity {}

/// Asks whether the archetype carries `T`.
///
/// The answer is read from the archetype, so it borrows no cell and never
/// conflicts: `(&mut T, Has<T>)` is legal and yields `true` for every row of
/// a match. Use it instead of `Option<&T>::is_some()` to ask about a component
/// without borrowing it.
#[derive(Clone, Copy, Debug)]
pub struct Has<T>(PhantomData<fn() -> T>);

impl<T: 'static> Query for Has<T> {
    type Item<'a> = bool;
    type Fetch<'a> = ();

    fn matches(archetype: &Archetype) -> bool {
        archetype.has::<T>()
    }

    fn fetch_state(_: &Archetype) -> Self::Fetch<'_> {}

    fn fetch<'a>(_: &Self::Fetch<'a>, _: usize) -> Self::Item<'a> {
        true
    }

    fn type_ids(_: &mut dyn FnMut(TypeId, &'static str)) {}
}

impl<T: 'static> QueryItem for Has<T> {}

macro_rules! impl_tuple {
    ($($name:ident),+) => {
        /// Asks for every member, yielding a tuple of their items.
        impl<$($name: QueryItem),+> Query for ($($name,)+) {
            type Item<'a> = ($($name::Item<'a>,)+);
            type Fetch<'a> = ($($name::Fetch<'a>,)+);

            fn matches(archetype: &Archetype) -> bool {
                $($name::matches(archetype))&&+
            }

            fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
                ($($name::fetch_state(archetype),)+)
            }

            fn fetch<'a>(state: &Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
                #[expect(non_snake_case, reason = "the pattern binds one variable per tuple member")]
                let ($($name,)+) = state;
                ($($name::fetch($name, row),)+)
            }

            fn type_ids(visit: &mut dyn FnMut(TypeId, &'static str)) {
                $($name::type_ids(visit);)+
            }
        }
    };
}

impl_tuple!(Q0);
impl_tuple!(Q0, Q1);
impl_tuple!(Q0, Q1, Q2);
impl_tuple!(Q0, Q1, Q2, Q3);
impl_tuple!(Q0, Q1, Q2, Q3, Q4);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11, Q12);
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11, Q12, Q13);
impl_tuple!(
    Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11, Q12, Q13, Q14
);
impl_tuple!(
    Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11, Q12, Q13, Q14, Q15
);

/// Narrows the archetypes a query visits.
///
/// An unadorned `query::<Q>()` accepts every archetype its items match.
/// [`QueryIter::by`] installs a filter.
///
/// A filter is answered from the archetype alone, before any row is fetched and
/// before any cell is borrowed. Deciding there rather than skipping rows in the
/// loop is both faster -- a rejected archetype costs one test instead of one per
/// row -- and correct when the rows it rejects carry a component the caller has
/// already borrowed exclusively.
///
/// Implemented for `()`, which accepts everything, and for any
/// `Fn(&Archetype) -> bool`. A closure can decide from anything an archetype
/// knows, including a component set only known at run time, which a type-written
/// query cannot name.
///
/// ```rust
/// # use unlit_ecs::{Archetype, World, bundle};
/// let mut world = World::new();
/// world.root_scope_mut().spawn(bundle!(1u32, true));
/// world.root_scope_mut().spawn(bundle!(2u32));
///
/// let sum: u32 = world
///     .root_scope()
///     .query::<&u32>()
///     .by(|archetype: &Archetype| !archetype.has::<bool>())
///     .map(|value| *value)
///     .sum();
/// assert_eq!(sum, 2);
/// ```
pub trait ArchetypeFilter {
    /// Whether entities of `archetype` belong to this query.
    fn accept(&self, archetype: &Archetype) -> bool;
}

impl ArchetypeFilter for () {
    fn accept(&self, _: &Archetype) -> bool {
        true
    }
}

impl<F: Fn(&Archetype) -> bool> ArchetypeFilter for F {
    fn accept(&self, archetype: &Archetype) -> bool {
        self(archetype)
    }
}

/// Panics when a query names one component twice.
///
/// Type ids cannot be compared in a `const` context, so this is the earliest
/// place a query can be checked. It is a development-time check: in a release
/// build the conflict surfaces as the cell that refuses the second borrow.
#[cfg(debug_assertions)]
#[track_caller]
pub(crate) fn assert_no_duplicates<Q: Query>(origin: &'static core::panic::Location<'static>) {
    /// Enough for any tuple the impls cover; a hand-written query naming more
    /// distinct components than this goes unchecked rather than panicking.
    const CAPACITY: usize = 16;

    let mut names: [&'static str; CAPACITY] = [""; CAPACITY];
    let mut ids = [TypeId::of::<()>(); CAPACITY];
    let mut len = 0;
    Q::type_ids(&mut |type_id, name| {
        assert!(
            !ids[..len].contains(&type_id),
            "a query cannot name `{name}` twice, in {origin}",
        );
        if len < CAPACITY {
            ids[len] = type_id;
            names[len] = name;
            len += 1;
        }
    });
}
