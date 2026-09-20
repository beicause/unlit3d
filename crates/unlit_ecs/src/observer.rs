//! Events and observers.
//!
//! Observers are attached to an entity, and [`World::trigger`] runs the
//! observers of one entity with an event value. An entity may have several
//! observers and several event types; observers of other event types are
//! skipped.
//!
//! Which entities an event reaches is the caller's business. A caller that
//! wants an event to reach an entity's ancestors walks
//! [`World::ancestors`](crate::LocalWorld::ancestors) and triggers each; a
//! caller that wants a whole subtree walks
//! [`World::descendants`](crate::LocalWorld::descendants). No direction is
//! imposed here.
//!
//! `````
//! # use unlit_ecs::{Ctx, LocalWorld};
//! #[derive(Debug, PartialEq)]
//! struct Damaged(u32);
//!
//! let mut world = LocalWorld::new();
//! let root = world.spawn(("root",));
//! let child = world.spawn(("child",));
//! world.set_parent(child, root);
//!
//! let seen = std::rc::Rc::new(std::cell::Cell::new(0));
//! let observer = seen.clone();
//! world.observe::<Damaged, _>(root, move |event, _ctx: &mut Ctx<'_, _>| {
//!     observer.set(observer.get() + event.0);
//! });
//! world.trigger(root, &Damaged(3));
//! assert_eq!(seen.get(), 3);
//!
//! // `trigger` reaches exactly the entity it names.
//! world.trigger(child, &Damaged(3));
//! assert_eq!(seen.get(), 3);
//! `````
//!
//! The registration form above is the simple one; the observer receives the
//! event and the scope of the entity it runs on.

use core::any::{Any, TypeId};

use crate::ctx::Ctx;
use crate::entity::Entity;
use crate::mode::{LocalMode, Mode, SendMode};
use crate::world::World;

/// A type-erased event observer.
pub trait Observer<M: Mode>: 'static {
    /// Run the observer on `entity` with `event`.
    fn call(&mut self, world: &World<M>, entity: Entity, event: &dyn Any);

    /// The event type the observer accepts.
    fn event_type(&self) -> TypeId;
}

/// Wraps a closure as an observer for event type `E`.
pub struct FnObserver<E, F> {
    f: F,
    _event: core::marker::PhantomData<fn(&E)>,
}

impl<E: 'static, F> FnObserver<E, F> {
    /// Wrap `f`.
    pub fn new(f: F) -> Self {
        Self {
            f,
            _event: core::marker::PhantomData,
        }
    }
}

impl<M: Mode, E: 'static, F> Observer<M> for FnObserver<E, F>
where
    F: 'static + FnMut(&E, &mut Ctx<'_, M>),
{
    fn call(&mut self, world: &World<M>, entity: Entity, event: &dyn Any) {
        if let Some(event) = event.downcast_ref::<E>() {
            let mut ctx = Ctx::new(world, entity);
            (self.f)(event, &mut ctx);
        }
    }

    fn event_type(&self) -> TypeId {
        TypeId::of::<E>()
    }
}

/// Erases an observer into a mode's observer storage.
pub trait ObserverErase<M: Mode>: Observer<M> {
    /// Erase the observer.
    fn erase(self) -> Box<M::ErasedObserver>;
}

