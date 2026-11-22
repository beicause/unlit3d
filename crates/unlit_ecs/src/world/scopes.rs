//! The entry points that open a scope by handle.
//!
//! Every other way into the world's contents goes through a [`Scope`] or a
//! [`ScopeMut`]; these are the four plus their fallible forms.

use crate::entity::Entity;
use crate::error::EntityAccessError;
use crate::view::{Scope, ScopeMut};

use super::World;

impl World {
    /// The scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope(&self) -> Scope<'_> {
        Scope::new(self, self.root)
    }

    /// The scope of `scope`.
    ///
    /// # Panics
    ///
    /// When `scope` names no live entity, the message is the reason it was not:
    /// [`World::try_scope`] answers the same question as a `Result`.
    #[track_caller]
    pub fn scope(&self, scope: Entity) -> Scope<'_> {
        self.try_scope(scope)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// The fallible form of [`World::scope`].
    ///
    /// Answers what that does, as a `Result` instead of a panic.
    pub fn try_scope(&self, scope: Entity) -> Result<Scope<'_>, EntityAccessError> {
        self.tree.parent(scope)?;
        Ok(Scope::new(self, scope))
    }

    /// The mutable scope of the root: everything in the world.
    #[must_use]
    pub fn root_scope_mut(&mut self) -> ScopeMut<'_> {
        ScopeMut::new(self, self.root)
    }

    /// The mutable scope of `scope`.
    ///
    /// # Panics
    ///
    /// When `scope` names no live entity, the message is the reason it was not:
    /// [`World::try_scope_mut`] answers the same question as a `Result`.
    #[track_caller]
    pub fn scope_mut(&mut self, scope: Entity) -> ScopeMut<'_> {
        self.try_scope_mut(scope)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// The fallible form of [`World::scope_mut`].
    ///
    /// Answers what that does, as a `Result` instead of a panic.
    pub fn try_scope_mut(&mut self, scope: Entity) -> Result<ScopeMut<'_>, EntityAccessError> {
        self.tree.parent(scope)?;
        Ok(ScopeMut::new(self, scope))
    }
}
