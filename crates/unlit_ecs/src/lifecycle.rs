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
//! cannot change the world's structure: it queues those changes instead. It may
//! read and write its own lifecycle component; a callback it adds there runs
//! after the ones that were already registered.

use crate::entity::Entity;
use crate::view::Scope;

/// One callback of a lifecycle component.
pub type LifecycleCallback = Box<dyn for<'a, 'b> FnMut(&'a Scope<'b>, Entity)>;

/// Declare one lifecycle component: a list of callbacks with its builder.
macro_rules! lifecycle_component {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Default)]
        pub struct $name(
            /// The callbacks, in the order they run.
            pub Vec<LifecycleCallback>,
        );

        impl $name {
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
                self.push(callback);
                self
            }

            /// Append a callback at run time.
            pub fn push(
                &mut self,
                callback: impl for<'a, 'b> FnMut(&'a Scope<'b>, Entity) + 'static,
            ) {
                self.0.push(Box::new(callback));
            }
        }

        impl $crate::lifecycle::Lifecycle for $name {
            fn callbacks(&mut self) -> &mut Vec<LifecycleCallback> {
                &mut self.0
            }
        }
    };
}

lifecycle_component!(
    /// The callbacks that run, in order, right after the entity is spawned.
    OnSpawn
);
lifecycle_component!(
    /// The callbacks that run, in order, right before the entity is despawned.
    OnDespawn
);

/// A component whose callbacks the world runs at a structural change.
///
/// The trait is what lets the spawn and despawn paths share one runner. It is
/// sealed: the world only knows how to run [`OnSpawn`] and [`OnDespawn`], so
/// implementing it elsewhere would not make anything run.
pub trait Lifecycle: 'static + sealed::Sealed {
    /// The callbacks, in the order they run.
    ///
    /// A list rather than a slice so that the runner can move the callbacks out
    /// of the component, run them without holding a borrow of it, and put back
    /// whatever they registered.
    fn callbacks(&mut self) -> &mut Vec<LifecycleCallback>;
}

mod sealed {
    /// Keeps [`Lifecycle`](super::Lifecycle) to the components the world runs.
    pub trait Sealed {}

    impl Sealed for super::OnSpawn {}
    impl Sealed for super::OnDespawn {}
}
