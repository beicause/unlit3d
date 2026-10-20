//! Lifecycle behaviour components: callbacks that run when an entity is
//! spawned or despawned.
//!
//! These are components, not hooks into a scheduler: the callbacks live in the
//! entity's own row and the world runs them at the structural change that
//! produced or is about to remove it. There is no change detection and no
//! deferred pass -- an [`OnDespawn`] callback runs while the entity still has
//! its components, because afterwards there is nothing left to read.
//!
//! A callback gets the scope of the entity's own scope, which is its parent, and
//! cannot change the world's structure: it queues those changes instead. Like
//! every behaviour component it is borrowed while it runs, so reaching for its
//! own lifecycle component from inside it is a borrow panic.

use crate::entity::Entity;
use crate::view::Scope;

/// The callback of a lifecycle component.
pub type LifecycleCallback = Box<dyn for<'a, 'b> FnMut(&'a Scope<'b>, Entity)>;

/// Declare one lifecycle component: its callback and its constructor.
macro_rules! lifecycle_component {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub struct $name(
            /// The callback, run at the structural change.
            pub LifecycleCallback,
        );

        impl $name {
            #[doc = concat!("Wrap `callback` as an [`", stringify!($name), "`].")]
            #[must_use]
            pub fn new(
                callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static,
            ) -> Self {
                Self(Box::new(callback))
            }
        }

        impl $crate::lifecycle::Lifecycle for $name {
            fn callback(&mut self) -> &mut LifecycleCallback {
                &mut self.0
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
    /// The callback the world runs.
    fn callback(&mut self) -> &mut LifecycleCallback;
}

mod sealed {
    /// Keeps [`Lifecycle`](super::Lifecycle) to the components the world runs.
    pub trait Sealed {}

    impl Sealed for super::OnSpawn {}
    impl Sealed for super::OnDespawn {}
}
