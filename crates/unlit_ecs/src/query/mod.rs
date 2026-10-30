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
//!
//! # Queries of your own
//!
//! A row that carries more than a tuple impl covers, or one whose components are
//! worth naming, is declared with the [`crate::query!`] macro:
//!
//! ```rust
//! # use unlit_ecs::{Entity, Has, World, bundle, query};
//! pub struct Health(pub i32);
//! pub struct Armor(pub i32);
//! pub struct Mana(pub i32);
//!
//! query! {
//!     /// Everything one combatant is made of.
//!     pub struct Combatant<'a> {
//!         pub entity: Entity,
//!         pub health: &'a mut Health,
//!         pub armor: &'a Armor,
//!         /// `None` for a row that carries no mana.
//!         pub mana: Option<&'a Mana>,
//!         pub has_armor: Has<Armor>,
//!     }
//! }
//!
//! let mut world = World::new();
//! world.root_scope_mut().spawn(bundle![Health(10), Armor(2)]);
//!
//! for row in world.root_scope().query::<Combatant>().iter() {
//!     row.health.0 -= row.armor.0;
//!     assert!(row.has_armor);
//!     assert!(row.mana.is_none());
//! }
//! ```
//!
//! The declaration is one row: the macro derives the item struct and the
//! [`Query`] impl from it. No `unsafe` is involved on the caller's side, and
//! the claims the query makes are exactly the fields it declares: a component is
//! claimed by asking the [`PlanBuilder`] for it, so there is no second list
//! that could disagree with what the query actually reaches.

use core::any::TypeId;
use core::cell::UnsafeCell;
use core::marker::PhantomData;

use crate::archetype::Archetype;
use crate::borrow::Claims;
use crate::entity::Entity;
use crate::world::World;

#[macro_use]
mod macros;
mod iter;

pub use iter::QueryIter;

/// Whether a component is reached shared or exclusive.
///
/// A query reports it per component while it resolves its plan, so the holder
/// knows what to claim before anything is handed out.
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

/// One component a query reaches, and how.
pub(crate) type Ask = (TypeId, &'static str, Mode);

/// A claimed column, shared.
///
/// Handed out by [`PlanBuilder::read`] and kept in a query's plan. It is
/// [`Copy`], because reading a column twice is not a conflict.
pub struct Col<'w, T: 'static> {
    cells: &'w [UnsafeCell<T>],
}

impl<T: 'static> Clone for Col<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> Copy for Col<'_, T> {}

impl<'w, T: 'static> Col<'w, T> {
    /// The value at `row`.
    ///
    /// It lives as long as the claim behind it, so nothing writes the column
    /// while any item names it.
    #[inline]
    pub fn at(&self, row: usize) -> &'w T {
        let cell = self
            .cells
            .get(row)
            .unwrap_or_else(|| panic!("row {row} is past the end of a claimed column"));
        // SAFETY: the holder claimed this column shared for 'w, so nothing
        // writes it while this reference lives, and the row is in bounds.
        unsafe { &*cell.get() }
    }

    /// The number of values in the column, one per row of the archetype.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the column holds no value.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// A claimed column, exclusive.
///
/// Handed out by [`PlanBuilder::write`] and kept in a query's plan. Unlike
/// [`Col`] it is not [`Copy`], and [`ColMut::at`] takes the handle
/// exclusively: that is what makes one [`Query::read`] hand out at most one
/// exclusive reference per column, which in turn is why declaring a query needs
/// no `unsafe`.
pub struct ColMut<'w, T: 'static> {
    cells: &'w [UnsafeCell<T>],
}

impl<'w, T: 'static> ColMut<'w, T> {
    /// The value at `row`, exclusively.
    ///
    /// Taking `&mut self` is the safety argument: the caller can reach the
    /// same cell twice only by calling this twice, and the first reference is
    /// still alive then, so the compiler refuses the second call.
    #[inline]
    pub fn at(&mut self, row: usize) -> &mut T {
        let cell = self
            .cells
            .get(row)
            .unwrap_or_else(|| panic!("row {row} is past the end of a claimed column"));
        // SAFETY: the holder claimed this column exclusively for the plan's
        // life, and this handle is the only way to the cells, so no other
        // reference to any row of it exists.
        unsafe { &mut *cell.get() }
    }

    /// The number of values in the column, one per row of the archetype.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the column holds no value.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// What one archetype offers, and what a query asks of it while resolving its
/// plan.
///
/// This is the only way a query reaches a column, so asking for one *is* the
/// claim: there is no separate list of reached components that could disagree
/// with what [`Query::read`] actually reads. A duplicate ask that would need
/// two modes for one type is refused here, before anything is handed out.
pub struct PlanBuilder<'w> {
    archetype: &'w Archetype,
    asked: Vec<Ask>,
    marker: PhantomData<&'w ()>,
}

