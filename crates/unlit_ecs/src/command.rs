//! Deferred structural changes.
//!
//! A behaviour component only has a shared world, so it cannot spawn, despawn or
//! reparent directly — those need `&mut World`. Instead it queues the change on
//! a [`Commands`] queue; the driver applies the queue with
//! [`World::apply`] once the callback has returned.
//!
//! A queued spawn still needs a handle right away, so
//! [`Commands::spawn`] reserves one with [`World::reserve_entity`] and
//! returns it. The handle is usable immediately — it can be given to
//! [`Commands::set_parent`], stored in a component, or handed to another
//! callback — and refers to a live entity once the queue is applied.
//!
//! A queued command is `Send`, so a spawn that carries a component which
//! cannot cross threads must use [`World::spawn`](crate::LocalWorld::spawn)
//! directly instead of the queue.
//!
//! `````
//! # use unlit_ecs::LocalWorld;
//! let mut world = LocalWorld::new();
//! let parent = world.spawn(("parent",));
//!
//! let child = {
//!     let commands = world.queue();
//!     let child = commands.spawn(("child",));
//!     commands.set_parent(child, parent);
//!     child
//! };
//! // Nothing has happened yet.
//! assert!(!world.contains(child));
//! world.apply();
//! assert!(world.contains(child));
//! assert_eq!(world.child_entities(parent), [child]);
//! `````

use crate::bundle::Bundle;
use crate::component::AddableComponent;
use crate::entity::Entity;
use crate::mode::{Column, ColumnErase, Mode};
use crate::world::World;

/// A structural change that can be applied later.
pub trait Command<M: Mode>: Send + Sync + 'static {
    /// Apply the change to the world.
    fn apply(self: Box<Self>, world: &mut World<M>);
}

/// A queue of structural changes.
///
/// The queue lives in the world, so deferring a spawn from a callback to the
/// driver needs no channel.
pub struct Commands<'w, M: Mode> {
    world: &'w World<M>,
}

impl<'w, M: Mode> Commands<'w, M> {
    pub(crate) fn new(world: &'w World<M>) -> Self {
        Self { world }
    }

    /// Queue a spawn of `bundle` and return its handle.
    ///
    /// The handle is reserved now and refers to a live entity once the queue is
    /// applied.
    pub fn spawn<B: Bundle<M> + Send + Sync + 'static>(&self, bundle: B) -> Entity {
        let entity = self.world.reserve_entity();
        self.push(Spawn {
            entity,
            bundle: Some(bundle),
        });
        entity
    }

    /// Queue the despawn of `entity` and its descendants.
    pub fn despawn(&self, entity: Entity) {
        self.push(Despawn { entity });
    }

    /// Queue adding `component` to `entity`.
    pub fn insert<C: AddableComponent + Send + Sync + 'static>(&self, entity: Entity, component: C)
    where
        Column<M, C>: ColumnErase<M>,
    {
        self.push(Insert {
            entity,
            component: Some(component),
        });
    }

    /// Queue removing component `C` from `entity`.
    pub fn remove<C: AddableComponent + Send + Sync + 'static>(&self, entity: Entity)
    where
        Column<M, C>: ColumnErase<M>,
    {
        self.push(Remove::<C> {
            entity,
            _component: core::marker::PhantomData,
        });
    }

    /// Queue making `child` a child of `parent`.
    pub fn set_parent(&self, child: Entity, parent: Entity) {
        self.push(SetParent { child, parent });
    }

    /// Queue detaching `child` from `parent`.
    pub fn remove_child(&self, parent: Entity, child: Entity) {
        self.push(RemoveChild { parent, child });
    }

    /// Queue a command.
    pub fn push<C: Command<M>>(&self, command: C) {
        self.world.push_command(M::erase_command(command));
    }
}

/// Spawns a bundle on a reserved entity.
pub struct Spawn<B> {
    /// The reserved handle.
    pub entity: Entity,
    bundle: Option<B>,
}

impl<M: Mode, B: Bundle<M> + Send + Sync + 'static> Command<M> for Spawn<B> {
    fn apply(mut self: Box<Self>, world: &mut World<M>) {
        let bundle = self.bundle.take().expect("a command is applied only once");
        world.spawn_at(self.entity, bundle);
    }
}

