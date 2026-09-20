//! Storage modes: the two worlds differ only in the cells components live in.
//!
//! Every component value of a world lives in a single [`Cell`]. The `!Send`
//! world uses `RefCell`, so its cells and therefore the world itself are not
//! `Send` and it can only run on the thread that owns it. The `Send` world
//! uses `RwLock`, so a world can be shared across threads.
//!
//! Component access goes through the cells, which is what lets a shared
//! `&World` read and write components without any `unsafe`. Structural
//! changes (spawning, despawning, adding or removing components, changing the
//! hierarchy) still need `&mut World`, so a component borrow can never be
//! held while the storage it lives in is moved around.

use core::any::Any;
use core::cell::{Ref, RefCell, RefMut};
use core::future::Future;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::command::Command;

/// The interior-mutability cell holding one component value.
pub trait Cell<T: 'static>: 'static {
    /// Shared borrow of the value.
    type Ref<'a>: Deref<Target = T>
    where
        Self: 'a;
    /// Exclusive borrow of the value.
    type RefMut<'a>: DerefMut<Target = T>
    where
        Self: 'a;

    /// Store `value`.
    fn new(value: T) -> Self;
    /// Borrow shared, or `None` while exclusively borrowed.
    fn try_read(&self) -> Option<Self::Ref<'_>>;
    /// Borrow exclusively, or `None` while borrowed at all.
    fn try_write(&self) -> Option<Self::RefMut<'_>>;
    /// Take the value back out.
    ///
    /// Consuming the cell means no borrow can be outstanding, so this cannot
    /// fail.
    fn into_inner(self) -> T;
}

/// `RefCell`-backed cell of the `!Send` world.
pub struct LocalCell<T>(RefCell<T>);

impl<T: 'static> Cell<T> for LocalCell<T> {
    type Ref<'a>
        = Ref<'a, T>
    where
        Self: 'a;
    type RefMut<'a>
        = RefMut<'a, T>
    where
        Self: 'a;

    fn new(value: T) -> Self {
        Self(RefCell::new(value))
    }

    fn try_read(&self) -> Option<Self::Ref<'_>> {
        self.0.try_borrow().ok()
    }

    fn try_write(&self) -> Option<Self::RefMut<'_>> {
        self.0.try_borrow_mut().ok()
    }

    fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

/// `RwLock`-backed cell of the `Send` world.
///
/// The lock is tried, not waited on, so two threads that touch the same
/// component at once get a borrow conflict rather than a stall. Partition the
/// entities between threads; that is the same rule the `!Send` world follows,
/// where a re-entrant borrow is a conflict too.
pub struct SyncCell<T>(RwLock<T>);

impl<T: 'static> Cell<T> for SyncCell<T> {
    type Ref<'a>
        = RwLockReadGuard<'a, T>
    where
        Self: 'a;
    type RefMut<'a>
        = RwLockWriteGuard<'a, T>
    where
        Self: 'a;

    fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    fn try_read(&self) -> Option<Self::Ref<'_>> {
        self.0.try_read().ok()
    }

    fn try_write(&self) -> Option<Self::RefMut<'_>> {
        self.0.try_write().ok()
    }

    fn into_inner(self) -> T {
        self.0
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

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
    /// Downcast support.
    fn as_any_mut(&mut self) -> &mut dyn Any;
    /// Number of rows.
    fn len(&self) -> usize;
    /// Whether the column has no rows.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Move the value at `row` out, filling the hole with the last value.
    fn swap_remove(&mut self, row: usize) -> Box<dyn Any>;
    /// Append an already erased value of this column's component type.
    fn push_boxed(&mut self, value: Box<dyn Any>);
}

/// One component type's values inside one archetype.
pub struct Column<M: Mode, T: 'static> {
    cells: Vec<M::Cell<T>>,
    _marker: PhantomData<M>,
}

impl<M: Mode, T: 'static> Column<M, T> {
    /// An empty column.
    pub(crate) fn new() -> Self {
        Self {
            cells: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// A function that builds an empty column of this component type.
    pub(crate) fn eraser() -> fn() -> Box<M::ErasedColumn>
    where
        Column<M, T>: ColumnErase<M>,
    {
        fn make<M: Mode, T: 'static>() -> Box<M::ErasedColumn>
        where
            Column<M, T>: ColumnErase<M>,
        {
            Column::<M, T>::new().erase()
        }
        make::<M, T>
    }

    /// The cell at `row`.
    pub(crate) fn cell(&self, row: usize) -> Option<&M::Cell<T>> {
        self.cells.get(row)
    }
}

impl<M: Mode, T: 'static> AnyColumn for Column<M, T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn len(&self) -> usize {
        self.cells.len()
    }

    fn swap_remove(&mut self, row: usize) -> Box<dyn Any> {
        Box::new(M::Cell::into_inner(self.cells.swap_remove(row)))
    }

    fn push_boxed(&mut self, value: Box<dyn Any>) {
        let value = *value
            .downcast::<T>()
            .expect("only a value of this column's component type can be pushed");
        self.cells.push(M::Cell::new(value));
    }
}

