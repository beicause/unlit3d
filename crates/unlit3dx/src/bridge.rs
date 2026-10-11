//! Bridge components: the handles two worlds share to agree on one entity.
//!
//! A bridge component is written by the world that owns the real entity —
//! usually as the payload of a job — and carried by the simulation-side
//! counterpart, so a snapshot can name an entity across worlds without the
//! worlds sharing a single ECS.

use unlit_ecs::Entity;

/// A render-world entity a simulation world addresses by handle.
///
/// The handle is whatever the snapshot the render world reads keys its
/// transforms by, so a ball in the physics world and the mesh in the render
/// world meet only through this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderEntity(pub Entity);