impl<'w> PlanBuilder<'w> {
    pub(crate) fn new(archetype: &'w Archetype, asked: Vec<Ask>) -> Self {
        Self {
            archetype,
            asked,
            marker: PhantomData,
        }
    }

    /// The asks recorded so far, which is what the holder claims.
    pub(crate) fn asks(&self) -> &[Ask] {
        &self.asked
    }

    /// Recover the buffer, so the next archetype reuses it instead of
    /// allocating.
    pub(crate) fn into_asks(self) -> Vec<Ask> {
        self.asked
    }

    /// The archetype's column of `T`, shared. `None` when it carries no
    /// `T`, which is what makes the query skip it.
    pub fn read<T: 'static>(&mut self) -> Option<Col<'w, T>> {
        let cells = self.ask::<T>(Mode::Read)?;
        Some(Col { cells })
    }

    /// The archetype's column of `T`, exclusive. `None` when it carries no
    /// `T`.
    pub fn write<T: 'static>(&mut self) -> Option<ColMut<'w, T>> {
        let cells = self.ask::<T>(Mode::Write)?;
        Some(ColMut { cells })
    }

    /// Whether the archetype carries `T`.
    ///
    /// Claims nothing: a yes/no answer cannot alias anything, which is what
    /// keeps `(&mut T, Has<T>)` legal.
    #[inline]
    #[must_use]
    pub fn has<T: 'static>(&self) -> bool {
        self.archetype.has::<T>()
    }

    /// The entities of the archetype, one per row.
    #[inline]
    #[must_use]
    pub fn entities(&self) -> &'w [Entity] {
        self.archetype.entities()
    }

    /// Record that the query reaches `T` in `mode`, and resolve its column.
    ///
    /// Panics when the same query asks for `T` twice with at least one of the
    /// asks exclusive: the two would hand out references that cannot coexist.
    fn ask<T: 'static>(&mut self, mode: Mode) -> Option<&'w [UnsafeCell<T>]> {
        let type_id = TypeId::of::<T>();
        for (asked, name, asked_mode) in &self.asked {
            if *asked == type_id && (*asked_mode == Mode::Write || mode == Mode::Write) {
                panic!(
                    "component `{name}` is already borrowed {} by this query, and this {} cannot share it",
                    asked_mode.adverb(),
                    mode.verb(),
                );
            }
        }
        self.asked
            .push((type_id, core::any::type_name::<T>(), mode));
        self.archetype.column::<T>().map(|column| column.cells())
    }
}

/// What to fetch from every entity a query matches.
///
/// The type is the query: it says which components to hand out and which
/// archetypes to visit. A query is resolved once per archetype into
/// [`Query::Plan`] and then read once per row, so iterating costs one
/// component lookup per archetype rather than one per row.
///
/// The impls cover the forms listed in the [module documentation](self), and the
/// [`crate::query!`] macro derives this trait for a row of the caller's own.
///
/// Implementing it by hand needs no `unsafe`: the plan is built through a
/// [`PlanBuilder`], which claims the very columns it hands out.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a query",
    label = "a query is `&C`, `&mut C`, `Option<&C>`, `Option<&mut C>`, `Entity`, `Has<C>`, or a tuple of those",
    note = "`&(a, b)` is one tuple component and `(&a, &b)` is two; items do not nest, and a wider row is declared with `query!`"
)]
pub trait Query {
    /// What one matched row yields.
    ///
    /// It borrows from the holder that yielded it, never from the world, so an
    /// item cannot outlive the claims behind it.
    type Item<'a>;

    /// What is resolved once per archetype and then read once per row.
    ///
    /// It borrows from the world, so it may keep the columns of the archetype.
    /// A row is read through `&mut` of it, which is what keeps the exclusive
    /// references of one read from aliasing those of the next.
    type Plan<'w>;

    /// Resolve the plan for one archetype.
    ///
    /// `None` means the archetype does not match, and the query skips it.
    /// Asking the builder for a column the archetype lacks is how a required
    /// component narrows the query: `builder.read::<T>()?`.
    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>>;

    /// Read the row at `row` through the resolved plan.
    ///
    /// The plan is taken exclusively, so one call reaches at most one exclusive
    /// reference per column.
    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a>;
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
    type Item<'a> = &'a T;
    type Plan<'w> = Col<'w, T>;

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        builder.read::<T>()
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
        plan.at(row)
    }
}

impl<T: 'static> QueryItem for &T {}

/// Asks for `T` exclusive.
impl<T: 'static> Query for &mut T {
    type Item<'a> = &'a mut T;
    type Plan<'w> = ColMut<'w, T>;

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        builder.write::<T>()
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
        plan.at(row)
    }
}

