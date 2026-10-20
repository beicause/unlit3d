//! The behaviour-component macro shared by every callback family.
//!
//! A behaviour component is the library's replacement for a system: a
//! component that holds a list of closures and is invoked by whatever driver
//! the caller writes. Every family — the portable input callbacks and the
//! winit callbacks — has the same shape, so one macro declares them all and
//! the shape cannot drift between families.
//!
//! A component holds a list rather than a single callback: an entity may
//! carry several behaviours of one family, and they run in the order they
//! were added. The driver runs them through [`unlit_ecs::World`], which takes
//! one callback out of its component at a time, so a callback may reach for its
//! own family and what it queued is applied before the next one runs.
//!
//! The macro is invoked from other modules, so every path it writes is
//! `$crate`-absolute: a `macro_rules!` body resolves bare names in the module
//! it is *invoked* from, not the one it is defined in.

pub(crate) use unlit_ecs::{CallbackList, Entity, World};

/// A callback that may read and write the world, and receives the entity it
/// runs for together with one event.
pub type EventCallback<E> = Box<dyn FnMut(&World, Entity, &E)>;

/// Declares one behaviour component over an event type.
///
/// Every behaviour is the same shape [`unlit_ecs`]' own behaviour components
/// use: a public list of boxed callbacks, a `new` that takes a list of them,
/// and `push` that adds one more. Events are passed by reference, so the
/// world can hold one copy of an event that several behaviours read.
///
/// A `run` takes the world and the entity the behaviour sits on, so the
/// callback can tell which entity it is acting for — one closure may be
/// mounted on many.
macro_rules! behaviour {
    ($name:ident, $event:ty, $doc:expr) => {
        #[doc = $doc]
        pub struct $name {
            /// The callbacks, in the order they were added.
            pub callbacks: Vec<$crate::behaviour::EventCallback<$event>>,
        }

        impl $crate::behaviour::CallbackList for $name {
            type Callback = $crate::behaviour::EventCallback<$event>;

            fn callbacks(&self) -> &[$crate::behaviour::EventCallback<$event>] {
                &self.callbacks
            }

            fn callbacks_mut(&mut self) -> &mut Vec<$crate::behaviour::EventCallback<$event>> {
                &mut self.callbacks
            }
        }

        impl $name {
            #[doc = concat!(
                "Box `f` as one callback of an [`",
                stringify!($name),
                "`]."
            )]
            ///
            /// [`new`](Self::new) takes callbacks in this form, which is what
            /// lets a closure be written without naming its family's event
            /// type.
            pub fn callback(
                f: impl FnMut(&$crate::behaviour::World, $crate::behaviour::Entity, &$event) + 'static,
            ) -> $crate::behaviour::EventCallback<$event> {
                Box::new(f)
            }

            #[doc = concat!(
                "Wrap the callbacks `cbs` as an [`",
                stringify!($name),
                "`]."
            )]
            ///
            /// They run in the order the iterator yields them. An empty
            /// iterator declares a component that runs nothing yet.
            pub fn new(
                cbs: impl IntoIterator<Item = $crate::behaviour::EventCallback<$event>>,
            ) -> Self {
                Self {
                    callbacks: cbs.into_iter().collect(),
                }
            }

            #[doc = concat!(
                "Add `f` at run time, after the callbacks already in this [`",
                stringify!($name),
                "`]."
            )]
            pub fn push(
                &mut self,
                f: impl FnMut(&$crate::behaviour::World, $crate::behaviour::Entity, &$event) + 'static,
            ) {
                self.callbacks.push(Box::new(f));
            }

            #[doc = concat!(
                "Run every callback of this [`",
                stringify!($name),
                "`] for `entity`, in the order they were added."
            )]
            ///
            /// For a component a caller holds itself, outside the world; one
            /// the world holds is driven by its host, which applies the queue
            /// between two callbacks where this does not.
            pub fn run(
                &mut self,
                world: &$crate::behaviour::World,
                entity: $crate::behaviour::Entity,
                event: &$event,
            ) {
                for callback in &mut self.callbacks {
                    callback(world, entity, event);
                }
            }
        }
    };
}

pub(crate) use behaviour;

#[cfg(test)]
mod tests {
    use super::*;

    behaviour!(OnTick, u32, "A test family.");

    #[test]
    fn a_component_runs_every_callback_in_registration_order() {
        let world = World::new();
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let first = log.clone();
        let second = log.clone();
        let third = log.clone();
        let mut behaviour = OnTick::new([
            OnTick::callback(move |_world, _entity, tick| first.borrow_mut().push(*tick)),
            OnTick::callback(move |_world, _entity, tick| second.borrow_mut().push(*tick + 1)),
        ]);
        behaviour.push(move |_world, _entity, tick| third.borrow_mut().push(*tick + 2));
        behaviour.run(&world, Entity::from_bits(0), &10);
        assert_eq!(*log.borrow(), vec![10, 11, 12]);
    }

    #[test]
    fn a_callback_receives_the_entity_it_runs_for() {
        let world = World::new();
        let seen = std::rc::Rc::new(std::cell::Cell::new(Entity::from_bits(0)));
        let handle = seen.clone();
        let mut behaviour = OnTick::new([OnTick::callback(move |_world, entity, _tick| {
            handle.set(entity);
        })]);
        let entity = Entity::from_bits(7);
        behaviour.run(&world, entity, &0);
        assert_eq!(seen.get(), entity);
    }
}
