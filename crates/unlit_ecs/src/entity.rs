//! Entity identifiers and their allocator.
//!
//! An [`Entity`] is an index plus a generation. Despawning bumps the
//! generation, so a handle to a dead entity never resolves to a new entity that
//! happened to reuse the index.

/// A handle to an entity.
///
/// The handle is a plain value: it stays valid across world mutations as long
/// as the entity lives, and resolves to nothing once the entity is despawned.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    /// A handle that never refers to a spawned entity.
    pub const PLACEHOLDER: Self = Self {
        index: u32::MAX,
        generation: u32::MAX,
    };

    /// Build a handle from raw parts.
    ///
    /// The handle only refers to an entity if it was produced by the same
    /// world, so this is for restoring a handle that came from that world
    /// (a save file, for example).
    pub const fn from_raw(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// The index part.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation part.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// The two parts as one value, generation high.
    ///
    /// This is not the order [`Ord`] uses: the comparison is by index first,
    /// then generation, which is what makes entities spawned together sort
    /// together.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }
}

/// Where an entity's components live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Location {
    pub(crate) archetype: u32,
    pub(crate) row: u32,
}

impl Location {
    pub(crate) fn row(self) -> usize {
        self.row as usize
    }
}

#[derive(Clone, Copy)]
struct Meta {
    generation: u32,
    location: Option<Location>,
    /// Whether the index is handed out by [`Entities::reserve`] and not
    /// spawned or freed yet.
    reserved: bool,
}

/// Allocates entity handles and maps them to their row in an archetype.
#[derive(Default)]
pub(crate) struct Entities {
    meta: Vec<Meta>,
    /// Indices whose entity is despawned and whose current generation is
    /// already recorded in `meta`.
    free: Vec<u32>,
    /// How many indices are reserved but not spawned. Every reserved index is
    /// flagged in its [`Meta`], so releasing one is a flag flip rather than a
    /// search.
    reserved: usize,
}

impl Entities {
    /// Number of live entities.
    pub(crate) fn len(&self) -> usize {
        self.meta.len() - self.free.len() - self.reserved
    }

    /// Whether the handle refers to a live entity.
    pub(crate) fn contains(&self, entity: Entity) -> bool {
        self.location(entity).is_some()
    }

    /// Where the entity lives, if it is live.
    pub(crate) fn location(&self, entity: Entity) -> Option<Location> {
        let meta = self.meta.get(entity.index as usize)?;
        (meta.generation == entity.generation)
            .then_some(meta.location)
            .flatten()
    }

    /// Record where an entity lives, replacing any earlier location.
    pub(crate) fn set_location(&mut self, entity: Entity, location: Location) {
        let meta = &mut self.meta[entity.index as usize];
        debug_assert_eq!(meta.generation, entity.generation);
        meta.location = Some(location);
    }

    /// Hand out a fresh handle and mark the index as reserved.
    pub(crate) fn reserve(&mut self) -> Entity {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.meta.push(Meta {
                    generation: 0,
                    location: None,
                    reserved: false,
                });
                (self.meta.len() - 1) as u32
            }
        };
        self.meta[index as usize].reserved = true;
        self.reserved += 1;
        Entity {
            index,
            generation: self.meta[index as usize].generation,
        }
    }

    /// Hand out a handle for a new entity, reusing a free index when possible.
    pub(crate) fn alloc(&mut self) -> Entity {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.meta.push(Meta {
                    generation: 0,
                    location: None,
                    reserved: false,
                });
                (self.meta.len() - 1) as u32
            }
        };
        Entity {
            index,
            generation: self.meta[index as usize].generation,
        }
    }

    /// Whether the handle came from [`reserve`](Self::reserve) and has not
    /// been spawned or freed yet.
    pub(crate) fn is_reserved(&self, entity: Entity) -> bool {
        self.meta
            .get(entity.index as usize)
            .is_some_and(|meta| meta.reserved && meta.generation == entity.generation)
    }

    /// Mark a reserved handle as spawned.
    pub(crate) fn take_reserved(&mut self, entity: Entity) {
        if self.is_reserved(entity) {
            self.meta[entity.index as usize].reserved = false;
            self.reserved -= 1;
        }
    }

    /// Release a dead entity's index, so a later entity reuses it with a new
    /// generation.
    ///
    /// Returns whether the handle referred to a reserved or live entity; a
    /// stale handle, or one already freed, is a no-op.
    pub(crate) fn free(&mut self, entity: Entity) -> bool {
        let Some(meta) = self.meta.get_mut(entity.index as usize) else {
            return false;
        };
        if meta.generation != entity.generation {
            return false;
        }
        if !meta.reserved && meta.location.is_none() {
            return false;
        }
        meta.location = None;
        meta.generation = meta.generation.wrapping_add(1);
        if meta.reserved {
            meta.reserved = false;
            self.reserved -= 1;
        }
        self.free.push(entity.index);
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_reuses_freed_indices_with_a_new_generation() {
        let mut entities = Entities::default();
        let first = entities.alloc();
        assert_eq!(first.index(), 0);
        assert_eq!(first.generation(), 0);

        entities.set_location(
            first,
            Location {
                archetype: 0,
                row: 0,
            },
        );
        assert!(entities.free(first));

        let second = entities.alloc();
        assert_eq!(second.index(), 0, "the index is reused");
        assert_ne!(second.generation(), first.generation(), "but not the id");
        assert!(!entities.contains(first), "the old handle is dead");
    }

    #[test]
    fn reserved_entities_are_not_alive_until_spawned() {
        let mut entities = Entities::default();
        let entity = entities.reserve();
        assert!(entities.is_reserved(entity));
        assert!(!entities.contains(entity));
        assert_eq!(entities.len(), 0);

        entities.set_location(
            entity,
            Location {
                archetype: 0,
                row: 0,
            },
        );
        entities.take_reserved(entity);
        assert!(entities.contains(entity));
        assert_eq!(entities.len(), 1);
    }

    #[test]
    fn freeing_a_stale_handle_fails() {
        let mut entities = Entities::default();
        let entity = entities.alloc();
        entities.set_location(
            entity,
            Location {
                archetype: 0,
                row: 0,
            },
        );
        assert!(entities.free(entity));
        assert!(!entities.free(entity), "the handle is already stale");
    }

    #[test]
    fn len_counts_live_entities_only() {
        let mut entities = Entities::default();
        assert_eq!(entities.len(), 0);
        let first = entities.alloc();
        let second = entities.alloc();
        entities.set_location(
            first,
            Location {
                archetype: 0,
                row: 0,
            },
        );
        entities.set_location(
            second,
            Location {
                archetype: 0,
                row: 1,
            },
        );
        assert_eq!(entities.len(), 2);
        assert!(entities.free(first));
        assert_eq!(entities.len(), 1);
        assert!(entities.contains(second));
    }

    #[test]
    fn entity_bits_round_trip() {
        let entity = Entity::from_raw(7, 3);
        assert_eq!(entity.index(), 7);
        assert_eq!(entity.generation(), 3);
        assert_eq!(entity.to_bits(), (3u64 << 32) | 7);
    }
}
