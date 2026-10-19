//! Lifecycle behaviour components: callbacks that run when an entity is
//! spawned or despawned.
//!
//! These are components, not hooks into a scheduler: the callbacks live in the
//! entity's own row and the world runs them at the structural change that
//! produced or is about to remove it. There is no change detection and no
//! deferred pass -- an [`OnDespawn`] callback runs while the entity still has
//! its components, because afterwards there is nothing left to read.
//!
//! Both components hold a list: an entity may carry several callbacks, and they
//! run in the order they were registered.
//!
//! A callback gets the scope of the entity's own scope, which is its parent, and
//! cannot change the world's structure: it queues those changes instead.

use crate::entity::Entity;
use crate::view::Scope;

/// One callback of a lifecycle component.
pub type LifecycleCallback = Box<dyn for<'a, 'b> FnMut(&'a Scope<'b>, Entity)>;

/// The callbacks that run, in order, right after the entity is spawned.
#[derive(Default)]
pub struct OnSpawn(
    /// The callbacks, in the order they run.
    pub Vec<LifecycleCallback>,
);

/// The callbacks that run, in order, right before the entity is despawned.
#[derive(Default)]
pub struct OnDespawn(
    /// The callbacks, in the order they run.
    pub Vec<LifecycleCallback>,
);

impl OnSpawn {
    /// No callbacks yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a callback, which runs after every callback added before it.
    #[must_use]
    pub fn with(
        mut self,
        callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static,
    ) -> Self {
        self.0.push(Box::new(callback));
        self
    }

    /// Append a callback at run time.
    pub fn push(&mut self, callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static) {
        self.0.push(Box::new(callback));
    }
}

impl OnDespawn {
    /// No callbacks yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a callback, which runs after every callback added before it.
    #[must_use]
    pub fn with(
        mut self,
        callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static,
    ) -> Self {
        self.0.push(Box::new(callback));
        self
    }

    /// Append a callback at run time.
    pub fn push(&mut self, callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static) {
        self.0.push(Box::new(callback));
    }
}

/// A component whose callbacks the world runs at a structural change.
///
/// The trait is what lets the spawn and despawn paths share one runner; it is
/// implemented for [`OnSpawn`] and [`OnDespawn`] and is not meant to be
/// implemented elsewhere.
pub trait Lifecycle: 'static {
    /// The callbacks, in the order they run.
    fn callbacks(&mut self) -> &mut [LifecycleCallback];
}

impl Lifecycle for OnSpawn {
    fn callbacks(&mut self) -> &mut [LifecycleCallback] {
        &mut self.0
    }
}

impl Lifecycle for OnDespawn {
    fn callbacks(&mut self) -> &mut [LifecycleCallback] {
        &mut self.0
    }
}
