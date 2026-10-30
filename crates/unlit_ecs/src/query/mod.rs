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
//! | [`Has<T>`] | whether the archetype carries `T`, narrowing nothing |
//!
//! A tuple of items asks for all of them at once and yields a tuple of their
//! items. Items do not nest: `&(a, b)` is one tuple component, `(&a, &b)` is
//! two components, and an item outside the forms above fails to compile.
//!
//! A query is held rather than borrowed one row at a time: [`crate::QueryBorrow`]
//! is what a scope hands out, it claims the components it will reach when it is
//! built, and the claims are released when it is dropped. Iterating it is
//! [`crate::QueryBorrow::iter`].
//!
//! ```rust
//! # use unlit_ecs::{Entity, Has, World, bundle};
//! let mut world = World::new();
//! world.root_scope_mut().spawn(bundle![1u32, 10.0f32]);
//! world.root_scope_mut().spawn(bundle![2u32, true]);
//!
//! let mut query = world.root_scope().query::<&u32>();
//! let sum: u32 = query.iter().map(|value| *value).sum();
//! assert_eq!(sum, 3);
//!
//! // The entity, its component, and whether it also carries a `bool`. The
//! // `Has<bool>` reports the answer without narrowing anything, so both spawns
//! // above are visited and the one without a `bool` prints `false`.
//! for (entity, value, has_bool) in world.root_scope().query::<(Entity, &u32, Has<bool>)>().iter() {
//!     println!("{entity:?} has {value}, and a bool: {has_bool}");
//! }
//! ```
//!
//! A query visits archetypes in creation order and rows in storage order, so
//! iteration is deterministic. It skips the rows the scope cannot see and the
//! tree's two scaffolding entities.
//!
//! Narrowing which archetypes a query visits, including by a component set only
//! known at run time, is [`crate::QueryBorrow::by`].

use core::any::TypeId;
use core::marker::PhantomData;

use crate::archetype::Archetype;
use crate::borrow::Claims;
use crate::column::Column;
use crate::entity::Entity;
use crate::world::World;

mod iter;

pub use iter::QueryIter;

/// Whether a component is reached shared or exclusive.
///
/// Reported per component by [`Query::access`], so a query knows what to claim
/// before it hands anything out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reached shared.
    Read,
    /// Reached exclusive.
    Write,
}

impl Mode {
    /// The verb a conflict message uses for this mode as a requirement.
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// How a conflict message describes a claim already held in this mode.
    pub(crate) fn adverb(self) -> &'static str {
        match self {
            Self::Read => "shared",
            Self::Write => "exclusively",
        }
    }
}

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
///
/// # Safety
///
/// [`Query::access`] has to report every component the query reaches and whether
/// it reaches it shared or exclusive, and [`Query::fetch`] has to reach nothing
/// else. The holder claims exactly what `access` reports and derives the
/// references `fetch` hands out from them; an impl that under-reports is what
/// lets two exclusive references to one value exist.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a query",
    label = "a query is `&C`, `&mut C`, `Option<&C>`, `Option<&mut C>`, `Entity`, `Has<C>`, or a tuple of those",
    note = "`&(a, b)` is one tuple component and `(&a, &b)` is two; items do not nest"
)]
pub unsafe trait Query {
    /// What one matched row yields.
    type Item<'a>;

    /// What is resolved once per archetype and then read once per row.
    ///
    /// Copied out for every row, so it describes where the values are rather
    /// than owning them.
    type Fetch<'a>: Copy;

    /// Whether an archetype carries the components this query requires.
    fn matches(archetype: &Archetype) -> bool;

    /// Resolve the per-archetype state.
    ///
    /// Only called for an archetype [`Query::matches`] accepted.
    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_>;

    /// Read the row at `row` through the resolved state.
    ///
    /// # Safety
    ///
    /// The caller must hold a claim on every component the query reaches on this
    /// archetype, shared for the shared items and exclusive for the exclusive
    /// ones, and must not let the item outlive it.
    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a>;

    /// Report the components this query reaches, and how.
    ///
    /// Items that borrow nothing, such as [`Has<T>`] and [`Entity`], report
    /// nothing: naming one beside an item that reaches the same component is not
    /// a conflict.
    fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode));
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
unsafe impl<T: 'static> Query for &T {
    type Item<'a> = &'a T;
    type Fetch<'a> = &'a Column<T>;

    fn matches(archetype: &Archetype) -> bool {
        archetype.has::<T>()
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = state.cells().get(row).unwrap_or_else(|| missing::<T>());
        // SAFETY: the holder claimed this archetype's `T` shared, so nothing
        // writes it and no exclusive reference to it exists.
        unsafe { &*cell.get() }
    }

    fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>(), Mode::Read);
    }
}

impl<T: 'static> QueryItem for &T {}

/// Asks for `T` exclusive.
unsafe impl<T: 'static> Query for &mut T {
    type Item<'a> = &'a mut T;
    type Fetch<'a> = &'a Column<T>;

    fn matches(archetype: &Archetype) -> bool {
        archetype.has::<T>()
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>().unwrap_or_else(|| missing::<T>())
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = state.cells().get(row).unwrap_or_else(|| missing::<T>());
        // SAFETY: the holder claimed this archetype's `T` exclusively, so neither
        // another exclusive reference nor a shared one exists.
        unsafe { &mut *cell.get() }
    }

    fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>(), Mode::Write);
    }
}

