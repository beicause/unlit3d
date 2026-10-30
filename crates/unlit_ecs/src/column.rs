//! Component storage.
//!
//! Every component value lives in its own [`UnsafeCell`], so a component can be
//! read or written through a shared `&World`. Aliasing is policed by the borrow
//! ledger instead of by the compiler: an access path claims what it is about to
//! reach before it hands out a reference, and the claim is released when it goes
//! away. See [`crate::borrow`].
//!
//! The world is therefore `!Send` and stays on the thread that created it, which
//! is where a renderer and its other thread-bound state live.
//!
//! Structural changes (spawning and despawning) still need `&mut World`, so a
//! claimed row can never be moved around while it is borrowed.

use core::any::Any;
use core::cell::UnsafeCell;

/// A type-erased component column: one component type's values in one
/// archetype.
///
/// A value that moves between archetypes travels in a `Box<dyn Any>` holding
/// the component value itself, so moving a row never needs to know the
/// component type. Reads and writes do not go through this interface; they use
/// the concrete [`Column`].
pub trait AnyColumn: 'static {
    /// Downcast support.
    fn as_any(&self) -> &dyn Any;
    /// Downcast support for a column reached exclusively.
    fn as_any_mut(&mut self) -> &mut dyn Any;
    /// Move the value at `row` out, filling the hole with the last value.
    fn swap_remove(&mut self, row: usize) -> Box<dyn Any>;
    /// Append an already erased value of this column's component type.
    fn push_boxed(&mut self, value: Box<dyn Any>);
}

/// One component type's values inside one archetype.
///
/// Each value sits in its own cell, so a query may hold the components of two
/// entities at once. A row's cell index is the row number and never changes
/// while the row exists.
pub struct Column<T: 'static> {
    cells: Vec<UnsafeCell<T>>,
}

impl<T: 'static> Column<T> {
    /// An empty column.
    pub(crate) fn new() -> Self {
        Self { cells: Vec::new() }
    }

    /// A function that builds an empty column of this component type.
    pub(crate) fn eraser() -> fn() -> Box<dyn AnyColumn> {
        fn make<T: 'static>() -> Box<dyn AnyColumn> {
            Column::<T>::new().erase()
        }
        make::<T>
    }

    /// Every cell of the column, one per row.
    ///
    /// Handing out the slice is safe; taking the value out of a cell needs the
    /// claim the caller took on the column. See [`crate::borrow`].
    #[inline]
    pub(crate) fn cells(&self) -> &[UnsafeCell<T>] {
        &self.cells
    }

    /// Every cell of the column, reached exclusively.
    #[inline]
    pub(crate) fn cells_mut(&mut self) -> &mut [UnsafeCell<T>] {
        &mut self.cells
    }

    /// Erase the column.
    pub(crate) fn erase(self) -> Box<dyn AnyColumn> {
        Box::new(self)
    }
}

impl<T: 'static> AnyColumn for Column<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn swap_remove(&mut self, row: usize) -> Box<dyn Any> {
        Box::new(self.cells.swap_remove(row).into_inner())
    }

    fn push_boxed(&mut self, value: Box<dyn Any>) {
        let value = *value
            .downcast::<T>()
            .expect("only a value of this column's component type can be pushed");
        self.cells.push(UnsafeCell::new(value));
    }
}
