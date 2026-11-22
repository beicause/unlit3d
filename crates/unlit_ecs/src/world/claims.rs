//! The borrow ledger: who holds which component values.

use core::any::TypeId;
use core::cell::RefMut;

use crate::borrow::{BorrowLedger, Rows};
use crate::query::Mode;

use super::World;

impl World {
    /// Register a claim on one component type for one holder.
    ///
    /// Panics when a claim already held for the same type clashes with it. See
    /// [`crate::borrow`] for what clashes.
    #[track_caller]
    pub(crate) fn claim(
        &self,
        group: u64,
        type_id: TypeId,
        name: &'static str,
        mode: Mode,
        rows: Rows,
        origin: &'static core::panic::Location<'static>,
    ) {
        self.borrow_write()
            .claim(group, type_id, name, mode, rows, origin);
    }

    /// The id the next holder files its claims under.
    pub(crate) fn next_group(&self) -> u64 {
        self.borrow_write().next_group()
    }

    /// Drop every claim one holder made.
    pub(crate) fn release(&self, group: u64) {
        self.borrow_write().release(group);
    }

    fn borrow_write(&self) -> RefMut<'_, BorrowLedger> {
        self.borrow
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the borrow ledger is already borrowed"))
    }
}