impl<T: 'static> QueryItem for &mut T {}

/// Asks for `T` shared when the archetype carries it.
unsafe impl<T: 'static> Query for Option<&T> {
    type Item<'a> = Option<&'a T>;
    type Fetch<'a> = Option<&'a Column<T>>;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>()
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = state?.cells().get(row)?;
        // SAFETY: where the archetype carries `T` the holder claimed it shared;
        // where it does not, this never runs.
        Some(unsafe { &*cell.get() })
    }

    fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>(), Mode::Read);
    }
}

impl<T: 'static> QueryItem for Option<&T> {}

/// Asks for `T` exclusive when the archetype carries it.
unsafe impl<T: 'static> Query for Option<&mut T> {
    type Item<'a> = Option<&'a mut T>;
    type Fetch<'a> = Option<&'a Column<T>>;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.column::<T>()
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        let cell = state?.cells().get(row)?;
        // SAFETY: where the archetype carries `T` the holder claimed it
        // exclusively; where it does not, this never runs.
        Some(unsafe { &mut *cell.get() })
    }

    fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode)) {
        visit(TypeId::of::<T>(), core::any::type_name::<T>(), Mode::Write);
    }
}

impl<T: 'static> QueryItem for Option<&mut T> {}

/// Asks for the entity itself.
///
/// It is an item like any other, so it can be written first, last, or on its
/// own: a query over `Entity` alone yields every entity the scope sees.
unsafe impl Query for Entity {
    type Item<'a> = Entity;
    type Fetch<'a> = &'a Archetype;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
        state.entities()[row]
    }

    fn access(_: &mut dyn FnMut(TypeId, &'static str, Mode)) {}
}

impl QueryItem for Entity {}

/// Yields whether the archetype carries `T`.
///
/// The answer is read from the archetype's component set, so it reaches no value
/// and never conflicts: `(&mut T, Has<T>)` is legal. Use it instead of
/// `Option<&T>::is_some()` to ask about a component without borrowing it.
///
/// It narrows nothing: every archetype matches, and a row whose archetype lacks
/// `T` yields `false`. That is what lets a query report presence for rows it was
/// going to visit anyway, and what keeps `query_one::<Has<T>>` answering
/// `Ok(false)` rather than an error for an entity that lacks `T`. To visit only
/// the archetypes carrying `T`, pair it with [`crate::QueryBorrow::by`]:
/// `.by(|archetype: &Archetype| archetype.has::<T>())`.
#[derive(Clone, Copy, Debug)]
pub struct Has<T>(PhantomData<fn() -> T>);

unsafe impl<T: 'static> Query for Has<T> {
    type Item<'a> = bool;
    type Fetch<'a> = bool;

    fn matches(_: &Archetype) -> bool {
        true
    }

    fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
        archetype.has::<T>()
    }

    unsafe fn fetch<'a>(state: Self::Fetch<'a>, _: usize) -> Self::Item<'a> {
        state
    }

    fn access(_: &mut dyn FnMut(TypeId, &'static str, Mode)) {}
}

impl<T: 'static> QueryItem for Has<T> {}

macro_rules! impl_tuple {
    ($($name:ident),+) => {
        /// Asks for every member, yielding a tuple of their items.
        unsafe impl<$($name: QueryItem),+> Query for ($($name,)+) {
            type Item<'a> = ($($name::Item<'a>,)+);
            type Fetch<'a> = ($($name::Fetch<'a>,)+);

            fn matches(archetype: &Archetype) -> bool {
                $($name::matches(archetype))&&+
            }

            fn fetch_state(archetype: &Archetype) -> Self::Fetch<'_> {
                ($($name::fetch_state(archetype),)+)
            }

            unsafe fn fetch<'a>(state: Self::Fetch<'a>, row: usize) -> Self::Item<'a> {
                #[expect(non_snake_case, reason = "the pattern binds one variable per tuple member")]
                let ($($name,)+) = state;
                // SAFETY: the holder claimed each member's components, which is
                // what each call's contract asks for.
                unsafe { ($($name::fetch($name, row),)+) }
            }

            fn access(visit: &mut dyn FnMut(TypeId, &'static str, Mode)) {
                $($name::access(visit);)+
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
/// An unadorned query accepts every archetype its items match.
/// [`crate::QueryBorrow::by`] installs a filter.
///
/// A filter is answered from the archetype alone, before any row is fetched and
/// before any value is reached. Deciding there rather than skipping rows in the
/// loop is both faster -- a rejected archetype costs one test instead of one per
/// row -- and correct when the rows it rejects carry a component the caller
/// reaches exclusively.
///
/// Implemented for `()`, which accepts everything, and for any
/// `Fn(&Archetype) -> bool`. A closure can decide from anything an archetype
/// knows, including a component set only known at run time, which a type-written
/// query cannot name.
///
/// ```rust
/// # use unlit_ecs::{Archetype, World, bundle};
/// let mut world = World::new();
/// world.root_scope_mut().spawn(bundle![1u32, true]);
/// world.root_scope_mut().spawn(bundle![2u32]);
///
/// let mut query = world
///     .root_scope()
///     .query::<&u32>()
///     .by(|archetype: &Archetype| !archetype.has::<bool>());
/// let sum: u32 = query.iter().map(|value| *value).sum();
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
/// What a scope hands out for a query: the claims, and the way to iterate them.
///
/// The holder is created by [`crate::Scope::query`] and claims the components
/// the query reaches before it hands out anything. Iterating is
/// [`QueryBorrow::iter`], and every item it yields borrows from this holder, so
/// the claims outlive the items by construction.
///
/// Dropping the holder releases the claims. Forgetting it instead
/// ([`core::mem::forget`]) leaks them: the components stay claimed for the rest
/// of the world's life, and the next query for them panics rather than running
/// alongside.
pub struct QueryBorrow<'w, Q: Query, F: ArchetypeFilter = ()> {
    world: &'w World,
    scope: Entity,
    filter: F,
    claims: Claims<'w>,
    origin: &'static core::panic::Location<'static>,
    marker: PhantomData<fn() -> Q>,
}

impl<'w, Q: Query> QueryBorrow<'w, Q, ()> {
    pub(crate) fn new(
        world: &'w World,
        scope: Entity,
        origin: &'static core::panic::Location<'static>,
    ) -> Self {
        Self {
            world,
            scope,
            filter: (),
            claims: Claims::new(world),
            origin,
            marker: PhantomData,
        }
    }
}

impl<'w, Q: Query, F: ArchetypeFilter> QueryBorrow<'w, Q, F> {
    /// Visit only the archetypes the filter accepts.
    ///
    /// The filter sees an archetype before any of its rows is fetched and before
    /// any value is reached, so rejecting an archetype costs one test rather than
    /// one per row, and works even when that archetype's components are already
    /// claimed elsewhere. See [`ArchetypeFilter`].
    ///
    /// It may be applied part-way through an iteration, and then narrows the
    /// archetype being visited too: what the filter rejects is not fetched, and
    /// the rows already yielded stay yielded.
    #[must_use]
    pub fn by<G: ArchetypeFilter>(self, filter: G) -> QueryBorrow<'w, Q, G> {
        QueryBorrow {
            world: self.world,
            scope: self.scope,
            filter,
            claims: self.claims,
            origin: self.origin,
            marker: PhantomData,
        }
    }

    /// Iterate the visible entities that match `Q`.
    ///
    /// The items borrow from this holder, so they cannot outlive it and it
    /// cannot be iterated for one item at a time while another is alive: collect
    /// them, or finish with each row before reaching for the next.
    pub fn iter(&mut self) -> QueryIter<'_, 'w, Q> {
        QueryIter::new(
            self.world,
            self.scope,
            &self.filter,
            &mut self.claims,
            self.origin,
        )
    }
}