impl<E: 'static, F: 'static + FnMut(&E, &mut Ctx<'_, LocalMode>)> ObserverErase<LocalMode>
    for FnObserver<E, F>
{
    fn erase(self) -> Box<dyn Observer<LocalMode>> {
        Box::new(self)
    }
}

impl<E: 'static, F: 'static + FnMut(&E, &mut Ctx<'_, SendMode>) + Send + Sync>
    ObserverErase<SendMode> for FnObserver<E, F>
{
    fn erase(self) -> Box<dyn Observer<SendMode> + Send + Sync> {
        Box::new(self)
    }
}

impl<M: Mode> World<M> {
    /// Register `observer` on `entity` for events of type `E`.
    pub fn observe<E, F>(&self, entity: Entity, observer: F)
    where
        E: 'static,
        F: FnMut(&E, &mut Ctx<'_, M>) + 'static,
        FnObserver<E, F>: ObserverErase<M>,
    {
        self.expect_alive(entity);
        self.observers_write()
            .entry(entity)
            .or_default()
            .push(FnObserver::new(observer).erase());
    }

    /// Register an already erased observer.
    pub fn observe_erased(&self, entity: Entity, observer: Box<M::ErasedObserver>) {
        self.expect_alive(entity);
        self.observers_write()
            .entry(entity)
            .or_default()
            .push(observer);
    }

    /// Remove every observer from `entity`.
    pub fn unobserve_all(&self, entity: Entity) {
        self.observers_write().remove(&entity);
    }

    /// Run `entity`'s observers for `event`.
    ///
    /// Observers of other event types are skipped. Each observer runs in
    /// registration order, and an observer may trigger another event or
    /// register another observer.
    ///
    /// This reaches exactly the entity it names; the module docs show how a
    /// caller walks the hierarchy when it wants an event to travel.
    pub fn trigger<E: Any>(&self, entity: Entity, event: &E) {
        // Take the handler list out before running it, so an observer may
        // trigger another event or register an observer without tripping the
        // table's own borrow.
        let handlers = self.observers_write().remove(&entity);
        if let Some(mut handlers) = handlers {
            for handler in handlers.iter_mut() {
                handler.call(self, entity, event);
            }
            self.observers_write()
                .entry(entity)
                .or_default()
                .append(&mut handlers);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Ctx, LocalWorld};

    #[derive(Debug, PartialEq)]
    struct Damaged(u32);
    #[derive(Debug, PartialEq)]
    struct Healed(u32);

    #[test]
    fn triggering_up_the_ancestors_reaches_each_of_them() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let middle = world.spawn(("middle",));
        let leaf = world.spawn(("leaf",));
        world.set_parent(middle, root);
        world.set_parent(leaf, middle);

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for (entity, tag) in [(leaf, "leaf"), (middle, "middle"), (root, "root")] {
            let seen = seen.clone();
            world.observe::<Damaged, _>(entity, move |event, _ctx: &mut Ctx<'_, _>| {
                seen.borrow_mut().push((tag, event.0));
            });
        }
        // The caller walks the ancestors; the library does not do it for them.
        for entity in core::iter::once(leaf).chain(world.ancestors(leaf)) {
            world.trigger(entity, &Damaged(5));
        }
        assert_eq!(
            *seen.borrow(),
            [("leaf", 5), ("middle", 5), ("root", 5)],
            "the entity and then each ancestor, in order"
        );
    }

    #[test]
    fn observers_of_other_event_types_are_skipped() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let damaged = std::rc::Rc::new(std::cell::Cell::new(0));
        let healed = std::rc::Rc::new(std::cell::Cell::new(0));
        let damaged_observer = damaged.clone();
        let healed_observer = healed.clone();
        world.observe::<Damaged, _>(root, move |_, _ctx: &mut Ctx<'_, _>| {
            damaged_observer.set(damaged_observer.get() + 1)
        });
        world.observe::<Healed, _>(root, move |_, _ctx: &mut Ctx<'_, _>| {
            healed_observer.set(healed_observer.get() + 1)
        });

        world.trigger(root, &Damaged(1));
        assert_eq!((damaged.get(), healed.get()), (1, 0));

        world.trigger(root, &Healed(1));
        assert_eq!((damaged.get(), healed.get()), (1, 1));
    }

    #[test]
    fn unobserve_all_removes_every_observer_of_an_entity() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let observer = count.clone();
        world.observe::<Damaged, _>(root, move |_, _ctx: &mut Ctx<'_, _>| {
            observer.set(observer.get() + 1)
        });
        world.trigger(root, &Damaged(1));
        assert_eq!(count.get(), 1);
        world.unobserve_all(root);
        world.trigger(root, &Damaged(1));
        assert_eq!(count.get(), 1, "the observer was removed");
    }

    #[test]
    fn an_observer_may_trigger_another_event() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        world.set_parent(child, root);

        let healed = std::rc::Rc::new(std::cell::Cell::new(0));
        let healed_observer = healed.clone();
        world.observe::<Healed, _>(root, move |_, _ctx: &mut Ctx<'_, _>| {
            healed_observer.set(healed_observer.get() + 1)
        });
        world.observe::<Damaged, _>(child, move |_, ctx: &mut Ctx<'_, _>| {
            // Re-entrant triggering from inside an observer must not deadlock or
            // trip the observer table's borrow.
            ctx.world().trigger(root, &Healed(1));
        });
        world.trigger(child, &Damaged(1));
        assert_eq!(healed.get(), 1);
    }

    #[test]
    fn trigger_only_runs_the_named_entity() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        world.set_parent(child, root);

        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for (entity, tag) in [(root, "root"), (child, "child")] {
            let seen = seen.clone();
            world.observe::<Damaged, _>(entity, move |_event, _ctx: &mut Ctx<'_, _>| {
                seen.borrow_mut().push(tag);
            });
        }
        // No direction is assumed: the child's event reaches only the child.
        world.trigger(child, &Damaged(1));
        assert_eq!(*seen.borrow(), ["child"]);
    }

    #[test]
    fn trigger_is_not_tied_to_the_hierarchy() {
        let mut world = LocalWorld::new();
        // Two unrelated roots: a caller may trigger in any order it likes.
        let first = world.spawn(("first",));
        let second = world.spawn(("second",));
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let first_seen = seen.clone();
        let second_seen = seen.clone();
        world.observe::<Damaged, _>(first, move |_, _ctx: &mut Ctx<'_, _>| {
            first_seen.borrow_mut().push("first")
        });
        world.observe::<Damaged, _>(second, move |_, _ctx: &mut Ctx<'_, _>| {
            second_seen.borrow_mut().push("second")
        });
        world.trigger(second, &Damaged(1));
        world.trigger(first, &Damaged(1));
        assert_eq!(*seen.borrow(), ["second", "first"]);
    }

    #[test]
    fn every_observer_of_an_entity_runs_in_registration_order() {
        let mut world = LocalWorld::new();
        let entity = world.spawn(("entity",));
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for tag in ["first", "second", "third"] {
            let order = order.clone();
            world.observe::<Damaged, _>(entity, move |_, _ctx: &mut Ctx<'_, _>| {
                order.borrow_mut().push(tag)
            });
        }
        world.trigger(entity, &Damaged(1));
        assert_eq!(*order.borrow(), ["first", "second", "third"]);
    }

    #[test]
    fn an_event_without_observers_is_harmless() {
        let mut world = LocalWorld::new();
        let root = world.spawn(("root",));
        let child = world.spawn(("child",));
        world.set_parent(child, root);
        world.trigger(child, &Damaged(1));
    }
}
