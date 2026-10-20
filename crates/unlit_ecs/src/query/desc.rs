//! What a query asks for, as data.
//!
//! A query is a list of [`Slot`]s plus a [`Filter`]. The list says which components to hand out
//! and how; the filter says which archetypes to visit. Both are plain data, so
//! a query can be built at run time from type ids, and a constant one costs
//! nothing.
//!
//! The [`query!`](crate::query!) macro turns the reference syntax into that data:
//!
//! ````rust
//! # use unlit_ecs::{bundle, query, World};
//! let mut world = World::new();
//! world.root_scope_mut().spawn(bundle!(1u32, 2.0f32));
//! let sum: u32 = world
//!     .root_scope()
//!     .query(query!(&u32))
//!     .map(|(_, row)| *row.read::<u32>())
//!     .sum();
//! assert_eq!(sum, 1);
//! ````

use core::any::TypeId;
use core::marker::PhantomData;

use crate::archetype::Archetype;

/// How a query wants a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// Shared, through [`Row::read`](crate::Row::read).
    Read,
    /// Exclusive, through [`Row::write`](crate::Row::write).
    Write,
}

/// One component a query names, and how it wants it.
///
/// A required slot makes the query skip every archetype that lacks the
/// component; an optional one leaves those archetypes in and answers `None`
/// from [`Row::try_read`](crate::Row::try_read).
///
/// A slot is built by a [`query!`](crate::query!) invocation or by the [`Query`] builders. The
/// fields are private so that the representation can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Slot {
    type_id: TypeId,
    access: Access,
    required: bool,
}

impl Slot {
    /// A component every match must have, read shared.
    #[must_use]
    pub const fn read<T: 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            access: Access::Read,
            required: true,
        }
    }

    /// A component every match must have, read exclusive.
    #[must_use]
    pub const fn write<T: 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            access: Access::Write,
            required: true,
        }
    }

    /// A component a match may lack, read shared.
    #[must_use]
    pub const fn optional_read<T: 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            access: Access::Read,
            required: false,
        }
    }

    /// A component a match may lack, read exclusive.
    #[must_use]
    pub const fn optional_write<T: 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            access: Access::Write,
            required: false,
        }
    }

    /// The component this slot names.
    #[must_use]
    pub const fn type_id(self) -> TypeId {
        self.type_id
    }

    /// How the slot wants the component.
    #[must_use]
    pub const fn access(self) -> Access {
        self.access
    }

    /// Whether a match must have the component.
    #[must_use]
    pub const fn is_required(self) -> bool {
        self.required
    }
}

/// What to fetch from every entity a query matches.
///
/// A query holds a fixed-size list of `Slot`s and a `Filter`. The list is
/// at most `Query::MAX_SLOTS` long and lives inside the value, so a query
/// contains no allocation, is `Copy`, and can be promoted to a constant:
/// that is what makes a `query!` invocation a `&'static Query`.
///
/// A query is built either by a `query!` invocation, which resolves to a
/// constant slot list, or by the builders on this type, which append slots one
/// at a time. The two produce the same value.
///
/// ```rust
/// # use unlit_ecs::{Query, Slot};
/// let query = Query::new().read::<u32>().write::<f32>();
/// assert_eq!(query.slots(), [Slot::read::<u32>(), Slot::write::<f32>()]);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Query {
    /// The named slots, then `Query::MAX_SLOTS` minus `Query::len` fillers.
    slots: [Slot; Query::MAX_SLOTS],
    /// How many entries of `slots` are named.
    len: u8,
    filter: Filter,
}

impl Default for Query {
    fn default() -> Self {
        Self::new()
    }
}

impl Query {
    /// How many components one query can name.
    pub const MAX_SLOTS: usize = 16;

    /// The slot filling the unused tail of the array. Its component is never
    /// looked at, because `Query::slots` cuts the array at `Query::len`.
    const FILLER: Slot = Slot::read::<()>();