/// Erases a typed column into a mode's column storage.
///
/// The `Send` implementation is only available for components that are
/// `Send + Sync`, which is what makes [`SendWorld`](crate::SendWorld)
/// `Send` and `Sync`.
pub trait ColumnErase<M: Mode> {
    /// Erase the column.
    fn erase(self) -> Box<M::ErasedColumn>;
}

impl<T: Send + Sync + 'static> ColumnErase<SendMode> for Column<SendMode, T> {
    fn erase(self) -> Box<dyn AnyColumn + Send + Sync> {
        Box::new(self)
    }
}

impl<T: 'static> ColumnErase<LocalMode> for Column<LocalMode, T> {
    fn erase(self) -> Box<dyn AnyColumn> {
        Box::new(self)
    }
}

mod sealed {
    pub trait Sealed {}
}

/// The storage choice of a world.
///
/// This trait is sealed: the only two implementations are [`LocalMode`] and
/// [`SendMode`], which is what the `World` type aliases use. A caller never
/// names it directly; the type aliases in the crate root do.
pub trait Mode: 'static + Sized + sealed::Sealed {
    /// The cell holding one component value.
    type Cell<T: 'static>: Cell<T>;
    /// A type-erased column (with the mode's thread-safety bound).
    type ErasedColumn: ?Sized + AnyColumn;
    /// A type-erased command.
    ///
    /// [`LocalMode`] leaves this unconstrained, so a `!Send` command may be
    /// queued on a `!Send` world; [`SendMode`] adds `Send + Sync` at the
    /// point where a command is boxed.
    type ErasedCommand: ?Sized + Command<Self>;
    /// A type-erased future owned by a [`Tasks`](crate::Tasks) table.
    type ErasedTask: ?Sized + Future<Output = ()>;

    /// An empty [`Children`](crate::Children) column.
    ///
    /// The hierarchy components are the ones a live entity always gains and
    /// loses, so the mode constructs their columns directly and no generic
    /// caller needs the component's erase bound.
    fn children_column() -> Box<Self::ErasedColumn>;
    /// An empty [`ChildOf`](crate::ChildOf) column.
    fn child_of_column() -> Box<Self::ErasedColumn>;
}

impl sealed::Sealed for LocalMode {}
impl sealed::Sealed for SendMode {}

/// The `!Send` storage mode, used by [`LocalWorld`](crate::LocalWorld).
pub struct LocalMode;

impl Mode for LocalMode {
    type Cell<T: 'static> = LocalCell<T>;
    type ErasedColumn = dyn AnyColumn;
    type ErasedCommand = dyn Command<LocalMode>;
    type ErasedTask = dyn Future<Output = ()>;

    fn children_column() -> Box<dyn AnyColumn> {
        Column::<LocalMode, crate::Children>::new().erase()
    }

    fn child_of_column() -> Box<dyn AnyColumn> {
        Column::<LocalMode, crate::ChildOf>::new().erase()
    }
}

/// The `Send` storage mode, used by [`SendWorld`](crate::SendWorld).
pub struct SendMode;

impl Mode for SendMode {
    type Cell<T: 'static> = SyncCell<T>;
    type ErasedColumn = dyn AnyColumn + Send + Sync;
    type ErasedCommand = dyn Command<SendMode> + Send + Sync;
    type ErasedTask = dyn Future<Output = ()> + Send;

    fn children_column() -> Box<dyn AnyColumn + Send + Sync> {
        Column::<SendMode, crate::Children>::new().erase()
    }

    fn child_of_column() -> Box<dyn AnyColumn + Send + Sync> {
        Column::<SendMode, crate::ChildOf>::new().erase()
    }
}

/// The borrow of a component value.
pub type CellRef<'a, M, T> = <<M as Mode>::Cell<T> as Cell<T>>::Ref<'a>;

/// The exclusive borrow of a component value.
pub type CellRefMut<'a, M, T> = <<M as Mode>::Cell<T> as Cell<T>>::RefMut<'a>;
