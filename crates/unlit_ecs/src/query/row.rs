//! One row of a query: the components of a single entity.

use core::any::TypeId;
use core::cell::RefCell;

use crate::archetype::Archetype;
use crate::column::{CellRef, CellRefMut, Column};
use crate::entity::Entity;
use crate::query::{Access, Query};

/// Panics when a matched archetype unexpectedly lacks a component. Only a
/// malformed slot list can reach this.
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

/// The components of one entity a query matched.
///
/// A row is a handle, not a borrow: it borrows nothing until a component is
/// asked for, and the borrow it hands out lives as long as the row's own
/// lifetime. Two rows of one iterator may therefore be alive at once.
///
/// ````rust
/// # use unlit_ecs::{bundle, query, World};
/// let mut world = World::new();
/// world.root_scope_mut().spawn(bundle!(1u32, 2.0f32));
/// for (entity, row) in world.root_scope().query(query!(&u32)) {
///     assert_eq!(*row.read::<u32>(), 1);
///     assert!(row.has::<f32>());
///     assert_eq!(row.entity(), entity);
/// }
/// ````
#[derive(Clone, Copy)]
pub struct Row<'a> {
    archetype: &'a Archetype,
    query: &'a Query,
    row: usize,
}

impl core::fmt::Debug for Row<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Row")
            .field("entity", &self.entity())
            .field("row", &self.row)
            .finish_non_exhaustive()
    }
}

impl<'a> Row<'a> {
    pub(crate) fn new(archetype: &'a Archetype, query: &'a Query, row: usize) -> Self {
        Self {
            archetype,
            query,
            row,
        }
    }

    /// The row at `row` of `archetype`, read through `query`.
    ///
    /// Iterating a query is the usual way to get rows. A caller that walks the
    /// archetypes itself instead, so that it can resolve per-archetype state
    /// once rather than once per entity, builds the rows it visits this way.
    ///
    /// # Panics
    ///
    /// When `query` does not match `archetype`, or `row` is past the
    /// archetype's last row. The first is what would otherwise turn a
    /// mismatched query into a panic on the first component read.
    #[must_use]
    pub fn at(archetype: &'a Archetype, query: &'a Query, row: usize) -> Self {
        assert!(
            query.matches_archetype(archetype),
            "the query does not match this archetype",
        );
        assert!(
            row < archetype.len(),
            "row {row} is past the archetype's {} rows",
            archetype.len(),
        );
        Self::new(archetype, query, row)
    }

    /// The entity this row belongs to.
    #[must_use]
    pub fn entity(&self) -> Entity {
        self.archetype.entities()[self.row]
    }

    /// The component of this row, shared.
    ///
    /// # Panics
    ///
    /// When the query does not name `T`, names it optional, names it
    /// exclusive, or the cell is already borrowed exclusively.
    #[must_use]
    pub fn read<T: 'static>(&self) -> CellRef<'a, T> {
        self.expect_slot::<T>(Access::Read, true);
        self.cell::<T>()
            .unwrap_or_else(|| missing::<T>())
            .try_borrow()
            .unwrap_or_else(|_| borrowed::<T>("read"))
    }

    /// The component of this row, exclusive.
    ///
    /// # Panics
    ///
    /// When the query does not name `T`, names it optional, names it shared,
    /// or the cell is already borrowed.
    #[must_use]
    pub fn write<T: 'static>(&self) -> CellRefMut<'a, T> {
        self.expect_slot::<T>(Access::Write, true);
        self.cell::<T>()
            .unwrap_or_else(|| missing::<T>())
            .try_borrow_mut()
            .unwrap_or_else(|_| borrowed::<T>("write"))
    }

    /// The component of this row, shared, or `None` when the row lacks it.
    ///
    /// # Panics
    ///
    /// When the query does not name `T`, or names it exclusive. A cell that is
    /// already borrowed is a conflict rather than a missing component, so it
    /// panics as [`Row::read`] does.
    #[must_use]
    pub fn try_read<T: 'static>(&self) -> Option<CellRef<'a, T>> {
        self.expect_slot::<T>(Access::Read, false);
        let cell = self.cell::<T>()?;
        Some(cell.try_borrow().unwrap_or_else(|_| borrowed::<T>("read")))
    }

    /// The component of this row, exclusive, or `None` when the row lacks it.
    ///
    /// # Panics
    ///
    /// When the query does not name `T`, or names it shared. A cell that is
    /// already borrowed panics as [`Row::write`] does.
    #[must_use]
    pub fn try_write<T: 'static>(&self) -> Option<CellRefMut<'a, T>> {
        self.expect_slot::<T>(Access::Write, false);
        let cell = self.cell::<T>()?;
        Some(
            cell.try_borrow_mut()
                .unwrap_or_else(|_| borrowed::<T>("write")),
        )
    }

    /// Whether this row carries `T`, whatever the query asked for.
    #[must_use]
    pub fn has<T: 'static>(&self) -> bool {
        self.archetype.column_index(TypeId::of::<T>()).is_some()
    }

    /// The cell of this row's component type, when the row has one.
    fn cell<T: 'static>(&self) -> Option<&'a RefCell<T>> {
        self.archetype
            .column::<T>()
            .and_then(|column: &'a Column<T>| column.cell(self.row))
    }

    /// Panics unless the query names `T` the way the caller asks for it.
    ///
    /// The slot list is what replaced the type-level shape of a query, so the
    /// mistakes it used to catch at compile time are reported here instead.
    fn expect_slot<T: 'static>(&self, access: Access, required: bool) {
        let Some(slot) = self.query.slot(TypeId::of::<T>()) else {
            panic!(
                "component `{}` is not part of this query",
                core::any::type_name::<T>(),
            );
        };
        assert_eq!(
            slot.access(),
            access,
            "component `{}` is queried for {:?}",
            core::any::type_name::<T>(),
            slot.access(),
        );
        assert_eq!(
            slot.is_required(),
            required,
            "component `{}` is {} in this query",
            core::any::type_name::<T>(),
            if slot.is_required() {
                "required"
            } else {
                "optional"
            },
        );
    }
}
