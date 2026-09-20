//! `unlit_ecs` — a compact archetype ECS for the `unlit3d` layer.
//!
//! The design is deliberately small. There is no change detection, no component
//! hooks, no resource registry and no scheduler; everything that would need
//! those is done by the caller on the entities themselves.
//!
//! # Worlds
//!
//! Components live in archetypes, and every component value lives in its own
//! cell. That has two consequences:
//!
//! - Reading and writing components needs only `&World`. Structural changes —
//!   spawning, despawning, adding or removing components, reparenting — need
//!   `&mut World`, or a [`Commands`] queue applied by the driver.
//! - The two worlds differ only in their cells. [`LocalWorld`] uses
//!   `RefCell` and cannot leave its thread; [`SendWorld`] uses `RwLock` and
//!   is `Send + Sync`. Everything else is shared code.
//!
//! # Model
//!
//! - An entity's components are fixed when it is spawned. To change the set of
//!   components, despawn and respawn. The exceptions are components that opt in
//!   with [`AddableComponent`]: [`Children`], [`ChildOf`] and
//!   [`Resource`].
//! - [`Ctx`] is one entity's view of the world: itself, its descendants, and
//!   resource entities. A behaviour component cannot reach its parent or a
//!   sibling, so an entity's state stays its own.
//! - There are no events or observers. To reach another entity, call its
//!   behaviour component with [`World::call`]; a caller composes a direction
//!   from the hierarchy walkers (`ancestors`, `descendants`) when it wants
//!   one.
//! - Asynchronous behaviour returns a future; the driver polls it with
//!   [`Tasks`]. No executor is built in.
//!
//! # Example
//!
//! `````
//! use unlit_ecs::{Ctx, LocalWorld, Query, Resource};
//!
//! // A behaviour component: a closure the driver calls every frame.
//! struct Spin {
//!     radians_per_second: f32,
//!     angle: f32,
//! }
//!
//! let mut world = LocalWorld::new();
//! let scene = world.spawn(("scene",));
//! let cube = world.spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
//! world.set_parent(cube, scene);
//!
//! // Drive every Spin under the scene, depth first. The caller picks the
//! // order; the library has no built-in notion of "down".
//! let clock = Resource;
//! let _clock = world.spawn((clock, 0.016f32));
//! for entity in world.descendants(scene) {
//!     world.call::<Spin, _>(entity, |spin, ctx: Ctx<'_, _>| {
//!         let Some(clock) = ctx.get_in::<f32>(_clock) else { return };
//!         spin.angle += spin.radians_per_second * *clock;
//!     });
//! }
//!
//! assert_ne!(world.get::<Spin>(cube).unwrap().angle, 0.0);
//! let _ = world.query::<&Spin>().count();
//! `````

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod archetype;
mod bundle;
mod command;
mod component;
mod ctx;
mod entity;
mod hash;
mod hierarchy;
mod mode;
mod query;
mod tasks;
#[cfg(test)]
mod tests_common;
mod world;

pub use archetype::{Archetype, Archetypes};
pub use bundle::{ArchetypeBuilder, Bundle};
pub use command::{Command, CommandErase, Commands};
#[doc(hidden)]
pub use component::Component;
pub use component::{AddableComponent, InsertError, NoSuchEntity, RemoveError, Resource};
pub use ctx::Ctx;
pub use entity::Entity;
pub use hash::{EntityHashMap, EntityHashSet, TypeIdHashMap, TypeIdHashSet};
pub use hierarchy::{ChildOf, Children};
pub use mode::{LocalMode, Mode, SendMode};
pub use query::{Query, QueryIter, With, Without};
pub use tasks::Tasks;
pub use world::World;

/// The world whose components live in `RefCell`s.
///
/// It is `!Send`, so it stays on the thread that created it; this is where a
/// renderer and other thread-bound state belong.
pub type LocalWorld = World<LocalMode>;

/// The world whose components live in `RwLock`s.
///
/// It is `Send + Sync`, so worker threads may read and write it.
pub type SendWorld = World<SendMode>;

/// The types most callers need.
pub mod prelude {
    pub use crate::{
        AddableComponent, ChildOf, Children, Ctx, Entity, LocalWorld, Query, Resource, SendWorld,
        Without, World,
    };
}
