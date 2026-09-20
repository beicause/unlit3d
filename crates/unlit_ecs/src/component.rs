//! Components and the errors of structural component changes.
//!
//! A component is any `'static` type. Components live in archetypes, and an
//! entity's set of components is fixed when it is spawned: components are only
//! read and written, never added or removed — unless the type opts in by
//! implementing [`AddableComponent`].

use core::fmt;

/// A value stored on an entity.
///
/// This is implemented for every `'static` type, so it only serves as
/// documentation and as a bound: it is what keeps a non-`'static` type from
/// being used as a component.
pub trait Component: 'static {}

impl<T: 'static> Component for T {}

/// A component that may be added to and removed from a live entity.
///
/// By default an entity's components are fixed at spawn time; changing the set
/// of components means despawning and respawning. Implementing this trait opts
/// a component type into [`World::insert`](crate::LocalWorld::insert) and
/// [`World::remove`](crate::LocalWorld::remove), which move the entity to
/// another archetype.
///
/// [`Children`](crate::Children) and [`ChildOf`](crate::ChildOf) implement
/// it, because the hierarchy is maintained while the world runs.
///
/// `````
/// # use unlit_ecs::{AddableComponent, LocalWorld, Entity};
/// #[derive(Debug, PartialEq)]
/// struct Health(u32);
/// impl AddableComponent for Health {}
///
/// let mut world = LocalWorld::new();
/// let entity = world.spawn((1u32,));
/// world.insert(entity, Health(3)).unwrap();
/// world.with_mut::<Health, _>(entity, |health| health.0 += 1).unwrap();
/// assert_eq!(world.remove::<Health>(entity).unwrap(), Health(4));
/// `````
pub trait AddableComponent: Component {}

/// The entity does not exist (was never spawned, or was despawned).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NoSuchEntity;

impl fmt::Display for NoSuchEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no such entity")
    }
}

impl core::error::Error for NoSuchEntity {}

/// Why [`World::insert`](crate::LocalWorld::insert) failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InsertError {
    /// The entity does not exist.
    NoSuchEntity,
    /// The entity already has the component.
    AlreadyPresent {
        /// The component's name.
        component: &'static str,
    },
}

impl fmt::Display for InsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchEntity => f.write_str("no such entity"),
            Self::AlreadyPresent { component } => {
                write!(f, "the entity already has a {}", component)
            }
        }
    }
}

impl core::error::Error for InsertError {}

impl From<NoSuchEntity> for InsertError {
    fn from(_: NoSuchEntity) -> Self {
        Self::NoSuchEntity
    }
}

/// Why [`World::remove`](crate::LocalWorld::remove) failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoveError {
    /// The entity does not exist.
    NoSuchEntity,
    /// The entity does not have the component.
    Missing {
        /// The component's name.
        component: &'static str,
    },
}

impl fmt::Display for RemoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchEntity => f.write_str("no such entity"),
            Self::Missing { component } => write!(f, "the entity has no {}", component),
        }
    }
}

impl core::error::Error for RemoveError {}

impl From<NoSuchEntity> for RemoveError {
    fn from(_: NoSuchEntity) -> Self {
        Self::NoSuchEntity
    }
}

/// A marker component: its entity is a "resource".
///
/// Resources are ordinary entities that every behaviour may reach, no matter
/// where it sits in the hierarchy — see
/// [`Ctx::can_access`](crate::Ctx::can_access). References to resources are
/// plain [`Entity`](crate::Entity) handles, or plain component references.
///
/// `````
/// # use unlit_ecs::{LocalWorld, Resource};
/// let mut world = LocalWorld::new();
/// let settings = world.spawn((Resource, 60u32));
/// assert!(world.has::<Resource>(settings));
/// `````
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Resource;

impl AddableComponent for Resource {}
