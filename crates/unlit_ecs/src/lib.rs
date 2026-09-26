//! `unlit_ecs` — a compact archetype ECS for the `unlit3d` layer.
//!
//! The design is deliberately small. There is no change detection, no component
//! hooks and no scheduler; everything that would need those is done by the
//! caller on the entities themselves.
//!
//! # Worlds
//!
//! Components live in archetypes, and every component value lives in its own
//! cell. That has two consequences:
//!
//! - Reading and writing components needs only `&World`. Structural changes —
//!   spawning and despawning — need `&mut World`, or a [`Commands`] queue
//!   applied by the driver.
//! - The two worlds differ only in their cells. [`LocalWorld`] uses
//!   `RefCell` and cannot leave its thread; [`SendWorld`] uses `RwLock` and
//!   is `Send + Sync`. Everything else is shared code.
//!
//! # Model
//!
//! - An entity's components are fixed when it is spawned. To change the set of
//!   components, despawn the entity and spawn a new one.
//! - When one entity should point at another, store the [`Entity`] handle in
//!   a component and keep it up to date yourself; the world does not track the
//!   reference or clean it up.
//! - There are no events or observers. To drive behaviour, the caller reads the
//!   world and calls the closure or method it wants on the entities it chooses,
//!   using [`World::with_mut`] or a query.
//! - A behaviour that needs to wait returns a future, and the caller decides
//!   when to poll it. No executor is built in.
//!
//! # Example
//!
//! `````
//! use unlit_ecs::{LocalWorld, Query};
//!
//! // A behaviour component: data the driver reads and writes every frame.
//! struct Spin {
//!     radians_per_second: f32,
//!     angle: f32,
//! }
//!
//! let mut world = LocalWorld::new();
//! let cube = world.spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
//! let clock = world.spawn((0.016f32,));
//!
//! // Drive every Spin. The caller picks which entities to touch and in what
//! // order; the library has no built-in notion of a scene graph.
//! let delta = world.get::<f32>(clock).unwrap().to_owned();
//! for (_, mut spin) in world.query::<&mut Spin>() {
//!     spin.angle += spin.radians_per_second * delta;
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
mod entity;
mod hash;
mod mode;
mod query;
#[cfg(test)]
mod tests_common;
mod world;

pub use archetype::{Archetype, Archetypes};
pub use bundle::{ArchetypeBuilder, Bundle};
pub use command::{Command, CommandErase, Commands};
#[doc(hidden)]
pub use component::Component;
pub use entity::Entity;
pub use hash::{EntityHashMap, EntityHashSet, TypeIdHashMap, TypeIdHashSet};
pub use mode::{LocalMode, Mode, SendMode};
pub use query::{Or, Query, QueryFilter, QueryIter, With, Without};
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
    pub use crate::{Entity, LocalWorld, Query, SendWorld, Without, World};
}