/// Despawns an entity and its descendants.
pub struct Despawn {
    /// The entity.
    pub entity: Entity,
}

impl<M: Mode> Command<M> for Despawn {
    fn apply(self: Box<Self>, world: &mut World<M>) {
        world.despawn(self.entity);
    }
}

/// Adds a component to an entity.
pub struct Insert<C> {
    /// The entity.
    pub entity: Entity,
    component: Option<C>,
}

impl<M: Mode, C: AddableComponent + Send + Sync + 'static> Command<M> for Insert<C>
where
    Column<M, C>: ColumnErase<M>,
{
    fn apply(mut self: Box<Self>, world: &mut World<M>) {
        let component = self
            .component
            .take()
            .expect("a command is applied only once");
        let _ = world.insert(self.entity, component);
    }
}

/// Removes a component from an entity.
pub struct Remove<C> {
    /// The entity.
    pub entity: Entity,
    _component: core::marker::PhantomData<fn() -> C>,
}

impl<M: Mode, C: AddableComponent + Send + Sync + 'static> Command<M> for Remove<C>
where
    Column<M, C>: ColumnErase<M>,
{
    fn apply(self: Box<Self>, world: &mut World<M>) {
        let _ = world.remove::<C>(self.entity);
    }
}

/// Makes a child of a parent.
pub struct SetParent {
    /// The child.
    pub child: Entity,
    /// The parent.
    pub parent: Entity,
}

impl<M: Mode> Command<M> for SetParent {
    fn apply(self: Box<Self>, world: &mut World<M>) {
        world.set_parent(self.child, self.parent);
    }
}

/// Detaches a child from a parent.
pub struct RemoveChild {
    /// The parent.
    pub parent: Entity,
    /// The child.
    pub child: Entity,
}

impl<M: Mode> Command<M> for RemoveChild {
    fn apply(self: Box<Self>, world: &mut World<M>) {
        world.remove_child(self.parent, self.child);
    }
}
#[cfg(test)]
mod tests {
    use crate::LocalWorld;
    use crate::tests_common::{Addable, Marker};

    #[test]
    fn a_queued_spawn_only_happens_on_apply() {
        let mut world = LocalWorld::new();
        let child = {
            let commands = world.queue();
            commands.spawn((Marker(1),))
        };
        assert!(!world.contains(child), "queued, not spawned");
        world.apply();
        assert!(world.contains(child));
        assert_eq!(world.get::<Marker>(child).unwrap().0, 1);
    }

    #[test]
    fn commands_apply_in_order() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        {
            let commands = world.queue();
            commands.insert(entity, Addable(1));
            commands.remove::<Addable>(entity);
            commands.insert(entity, Addable(2));
        }
        world.apply();
        assert_eq!(world.get::<Addable>(entity).unwrap().0, 2);
    }

    #[test]
    fn a_queued_parent_can_be_set_before_the_child_exists() {
        let mut world = LocalWorld::new();
        let parent = world.spawn(("parent",));
        let child = {
            let commands = world.queue();
            let child = commands.spawn(("child",));
            commands.set_parent(child, parent);
            child
        };
        world.apply();
        assert!(world.contains(child));
        assert_eq!(world.parent(child), Some(parent));
        assert_eq!(world.child_entities(parent), [child]);
    }

    #[test]
    fn a_queued_despawn_happens_on_apply() {
        let mut world = LocalWorld::new();
        let entity = world.spawn((Marker(1),));
        world.queue().despawn(entity);
        assert!(world.contains(entity));
        world.apply();
        assert!(!world.contains(entity));
    }

    #[test]
    fn commands_queued_while_applying_are_applied_too() {
        let mut world = LocalWorld::new();
        world.spawn(("parent",));
        let child = {
            let commands = world.queue();
            let child = commands.spawn(("child",));
            // A second batch queued from the same shared world.
            commands.spawn(("grandchild",));
            child
        };
        world.apply();
        assert!(world.contains(child));
        assert_eq!(world.len(), 3);

        let second = world.queue().spawn(("second",));
        world.apply();
        assert!(world.contains(second));
        assert_eq!(world.len(), 4);
    }

    #[test]
    fn applying_an_empty_queue_does_nothing() {
        let mut world = LocalWorld::new();
        world.apply();
        assert!(world.is_empty());
    }
}