    /// A query that fetches nothing and visits every archetype.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Self::FILLER; Self::MAX_SLOTS],
            len: 0,
            filter: Filter::True,
        }
    }

    /// A query over a constant slot list, as `query!` builds one.
    ///
    /// # Panics
    ///
    /// When `slots` is longer than `Query::MAX_SLOTS`. The check is a
    /// constant one when the argument is constant, so a `query!` invocation
    /// naming too many components fails to compile.
    #[must_use]
    pub const fn from_static(slots: &[Slot], filter: Filter) -> Self {
        assert!(
            slots.len() <= Self::MAX_SLOTS,
            "a query names more components than Query::MAX_SLOTS",
        );
        let mut array = [Self::FILLER; Self::MAX_SLOTS];
        let mut index = 0;
        while index < slots.len() {
            array[index] = slots[index];
            index += 1;
        }
        Self {
            slots: array,
            len: slots.len() as u8,
            filter,
        }
    }

    /// Fetch `T` shared. Every match must have it.
    ///
    /// # Panics
    ///
    /// When `T` is already named exclusively, which would be two conflicting
    /// borrows of one cell, or when the query already names
    /// `Query::MAX_SLOTS` components.
    #[must_use]
    pub fn read<T: 'static>(self) -> Self {
        self.push(Slot::read::<T>())
    }

    /// Fetch `T` exclusive. Every match must have it.
    ///
    /// # Panics
    ///
    /// When `T` is already named, shared or exclusive, or when the query
    /// already names `Query::MAX_SLOTS` components.
    #[must_use]
    pub fn write<T: 'static>(self) -> Self {
        self.push(Slot::write::<T>())
    }

    /// Fetch `T` shared when the match has it.
    ///
    /// # Panics
    ///
    /// When `T` is already named exclusively, or when the query already names
    /// `Query::MAX_SLOTS` components.
    #[must_use]
    pub fn optional_read<T: 'static>(self) -> Self {
        self.push(Slot::optional_read::<T>())
    }

    /// Fetch `T` exclusive when the match has it.
    ///
    /// # Panics
    ///
    /// When `T` is already named exclusively, or when the query already names
    /// `Query::MAX_SLOTS` components.
    #[must_use]
    pub fn optional_write<T: 'static>(self) -> Self {
        self.push(Slot::optional_write::<T>())
    }

    /// Narrow the archetypes the query visits.
    #[must_use]
    pub const fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// The slots, in the order they were named.
    #[must_use]
    pub fn slots(&self) -> &[Slot] {
        &self.slots[..self.len as usize]
    }

    /// The filter narrowing the archetypes visited.
    #[must_use]
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// The slot naming `type_id`, when the query names it at all.
    #[must_use]
    pub fn slot(&self, type_id: TypeId) -> Option<Slot> {
        self.slots()
            .iter()
            .copied()
            .find(|slot| slot.type_id == type_id)
    }

    /// Whether an archetype satisfies every required slot and the filter.
    #[must_use]
    pub fn matches_archetype(&self, archetype: &Archetype) -> bool {
        self.slots()
            .iter()
            .all(|slot| !slot.required || archetype.column_index(slot.type_id).is_some())
            && self.filter.matches(archetype)
    }

    /// Check that no component is named in conflicting ways.
    ///
    /// `query!` assembles its slots in a `const` block, where `TypeId`s cannot
    /// be compared, so the check runs when the query is first iterated. The
    /// builders catch the same mistakes as they add the slot.
    ///
    /// # Panics
    ///
    /// When one component is named both shared and exclusive, exclusively
    /// twice, or both required and optional.
    pub(crate) fn assert_consistent(&self) {
        let slots = self.slots();
        for (index, slot) in slots.iter().enumerate() {
            for other in &slots[index + 1..] {
                if slot.type_id != other.type_id {
                    continue;
                }
                assert!(
                    slot.access == other.access,
                    "a component cannot be queried both shared and exclusive",
                );
                assert!(
                    slot.access == Access::Read,
                    "a component cannot be queried exclusive twice",
                );
                assert!(
                    slot.required == other.required,
                    "a component cannot be both required and optional in one query",
                );
            }
        }
    }

    /// Add `slot`, merging it with one that names the same component.
    ///
    /// Naming a component twice the same way keeps one slot; naming it both
    /// ways, or exclusively twice, panics, because the two slots would borrow
    /// one cell in conflicting ways.
    fn push(mut self, slot: Slot) -> Self {
        if let Some(existing) = self.slot(slot.type_id) {
            assert!(
                existing.access == slot.access,
                "a component cannot be queried both shared and exclusive",
            );
            assert!(
                slot.access == Access::Read,
                "a component cannot be queried exclusive twice",
            );
            if slot.required && !existing.required {
                let required = Slot {
                    required: true,
                    ..existing
                };
                self.replace(required);
            }
            return self;
        }
        let len = self.len as usize;
        assert!(
            len < Self::MAX_SLOTS,
            "a query names more components than Query::MAX_SLOTS",
        );
        self.slots[len] = slot;
        self.len += 1;
        self
    }

    fn replace(&mut self, slot: Slot) {
        let index = self
            .slots()
            .iter()
            .position(|existing| existing.type_id == slot.type_id)
            .expect("the slot is there to be replaced");
        self.slots[index] = slot;
    }
}

