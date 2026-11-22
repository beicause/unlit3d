//! What tells a handle from this world's apart from another world's.

use crate::entity::Entity;
use crate::error::RemoteEntityError;
use crate::identity::{RemoteEntity, WorldId};

use super::World;

impl World {
    /// What tells this world's entity handles from another world's.
    ///
    /// The id is made with the world and dies with it: no two live worlds share
    /// one, and a world made after this one is gone does not inherit its id. See
    /// [`World::remote`] for the handle that carries it.
    #[must_use]
    pub fn id(&self) -> WorldId {
        self.id
    }

    /// The handle to carry `entity` out of this world.
    ///
    /// The plain [`Entity`] names a row in this world's storage and means
    /// something else in every other world, so a handle that crosses a thread,
    /// a job or a channel crosses as the pair.
    #[must_use]
    pub fn remote(&self, entity: Entity) -> RemoteEntity {
        RemoteEntity {
            world: self.id,
            entity,
        }
    }

    /// The local handle `remote` names, if it was made in this world.
    ///
    /// Turns a handle from another world down rather than resolving it to
    /// whatever local entity shares its bits; the error names both worlds.
    pub fn resolve(&self, remote: RemoteEntity) -> Result<Entity, RemoteEntityError> {
        if remote.world == self.id {
            Ok(remote.entity)
        } else {
            Err(RemoteEntityError::ForeignWorld {
                entity: remote.entity,
                expected: self.id,
                found: remote.world,
            })
        }
    }
}