impl<T: 'static> QueryItem for &mut T {}

/// Asks for `T` shared when the archetype carries it.
impl<T: 'static> Query for Option<&T> {
    type Item<'a> = Option<&'a T>;
    type Plan<'w> = Option<Col<'w, T>>;

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        Some(builder.read::<T>())
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
        plan.as_ref().map(|column| column.at(row))
    }
}

impl<T: 'static> QueryItem for Option<&T> {}

/// Asks for `T` exclusive when the archetype carries it.
impl<T: 'static> Query for Option<&mut T> {
    type Item<'a> = Option<&'a mut T>;
    type Plan<'w> = Option<ColMut<'w, T>>;

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        Some(builder.write::<T>())
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
        plan.as_mut().map(|column| column.at(row))
    }
}

impl<T: 'static> QueryItem for Option<&mut T> {}

/// Asks for the entity itself.
///
/// It is an item like any other, so it can be written first, last, or on its
/// own: a query over `Entity` alone yields every entity the scope sees.
impl Query for Entity {
    type Item<'a> = Entity;
    type Plan<'w> = &'w [Entity];

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        Some(builder.entities())
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
        plan[row]
    }
}

impl QueryItem for Entity {}

/// Yields whether the archetype carries `T`.
///
/// The answer is read from the archetype's component set, so it reaches no value
/// and never conflicts: `(&mut T, Has<T>)` is legal. Use it instead of
/// `Option<&T>::is_some()` to ask about a component without borrowing it.
///
/// It narrows nothing: every archetype matches, and a row whose archetype lacks
/// `T` yields `false`. That is what lets a query report presence for rows it
/// was going to visit anyway, and what keeps `query_one::<Has<T>>` answering
/// `Ok(false)` rather than an error for an entity that lacks `T`. To visit
/// only the archetypes carrying `T`, pair it with
/// [`crate::QueryBorrow::by`]: `.by(|archetype: &Archetype| archetype.has::<T>())`.
#[derive(Clone, Copy, Debug)]
pub struct Has<T>(PhantomData<fn() -> T>);

impl<T: 'static> Query for Has<T> {
    type Item<'a> = bool;
    type Plan<'w> = bool;

    fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
        Some(builder.has::<T>())
    }

    fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, _: usize) -> Self::Item<'a> {
        *plan
    }
}

impl<T: 'static> QueryItem for Has<T> {}

macro_rules! impl_tuple {
    ($($name:ident),+) => {
        /// Asks for every member, yielding a tuple of their items.
        impl<$($name: QueryItem),+> Query for ($($name,)+) {
            type Item<'a> = ($($name::Item<'a>,)+);
            type Plan<'w> = ($($name::Plan<'w>,)+);

            fn prepare<'w>(builder: &mut PlanBuilder<'w>) -> Option<Self::Plan<'w>> {
                Some(($($name::prepare(builder)?,)+))
            }

            fn read<'a, 'w: 'a>(plan: &'a mut Self::Plan<'w>, row: usize) -> Self::Item<'a> {
                #[expect(non_snake_case, reason = "the pattern binds one variable per tuple member")]
                let ($($name,)+) = plan;
                ($($name::read($name, row),)+)
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
impl_tuple!(Q0, Q1, Q2, Q3, Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q11, Q12, Q13, Q14);
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
/// the query reaches, archetype by archetype, before it hands out anything.
/// Iterating is [`QueryBorrow::iter`], and every item it yields borrows from
/// this holder, so the claims outlive the items by construction.
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
    /// The buffer a pass over the archetypes records its asks in, kept here so
    /// that a second pass reuses it rather than allocating again.
    asks: Vec<Ask>,
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
            asks: Vec::new(),
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
            asks: self.asks,
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
            &mut self.asks,
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
/// [`QueryOne::get`] is what reads the row, and it takes the holder
/// exclusively: an exclusive item borrows from the call, so the compiler refuses
/// a second one while the first is alive.
pub struct QueryOne<'w, Q: Query> {
    row: usize,
    plan: Q::Plan<'w>,
    /// Held for the life of the holder: dropping it is what releases the claim.
    _claims: Claims<'w>,
    marker: PhantomData<fn() -> Q>,
}

impl<'w, Q: Query> QueryOne<'w, Q> {
    pub(crate) fn new(plan: Q::Plan<'w>, row: usize, claims: Claims<'w>) -> Self {
        Self {
            row,
            plan,
            _claims: claims,
            marker: PhantomData,
        }
    }

    /// What the query yields for this entity's row.
    pub fn get(&mut self) -> Q::Item<'_> {
        Q::read(&mut self.plan, self.row)
    }
}

impl<Q: Query> core::fmt::Debug for QueryOne<'_, Q> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("QueryOne")
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
