//! Deferred structural changes.
//!
//! A callback that only has a shared world cannot spawn or despawn directly —
//! those need `&mut World`. Instead it queues the change on a [`Commands`]
//! queue; the driver applies the queue with [`World::apply`] once the callback
//! has returned.
//!
//! A queued spawn still needs a handle right away, so [`Commands::spawn`]
//! reserves one with [`World::reserve_entity`] and returns it. The handle is
//! usable immediately — it can be stored in a component or handed to another
//! callback — and refers to a live entity once the queue is applied.
//!
//! Commands run in the order they were queued. A command that acts on a
//! reserved entity must therefore be queued after the `Spawn` that brings it
//! to life: for example [`Commands::despawn`] panics when the entity is not
//! alive. This holds for the handles [`Commands::spawn`] itself returns; a
//! `Spawn` pushed by hand with [`Commands::push`] must be ordered the same
//! way.
//!
//! `````rust
//! # use unlit_ecs::World;
//! let mut world = World::new();
//!
//! let entity = {
//!     let commands = world.queue();
//!     commands.spawn(("entity",))
//! };
//! // Nothing has happened yet.
//! assert!(!world.contains(entity));
//! world.apply();
//! assert!(world.contains(entity));
//! `````

use crate::bundle::Bundle;
use crate::entity::Entity;
use crate::world::World;

/// A structural change that can be applied later.
pub trait Command: 'static {
    /// Apply the change to the world.
    fn apply(self: Box<Self>, world: &mut World);
}

/// Erases a command into the world's command storage.
pub trait CommandErase: Command {
    /// Erase the command.
    fn erase(self) -> Box<dyn Command>;
}

impl<C: Command> CommandErase for C {
    fn erase(self) -> Box<dyn Command> {
        Box::new(self)
    }
}

/// A queue of structural changes.
///
/// The queue lives in the world, so deferring a spawn from a callback to the
/// driver needs no channel.
pub struct Commands<'w> {
    world: &'w World,
}

impl<'w> Commands<'w> {
    pub(crate) fn new(world: &'w World) -> Self {
        Self { world }
    }

    /// Queue a spawn of `bundle` and return its handle.
    ///
    /// The handle is reserved now and refers to a live entity once the queue is
    /// applied.
    ///
    pub fn spawn<B: Bundle + 'static>(&self, bundle: B) -> Entity {
        let entity = self.world.reserve_entity();
        self.push(Spawn {
            entity,
            bundle: Some(bundle),
        });
        entity
    }

    /// Queue the despawn of `entity`.
    pub fn despawn(&self, entity: Entity) {
        self.push(Despawn { entity });
    }

    /// Queue a command.
    pub fn push<C: Command + CommandErase>(&self, command: C) {
        self.world.push_command(command.erase());
    }
}

/// Spawns a bundle on a reserved entity.
pub struct Spawn<B> {
    /// The reserved handle.
    pub entity: Entity,
    bundle: Option<B>,
}

impl<B: Bundle + 'static> Command for Spawn<B> {
    fn apply(mut self: Box<Self>, world: &mut World) {
        let bundle = self.bundle.take().expect("a command is applied only once");
        world.spawn_at(self.entity, bundle);
    }
}

/// Despawns an entity.
pub struct Despawn {
    /// The entity.
    pub entity: Entity,
}

impl Command for Despawn {
    fn apply(self: Box<Self>, world: &mut World) {
        world.despawn(self.entity);
    }
}

#[cfg(test)]
mod tests {
    use crate::World;
    use crate::tests_common::Marker;

    #[test]
    fn a_queued_spawn_only_happens_on_apply() {
        let mut world = World::new();
        let entity = {
            let commands = world.queue();
            commands.spawn((Marker(1),))
        };
        assert!(!world.contains(entity), "queued, not spawned");
        world.apply();
        assert!(world.contains(entity));
        assert_eq!(world.get::<Marker>(entity).unwrap().0, 1);
    }

    #[test]
    fn a_queued_despawn_happens_on_apply() {
        let mut world = World::new();
        let entity = world.spawn((Marker(1),));
        world.queue().despawn(entity);
        assert!(world.contains(entity));
        world.apply();
        assert!(!world.contains(entity));
    }

    #[test]
    fn commands_queued_while_applying_are_applied_too() {
        let mut world = World::new();
        world.spawn(("first",));
        let second = {
            let commands = world.queue();
            let second = commands.spawn(("second",));
            // A second batch queued from the same shared world.
            commands.spawn(("third",));
            second
        };
        world.apply();
        assert!(world.contains(second));
        assert_eq!(world.len(), 3);

        let fourth = world.queue().spawn(("fourth",));
        world.apply();
        assert!(world.contains(fourth));
        assert_eq!(world.len(), 4);
    }

    #[test]
    fn applying_an_empty_queue_does_nothing() {
        let mut world = World::new();
        world.apply();
        assert!(world.is_empty());
    }
}