/// Which archetypes a query visits, as data.
///
/// The filter is answered from the archetype alone: an archetype either has a
/// component or it does not, so a filter needs no per-row work and no borrow of
/// any cell. [`Filter::True`] accepts everything, which is what a query with no
/// filter uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Every archetype passes.
    True,
    /// Only archetypes with the component.
    With(TypeId),
    /// Only archetypes without the component.
    Without(TypeId),
    /// Only archetypes every member accepts: the conjunction a tuple of filters
    /// used to be.
    All(&'static [Filter]),
    /// Archetypes at least one member accepts: the disjunction [`Or`] used to be.
    Any(&'static [Filter]),
}

impl Filter {
    /// Accept archetypes that have `T`.
    #[must_use]
    pub const fn with<T: 'static>() -> Self {
        Self::With(TypeId::of::<T>())
    }

    /// Accept archetypes that lack `T`.
    #[must_use]
    pub const fn without<T: 'static>() -> Self {
        Self::Without(TypeId::of::<T>())
    }

    /// Accept archetypes every member accepts.
    #[must_use]
    pub const fn all(filters: &'static [Filter]) -> Self {
        Self::All(filters)
    }

    /// Accept archetypes at least one member accepts.
    #[must_use]
    pub const fn any(filters: &'static [Filter]) -> Self {
        Self::Any(filters)
    }

    /// Whether entities of `archetype` pass this filter.
    #[must_use]
    pub fn matches(&self, archetype: &Archetype) -> bool {
        match *self {
            Self::True => true,
            Self::With(type_id) => archetype.column_index(type_id).is_some(),
            Self::Without(type_id) => archetype.column_index(type_id).is_none(),
            Self::All(filters) => filters.iter().all(|filter| filter.matches(archetype)),
            Self::Any(filters) => filters.iter().any(|filter| filter.matches(archetype)),
        }
    }
}

/// Requires the component `T`, without fetching it.
///
/// A filter is data, so this type is only the syntax a [`filter!`](crate::filter!)
/// invocation accepts; it is what [`Filter::with`] turns into [`Filter::With`].
pub struct With<T>(PhantomData<fn() -> T>);

/// Forbids the component `T`.
///
/// The counterpart of [`With`]: it is what [`Filter::without`] turns into
/// [`Filter::Without`].
pub struct Without<T>(PhantomData<fn() -> T>);

/// Accepts an entity that passes at least one of `T`'s filters.
///
/// The disjunction of a tuple of filters, which is otherwise the conjunction.
/// It is what [`Filter::any`] turns into [`Filter::Any`].
pub struct Or<T>(PhantomData<fn() -> T>);

/// Turns one of the reference forms into the slot it means.
///
/// This is the trait the [`query!`] macro expands through, so that the macro
/// can keep the reference syntax: `&T` is a required shared slot, `&mut T` a
/// required exclusive one, and `Option<&T>`/`Option<&mut T>` the optional
/// forms. It is public only because a macro expansion has to name it.
#[doc(hidden)]
pub trait IntoSlot {
    /// The slot this form means.
    const SLOT: Slot;
}

impl<T: 'static> IntoSlot for &T {
    const SLOT: Slot = Slot::read::<T>();
}

impl<T: 'static> IntoSlot for &mut T {
    const SLOT: Slot = Slot::write::<T>();
}

impl<T: 'static> IntoSlot for Option<&T> {
    const SLOT: Slot = Slot::optional_read::<T>();
}

impl<T: 'static> IntoSlot for Option<&mut T> {
    const SLOT: Slot = Slot::optional_write::<T>();
}

/// Turns one of the filter forms into the filter it means.
///
/// The counterpart of [`IntoSlot`] for the filter half of a [`query!`]
/// invocation. Public only because a macro expansion has to name it.
#[doc(hidden)]
pub trait IntoFilter {
    /// The filter this form means.
    const FILTER: Filter;
}

impl<T: 'static> IntoFilter for With<T> {
    const FILTER: Filter = Filter::with::<T>();
}

