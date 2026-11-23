//! Lifecycle behaviour components: callbacks that run when an entity is
//! spawned or despawned.
//!
//! These are components, not hooks into a scheduler: the callbacks live in the
//! entity's own row and the world runs them at the structural change that
//! produced or is about to remove it. There is no change detection and no
//! deferred pass — an [`OnDespawn`] callback runs while the entity still has
//! its components, because afterwards there is nothing left to read.
//!
//! A callback gets a mutable scope over the entity's parent, so it changes the
//! world's structure directly: spawning, despawning and relinking all take
//! effect before the callback returns. Component queries go through
//! [`ScopeMut::readonly`], whose result borrows the mutable scope for as long as
//! it is held.
//!
//! The callback is moved out of the entity's row for the run, so the row carries
//! no callback while it runs and a callback that reaches for its own lifecycle
//! component reaches a row rather than one held open by its own run. It is put
//! back afterwards unless the row has a callback again by then: a callback that
//! writes a replacement keeps it, and one that writes `None` cannot be told
//! apart from one that wrote nothing, so `None` gets the callback it was
//! spawned with.

use crate::entity::Entity;
use crate::view::ScopeMut;

/// The callback of a lifecycle component.
pub type LifecycleCallback = Box<dyn for<'a, 'b> FnMut(&'a mut ScopeMut<'b>, Entity)>;

/// Declare one lifecycle component: its callback and its constructor.
macro_rules! lifecycle_component {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub struct $name(
            /// The callback, run at the structural change, or `None` while it
            /// runs and for an entity that carries no callback.
            pub Option<LifecycleCallback>,
        );

        impl $name {
            #[doc = concat!("Wrap `callback` as an [`", stringify!($name), "`].")]
            #[must_use]
            pub fn new(
                callback: impl for<'a, 'b> FnMut(&'a mut ScopeMut<'b>, Entity) + 'static,
            ) -> Self {
                Self(Some(Box::new(callback)))
            }
        }

        impl $crate::lifecycle::Lifecycle for $name {
            fn take(&mut self) -> Option<LifecycleCallback> {
                self.0.take()
            }

            fn restore(&mut self, callback: LifecycleCallback) {
                self.0.get_or_insert(callback);
            }
        }
    };
}

lifecycle_component!(
    /// The callback that runs right after the entity is spawned.
    OnSpawn
);
lifecycle_component!(
    /// The callback that runs right before the entity is despawned.
    OnDespawn
);

/// A component whose callback the world runs at a structural change.
///
/// The trait is what lets the spawn and despawn paths share one runner. It is
/// sealed: the world only knows how to run [`OnSpawn`] and [`OnDespawn`], so
/// implementing it elsewhere would not make anything run.
pub trait Lifecycle: 'static + sealed::Sealed {
    /// Take the callback out, leaving [`None`] in its place.
    fn take(&mut self) -> Option<LifecycleCallback>;

    /// Put a callback taken by [`Lifecycle::take`] back, unless the row has
    /// grown one of its own since.
    fn restore(&mut self, callback: LifecycleCallback);
}

mod sealed {
    /// Keeps [`Lifecycle`](super::Lifecycle) to the components the world runs.
    pub trait Sealed {}

    impl Sealed for super::OnSpawn {}
    impl Sealed for super::OnDespawn {}
}
