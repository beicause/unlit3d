//! Deferred structural changes.
//!
//! A callback that only has a shared world cannot spawn or despawn directly --
//! those need `&mut World`. Instead it queues the change on a [`Commands`]
//! queue; the driver applies the queue with [`World::apply`] once the callback
//! has returned.
//!
//! A queue belongs to a scope: everything queued through it lands in that
//! scope, and every command carries the scope it was queued from so that
//! applying it later cannot be redirected by a queue that outlived its view.
//!
//! A queued spawn still needs a handle right away, so [`Commands::spawn`]
//! reserves one with [`World::reserve_entity`] and returns it. The handle is
//! usable immediately -- it can be stored in a component or handed to another
//! callback -- and refers to a live entity once the queue is applied.
//!
//! Commands run in the order they were queued. A command that acts on a
//! reserved entity must therefore be queued after the `Spawn` that brings it
//! to life: for example [`Commands::despawn`] does nothing when the entity is
//! not alive yet. This holds for the handles [`Commands::spawn`] itself
//! returns; a `Spawn` pushed by hand with [`Commands::push`] must be ordered the
//! same way.
//!
//! `````rust
//! # use unlit_ecs::World;
//! let mut world = World::new();
//!
//! let entity = {
//!     let view = world.root_view();
//!     let commands = view.queue();
//!     commands.spawn(("entity",))
//! };
//! // Nothing has happened yet.
//! assert!(!world.root_view().contains(entity));
//! world.apply();
//! assert!(world.root_view().contains(entity));
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

/// A queue of structural changes, all of them in one scope.
///
/// The queue lives in the world, so deferring a spawn from a callback to the
/// driver needs no channel.
pub struct Commands<'w> {
    world: &'w World,
    scope: Entity,
}

impl<'w> Commands<'w> {
    pub(crate) fn new(world: &'w World, scope: Entity) -> Self {
        Self { world, scope }
    }

    /// The scope every command of this queue lands in.
    #[must_use]
    pub fn scope(&self) -> Entity {
        self.scope
    }

    /// Queue a spawn of `bundle` in this scope and return its handle.
    ///
    /// The handle is reserved now and refers to a live entity once the queue is
    /// applied.
    pub fn spawn<B: Bundle + 'static>(&self, bundle: B) -> Entity {
        let entity = self.world.reserve_entity();
        self.push(Spawn {
            entity,
            scope: self.scope,
            bundle: Some(bundle),
        });
        entity
    }

    /// Queue the despawn of `entity` and its subtree.
    ///
    /// The scope is read now, while the entity is still alive, so that applying
    /// the command later cannot despawn something the queue's scope does not
    /// own.
    pub fn despawn(&self, entity: Entity) {
        let Some(scope) = self.world.scope_of(entity) else {
            return;
        };
        self.push(Despawn { entity, scope });
    }

    /// Queue a relink of `child` into `parent`.
    pub fn link(&self, child: Entity, parent: Entity) {
        self.push(Link {
            child,
            parent,
            scope: self.scope,
        });
    }

    /// Queue a detach of `child` into this queue's scope.
    pub fn unlink(&self, child: Entity) {
        self.push(Unlink {
            child,
            scope: self.scope,
        });
    }

    /// Queue a command.
    pub fn push<C: Command + CommandErase>(&self, command: C) {
        self.world.push_command(command.erase());
    }
}

/// Spawns a bundle on a reserved entity, in a scope.
pub struct Spawn<B> {
    /// The reserved handle.
    pub entity: Entity,
    /// The scope the entity lands in.
    pub scope: Entity,
    bundle: Option<B>,
}

impl<B: Bundle + 'static> Command for Spawn<B> {
    fn apply(mut self: Box<Self>, world: &mut World) {
        let bundle = self.bundle.take().expect("a command is applied only once");
        world.spawn_at_in(self.entity, self.scope, bundle);
    }
}

/// Despawns an entity and its subtree.
pub struct Despawn {
    /// The entity.
    pub entity: Entity,
    /// The scope the entity lived in when the command was queued.
    pub scope: Entity,
}

impl Command for Despawn {
    fn apply(self: Box<Self>, world: &mut World) {
        // The scope is what the queue saw at queue time; an entity that has
        // moved out of it since is not this command's to remove.
        if world.scope_of(self.entity) == Some(self.scope) {
            world.despawn_in(self.entity);
        }
    }
}

/// Relinks an entity into another parent.
pub struct Link {
    /// The entity to move.
    pub child: Entity,
    /// Its new parent.
    pub parent: Entity,
    /// The scope the command was queued from.
    pub scope: Entity,
}

impl Command for Link {
    fn apply(self: Box<Self>, world: &mut World) {
        if world.owns(self.scope, self.child) && world.owns(self.scope, self.parent) {
            world.link_in(self.child, self.parent);
        }
    }
}

/// Detaches an entity into a scope.
pub struct Unlink {
    /// The entity to move.
    pub child: Entity,
    /// The scope it moves into.
    pub scope: Entity,
}

impl Command for Unlink {
    fn apply(self: Box<Self>, world: &mut World) {
        if world.owns(self.scope, self.child) {
            world.unlink_in(self.child, self.scope);
        }
    }
}
