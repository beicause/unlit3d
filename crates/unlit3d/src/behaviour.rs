//! The behaviour-component macro shared by every callback family.
//!
//! A behaviour component is the library's replacement for a system: a
//! component that holds a closure and is invoked by whatever driver the caller
//! writes. Every family — the portable input callbacks and the winit callbacks
//! — has the same shape, so one macro declares them all and the shape cannot
//! drift between families.
//!
//! The macro is invoked from other modules, so every path it writes is
//! `$crate`-absolute: a `macro_rules!` body resolves bare names in the module
//! it is *invoked* from, not the one it is defined in.

pub(crate) use unlit_ecs::{Entity, World};

/// A callback that may read and write the world, and receives the entity it
/// runs for together with one event.
pub(crate) type EventCallback<E> = Box<dyn FnMut(&World, Entity, &E)>;

/// Declares one behaviour component over an event type.
///
/// Every behaviour is the same shape [`unlit_ecs`]' own behaviour components
/// use: a public boxed closure, a `new` that boxes a caller's closure, and a
/// `run` that calls it. Events are passed by reference, so the world can hold
/// one copy of an event that several behaviours read.
///
/// A `run` takes the world and the entity the behaviour sits on, so the
/// callback can tell which entity it is acting for — one closure may be
/// mounted on many.
macro_rules! behaviour {
    ($name:ident, $event:ty, $doc:expr) => {
        #[doc = $doc]
        pub struct $name(pub $crate::behaviour::EventCallback<$event>);

        impl $name {
            #[doc = concat!("Wrap `f` as an [`", stringify!($name), "`].")]
            pub fn new(
                f: impl FnMut(&$crate::behaviour::World, $crate::behaviour::Entity, &$event) + 'static,
            ) -> Self {
                Self(Box::new(f))
            }

            #[doc = concat!("Run this behaviour for `entity` with `event`.")]
            pub fn run(
                &mut self,
                world: &$crate::behaviour::World,
                entity: $crate::behaviour::Entity,
                event: &$event,
            ) {
                (self.0)(world, entity, event);
            }
        }
    };
}

pub(crate) use behaviour;
