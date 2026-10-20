//! Queries: picking entities by the components they have.
//!
//! A query is a value, not a type parameter: it says which components to hand
//! out and which archetypes to visit, and it can be built at run time from type
//! ids or written as a constant by the [`query!`](crate::query!) macro.
//!
//! ````rust
//! # use unlit_ecs::{bundle, query, World, Without};
//! let mut world = World::new();
//! world.root_scope_mut().spawn(bundle!(1u32, 10.0f32));
//! world.root_scope_mut().spawn(bundle!(2u32, true));
//! let sum: u32 = world
//!     .root_scope()
//!     .query(query!(&u32))
//!     .map(|(_, row)| *row.read::<u32>())
//!     .sum();
//! assert_eq!(sum, 3);
//! // Only the entity without a bool.
//! let sum: u32 = world
//!     .root_scope()
//!     .query(query!(&u32; Without<bool>))
//!     .map(|(_, row)| *row.read::<u32>())
//!     .sum();
//! assert_eq!(sum, 1);
//! ````
//!
//! An entity matches when the query names components its archetype has and the
//! filter accepts the archetype. Iteration visits archetypes in creation order
//! and rows in storage order, so it is deterministic.

mod desc;
mod iter;
mod row;

pub use desc::{
    Access, Filter, IntoFilter, IntoFilterTuple, IntoSlot, Or, Query, Slot, With, Without,
};
pub use iter::QueryIter;
pub use row::Row;