impl<T: 'static> IntoFilter for Without<T> {
    const FILTER: Filter = Filter::without::<T>();
}

impl<T: IntoFilterTuple> IntoFilter for Or<T> {
    const FILTER: Filter = Filter::any(T::FILTERS);
}

impl<A: IntoFilter, B: IntoFilter> IntoFilter for (A, B) {
    const FILTER: Filter = Filter::all(&[A::FILTER, B::FILTER]);
}

impl<A: IntoFilter, B: IntoFilter, C: IntoFilter> IntoFilter for (A, B, C) {
    const FILTER: Filter = Filter::all(&[A::FILTER, B::FILTER, C::FILTER]);
}

/// The filters a tuple of filter forms means, which is what [`Or`] takes.
#[doc(hidden)]
pub trait IntoFilterTuple {
    /// The filters, in order.
    const FILTERS: &'static [Filter];
}

macro_rules! impl_filter_tuple {
    ($($name:ident),+) => {
        impl<$($name: IntoFilter),+> IntoFilterTuple for ($($name,)+) {
            const FILTERS: &'static [Filter] = &[$($name::FILTER),+];
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

/// Build a [`Query`] from the reference syntax a typed query used to have.
///
/// `&T` and `&mut T` are required slots, `Option<&T>` and `Option<&mut T>`
/// optional ones. A semicolon starts the filter: [`With<T>`], [`Without<T>`],
/// [`Or<(...)>`], or a tuple of those for the conjunction.
///
/// The macro yields a `&'static Query`: the slots are assembled in a const
/// block, so the query costs nothing at run time and can be handed straight to
/// [`Scope::query`](crate::Scope::query) without being bound to a variable
/// first.
///
/// ````rust
/// # use unlit_ecs::{query, Filter, Query, Slot, With};
/// let query = query!(&u32, &mut f32, Option<&bool>; With<i64>);
/// assert_eq!(
///     query.slots(),
///     [
///         Slot::read::<u32>(),
///         Slot::write::<f32>(),
///         Slot::optional_read::<bool>(),
///     ],
/// );
/// assert_eq!(query.filter(), &Filter::with::<i64>());
///
/// // A constant query, resolved at compile time.
/// const NAMED: &Query = query!(&u32);
/// assert_eq!(NAMED.slots(), [Slot::read::<u32>()]);
/// ````
#[macro_export]
macro_rules! query {
    () => {
        &const { $crate::Query::new() }
    };
    (; $($filter:ty),+ $(,)?) => {
        &const { $crate::Query::new().with_filter($crate::filter!($($filter),+)) }
    };
    ($($item:ty),+ $(,)? ; $($filter:ty),+ $(,)?) => {
        &const {
            $crate::Query::from_static(
                &[$(<$item as $crate::IntoSlot>::SLOT),+],
                $crate::filter!($($filter),+),
            )
        }
    };
    ($($item:ty),+ $(,)?) => {
        &const {
            $crate::Query::from_static(
                &[$(<$item as $crate::IntoSlot>::SLOT),+],
                $crate::Filter::True,
            )
        }
    };
}

/// Build a [`Filter`] from the forms a typed query used to take.
///
/// A list is the conjunction, [`Or`] the disjunction, and one form on its own
/// is simply that form.
///
/// ````rust
/// # use unlit_ecs::{filter, Filter, Or, With, Without};
/// const CONJUNCTION: Filter =
///     Filter::all(&[Filter::with::<u32>(), Filter::without::<bool>()]);
/// assert_eq!(filter!(With<u32>, Without<bool>), CONJUNCTION);
///
/// const DISJUNCTION: Filter = Filter::any(&[Filter::with::<u32>(), Filter::with::<bool>()]);
/// assert_eq!(filter!(Or<(With<u32>, With<bool>)>), DISJUNCTION);
///
/// assert_eq!(filter!(With<u32>), Filter::with::<u32>());
/// ````
#[macro_export]
macro_rules! filter {
    () => {
        $crate::Filter::True
    };
    // One form is that form: the conjunction of one is itself, and unwrapping
    // it keeps a single filter readable when it is inspected.
    ($filter:ty $(,)?) => {
        <$filter as $crate::IntoFilter>::FILTER
    };
    ($($filter:ty),+ $(,)?) => {
        $crate::Filter::All(&[$(<$filter as $crate::IntoFilter>::FILTER),+])
    };
}
