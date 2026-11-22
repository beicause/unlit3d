//! The queue of structural changes made through a shared reference.
//!
//! A caller holding only `&World` cannot spawn or despawn, so it queues a
//! [`Command`] instead; [`World::apply`] drains the queue in order under
//! `&mut World`. The queue lives in a cell for that reason.

use core::cell::RefMut;
use std::collections::VecDeque;

use crate::command::Command;
use crate::error::EntityAccessError;

use super::World;

/// The exclusive borrow of a world's command queue.
pub(crate) type CommandsGuard<'w> = RefMut<'w, VecDeque<Box<dyn Command>>>;

impl World {
    /// Apply every queued command, in order.
    ///
    /// Commands queued while applying are applied as well, after the ones
    /// already in the queue. A command that fails does not stop the ones after
    /// it: the queue is drained either way, and the caller is answered with the
    /// first error.
    pub fn apply(&mut self) -> Result<(), EntityAccessError> {
        let mut first = Ok(());
        loop {
            let Some(command) = self.commands_write().pop_front() else {
                return first;
            };
            let result = command.apply(self);
            if first.is_ok() {
                first = result;
            }
        }
    }

    pub(crate) fn commands_write(&self) -> CommandsGuard<'_> {
        self.commands
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("the command queue is already borrowed"))
    }

    /// Append a command to the deferred queue.
    pub(crate) fn push_command(&self, command: Box<dyn Command>) {
        self.commands_write().push_back(command);
    }
}
