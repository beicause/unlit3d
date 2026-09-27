//! Component storage.
//!
//! Every component value lives in its own [`RefCell`], so a component can be
//! read or written through a shared `&World` without any `unsafe`. The world
//! itself is therefore `!Send` and stays on the thread that created it, which
//! is where a renderer and its other thread-bound state live.
//!
//! Structural changes (spawning and despawning) still need `&mut World`, so a
//! component borrow can never be held while the storage it lives in is moved
//! around.

use core::any::Any;
use core::cell::{Ref, RefCell, RefMut};

/// The shared borrow of a component value.
pub type CellRef<'a, T> = Ref<'a, T>;

/// The exclusive borrow of a component value.
pub type CellRefMut<'a, T> = RefMut<'a, T>;

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
    cells: Vec<RefCell<T>>,
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

    /// The cell at `row`.
    #[inline]
    pub(crate) fn cell(&self, row: usize) -> Option<&RefCell<T>> {
        self.cells.get(row)
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

    fn swap_remove(&mut self, row: usize) -> Box<dyn Any> {
        Box::new(self.cells.swap_remove(row).into_inner())
    }

    fn push_boxed(&mut self, value: Box<dyn Any>) {
        let value = *value
            .downcast::<T>()
            .expect("only a value of this column's component type can be pushed");
        self.cells.push(RefCell::new(value));
    }
}
