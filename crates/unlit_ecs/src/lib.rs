#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_op_in_unsafe_fn)]

mod archetype;
mod borrow;
mod bundle;
mod column;
mod command;
mod component;
mod entity;
mod error;
mod hash;
mod info;
mod lifecycle;
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
pub mod mcp;
/// File-naming helpers that need neither a filesystem nor a random id.
pub mod naming;
pub mod query;
#[cfg(feature = "reflect")]
pub mod reflect;
mod scope;
mod view;
mod world;

pub use archetype::{Archetype, Archetypes};
pub use bundle::{Bundle, DynamicBundle};
pub use command::{Command, CommandErase, Commands};
#[doc(hidden)]
pub use component::Component;
pub use entity::{Entity, Location};
#[cfg(feature = "reflect")]
pub use entity::{EntityProxy, EntityVecProxy};
pub use error::{ArchetypeAccessError, EntityAccessError};
pub use hash::{EntityHashMap, EntityHashSet, TypeIdHashMap, TypeIdHashSet};
pub use info::{ArchetypeInfo, EntityInfo, WorldInfo};
pub use lifecycle::{Lifecycle, LifecycleCallback, OnDespawn, OnSpawn};
#[doc(hidden)]
pub use query::QueryItem;
pub use query::{
    ArchetypeFilter, Col, ColMut, Has, PlanBuilder, Query, QueryBorrow, QueryIter, QueryOne,
};
pub use view::{Scope, ScopeMut};
pub use world::World;

/// The types most callers need.
pub mod prelude {
    pub use crate::{
        ArchetypeAccessError, ArchetypeFilter, Entity, EntityAccessError, Has, Query, QueryBorrow,
        QueryOne, Scope, World, bundle,
    };
}
