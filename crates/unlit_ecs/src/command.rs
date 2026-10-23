//! Deferred structural changes.
//!
//! A callback that only has a shared world cannot spawn or despawn directly --
//! those need `&mut World`. Instead it queues the change on a [`Commands`]
//! queue; the driver applies the queue with [`World::apply`] once the callback
//! has returned.
//!
//! A queue belongs to a scope: everything queued through it lands in that
//! scope, and every command carries the scope it was queued from so that
//! applying it later cannot be redirected by a queue that outlived its scope.
//!
//! A queued spawn still needs a handle right away, so [`Commands::spawn`]
//! reserves one with [`World::reserve_entity`] and returns it. The handle is
//! usable immediately -- it can be stored in a component or handed to another
//! callback -- and refers to a live entity once the queue is applied.
//!
//! Commands run in the order they were queued. A command that acts on a
//! reserved entity must therefore be queued after the `Spawn` that brings it
//! to life: for example [`Commands::despawn`] does nothing when the entity is
//! not alive yet.
//!
//! A queue only queues changes to what its scope owns, and a despawn it cannot
//! take is dropped when it is queued rather than left to fail quietly when it
//! is applied. What a command does not decide up front is where the entity has
//! moved by the time it runs: a queued despawn re-checks its scope when it is
//! applied, so an entity that has since left that scope survives. A `Spawn`
//! pushed by hand with [`Commands::push`] carries its own ordering.
//!
//! `````rust
//! # use unlit_ecs::{World, bundle};
//! let mut world = World::new();
//!
//! let entity = {
//!     let scope = world.root_scope();
//!     let commands = scope.queue();
//!     commands.spawn(bundle!["entity"])
//! };
//! // Nothing has happened yet.
//! assert!(!world.root_scope().contains(entity));
//! world.apply().unwrap();
//! assert!(world.root_scope().contains(entity));
//! `````

use crate::bundle::Bundle;
use crate::entity::Entity;
use crate::error::EntityAccessError;
use crate::world::World;

/// A structural change that can be applied later.
pub trait Command: 'static {
    /// Apply the change to the world.
    ///
    /// Most commands cannot fail: they check what they are allowed to touch
    /// when they are queued and again when they are applied, and a change that
    /// no longer applies is dropped. Only a command that spawns on a reserved
    /// handle has something to report, so it answers the error instead of
    /// panicking on it.
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError>;
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
#[derive(Clone, Copy)]
pub struct Commands<'w> {
    world: &'w World,
    scope: Entity,
}

impl core::fmt::Debug for Commands<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Commands")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
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
    /// applied. Its name comes from the `bundle!` that built it, so a
    /// queued spawn names the code that wrote the bundle rather than the place
    /// that queued it.
    pub fn spawn<B: Bundle + 'static>(&self, bundle: B) -> Entity {
        let bundle = bundle.into_builder();
        let entity = self.world.reserve_entity(bundle.name());
        self.push(Spawn {
            entity,
            scope: self.scope,
            bundle: Some(bundle),
        });
        entity
    }

    /// Queue a spawn of `bundle` in this scope under `name`.
    ///
    /// [`Commands::spawn`] names the entity after the `bundle!` that built it;
    /// this is for the objects whose name has to say more than a source
    /// position does.
    pub fn spawn_named<B: Bundle + 'static>(&self, name: &'static str, bundle: B) -> Entity {
        let entity = self.world.reserve_entity(name);
        self.push(Spawn {
            entity,
            scope: self.scope,
            bundle: Some(bundle),
        });
        entity
    }

    /// Queue the despawn of `entity` and its subtree.
    ///
    /// Does nothing when this queue's scope does not own `entity`, or when the
    /// entity is one of the tree's two scaffolding entities: the command would
    /// be refused when it was applied, and queueing it only to have it silently
    /// dropped later is the worse of the two failures. A resource in the shared
    /// subtree is the root's to remove, so a queue of any other scope drops it.
    pub fn despawn(&self, entity: Entity) {
        if self.world.is_scaffolding(entity) || !self.world.owns(self.scope, entity) {
            return;
        }
        self.push(Despawn {
            entity,
            scope: self.scope,
        });
    }

    /// Queue a relink of `child` into `parent`.
    ///
    /// Nothing happens when it is applied if this queue's scope does not own
    /// both ends: the shared subtree is the root's to rearrange, and nobody
    /// moves a root or the shared scope itself.
    pub fn link(&self, child: Entity, parent: Entity) {
        self.push(Link {
            child,
            parent,
            scope: self.scope,
        });
    }

    /// Queue a detach of `child` into this queue's scope.
    ///
    /// Nothing happens when it is applied if this queue's scope does not own
    /// `child`: an entity in the shared subtree moves only for the root.
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
    fn apply(mut self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        let bundle = self.bundle.take().expect("a command is applied only once");
        world.spawn_at_in(self.entity, self.scope, bundle)
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
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        // The scope is what the queue saw at queue time; an entity that has
        // moved out of it since, or that turned out to be scaffolding, is not
        // this command's to remove.
        world.despawn_in(self.entity, self.scope)
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
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        world.link_in(self.child, self.parent, self.scope)
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
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), EntityAccessError> {
        world.unlink_in(self.child, self.scope, self.scope)
    }
}