/// What a scope hands out for a query over one entity.
///
/// Made by [`crate::Scope::query_one`], which claims the components the query
/// reaches on that entity's row. A missing match is still answered with an
/// error rather than a holder, so a caller that finds nothing holds no claim.
///
/// [`QueryOne::get`] is what reads the row, and it takes the holder exclusively:
/// an exclusive item borrows from the call, so the compiler refuses a second one
/// while the first is alive.
pub struct QueryOne<'w, Q: Query> {
    world: &'w World,
    archetype: u32,
    row: usize,
    /// Held for the life of the holder: dropping it is what releases the claim.
    _claims: Claims<'w>,
    marker: PhantomData<fn() -> Q>,
}

impl<'w, Q: Query> QueryOne<'w, Q> {
    pub(crate) fn new(
        world: &'w World,
        archetype: u32,
        archetype_ref: &Archetype,
        entity: Entity,
        row: usize,
        origin: &'static core::panic::Location<'static>,
    ) -> Self {
        let mut claims = Claims::new(world);
        claims.claim_row::<Q>(archetype, entity, archetype_ref, origin);
        Self {
            world,
            archetype,
            row,
            _claims: claims,
            marker: PhantomData,
        }
    }

    /// What the query yields for this entity's row.
    pub fn get(&mut self) -> Q::Item<'_> {
        let archetype = self
            .world
            .archetypes()
            .get(self.archetype)
            .expect("a live entity's location names a registered archetype");
        let state = Q::fetch_state(archetype);
        // SAFETY: `QueryOne::new` claimed what `Q::access` reports on this row,
        // and the item borrows from the holder that owns the claims.
        unsafe { Q::fetch(state, self.row) }
    }
}

impl<Q: Query> core::fmt::Debug for QueryOne<'_, Q> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("QueryOne")
            .field("archetype", &self.archetype)
            .field("row", &self.row)
            .finish_non_exhaustive()
    }
}

impl<Q: Query, F: ArchetypeFilter> core::fmt::Debug for QueryBorrow<'_, Q, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("QueryBorrow")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
