//! Components.
//!
//! A component is any `'static` type. Components live in archetypes, and an
//! entity's set of components is fixed when it is spawned: components are only
//! read and written, never added or removed. To change the set of components,
//! despawn the entity and spawn a new one.

/// A value stored on an entity.
///
/// This is implemented for every `'static` type, so it only serves as
/// documentation and as a bound: it is what keeps a non-`'static` type from
/// being used as a component.
pub trait Component: 'static {}

impl<T: 'static> Component for T {}

/// A marker component: its entity is a "resource".
///
/// A resource is an ordinary entity that a caller may reach from anywhere, so
/// the caller marks it and keeps its handle to look the resource up. References
/// to resources are plain [`Entity`](crate::Entity) handles, or plain
/// component references.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Resource};
/// let mut world = LocalWorld::new();
/// let settings = world.spawn((Resource, 60u32));
/// assert!(world.has::<Resource>(settings));
/// `````
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Resource;
