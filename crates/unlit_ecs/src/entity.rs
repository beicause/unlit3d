//! Entity identifiers and their allocator.
//!
//! An [`Entity`] is an index plus a generation. Despawning bumps the
//! generation, so a handle to a dead entity never resolves to a new entity that
//! happened to reuse the index.
//!
//! A handle also carries a name, so that a message names the object it is about
//! rather than a bare number. [`Debug`] prints it. The name is only a label: it
//! takes no part in comparison, hashing or [`Entity::to_bits`].
//!
//! Where the name comes from is decided where the entity is made. A [`crate::bundle!`]
//! names the entity after its own source position, [`crate::ArchetypeBuilder::named`]
//! and [`crate::ScopeMut::spawn_named`] name it after the string the caller
//! passed, and [`crate::World::reserve_entity`] names it at the reservation.
//! A builder put together at run time by hand carries no source position, so an
//! entity spawned from one is called by whatever the caller named it.

use crate::error::EntityAccessError;

/// A handle to an entity.
///
/// The handle is a plain value: it stays valid across world mutations as long
/// as the entity lives, and resolves to nothing once the entity is despawned.
///
/// It also carries a name, which [`Debug`] prints. A name is given where the
/// entity is made: a [`crate::bundle!`] names it after its own source position,
/// [`crate::ArchetypeBuilder::named`] and [`crate::ScopeMut::spawn_named`] after
/// the string the caller passed, and [`crate::World::reserve_entity`] at the
/// reservation. Two handles to one entity can carry different names and still be
/// equal, because a handle restored from [`Entity::to_bits`] has no name to
/// restore; [`crate::Scope::entity_name`] reads the one the row stores.
#[derive(Clone, Copy)]
pub struct Entity {
    index: u32,
    generation: u32,
    name: &'static str,
}

/// Prints the name and the bits the handle travels as, and not the index and
/// generation they are made of: those are the allocator's business, while the
/// bits are the only part a caller holds when the handle came over a wire.
impl core::fmt::Debug for Entity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Entity")
            .field("name", &self.name)
            .field("bits", &self.to_bits())
            .finish()
    }
}
impl PartialEq for Entity {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl Eq for Entity {}

impl PartialOrd for Entity {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entity {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.index, self.generation).cmp(&(other.index, other.generation))
    }
}

impl Entity {
    /// Build a handle from raw parts.
    ///
    /// The handle only refers to an entity if it was produced by the same
    /// world, so this is for restoring a handle that came from that world
    /// (a save file, for example).
    pub const fn from_raw(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            name: "",
        }
    }

    /// The name part, empty when the handle was restored rather than spawned.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
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

    /// The handle [`Entity::to_bits`] produced.
    ///
    /// The handle only refers to an entity if it was produced by the same
    /// world, so this is for restoring a handle that came from that world. The
    /// restored handle carries no name.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self::from_raw(bits as u32, (bits >> 32) as u32)
    }
}

/// An [`Entity`] as its [`Entity::to_bits`] integer.
///
/// `Entity` is not a newtype, so it cannot be reflected through
/// [`facet::Facet`]'s `transparent`; this proxy is what a field whose type is
/// an entity names with `#[facet(opaque, proxy = EntityProxy)]`, and its
/// transparent `u64` is the shape such a field takes in JSON.
#[cfg(feature = "reflect")]
#[derive(facet::Facet)]
#[facet(transparent)]
pub struct EntityProxy(pub u64);

#[cfg(feature = "reflect")]
impl From<EntityProxy> for Entity {
    fn from(proxy: EntityProxy) -> Self {
        Entity::from_bits(proxy.0)
    }
}

#[cfg(feature = "reflect")]
impl From<&Entity> for EntityProxy {
    fn from(value: &Entity) -> Self {
        Self(value.to_bits())
    }
}

/// A list of [`Entity`] handles, each as its [`Entity::to_bits`] integer.
///
/// The plural of [`EntityProxy`]: a reflected field whose type is
/// `Vec<Entity>` names this with `#[facet(opaque, proxy = EntityVecProxy)]`,
/// and its transparent `Vec<u64>` is the JSON array such a field becomes.
#[cfg(feature = "reflect")]
#[derive(facet::Facet)]
#[facet(transparent)]
pub struct EntityVecProxy(pub Vec<u64>);

#[cfg(feature = "reflect")]
impl From<EntityVecProxy> for Vec<Entity> {
    fn from(proxy: EntityVecProxy) -> Self {
        proxy.0.into_iter().map(Entity::from_bits).collect()
    }
}

#[cfg(feature = "reflect")]
impl From<&Vec<Entity>> for EntityVecProxy {
    fn from(value: &Vec<Entity>) -> Self {
        Self(value.iter().map(|entity| entity.to_bits()).collect())
    }
}

/// Where an entity's components live.
///
/// A location names an archetype and a row in it. Entities of the same
/// archetype share the archetype, so a caller holding many locations can group
/// them by [`Location::archetype`] and resolve that archetype's columns once,
/// then reach each row through [`Scope::query_one`](crate::Scope::query_one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Location {
    pub(crate) archetype: u32,
    pub(crate) row: u32,
}

impl Location {
    /// The archetype the entity's components live in.
    #[inline]
    pub fn archetype(self) -> u32 {
        self.archetype
    }

    /// The entity's row within its archetype.
    #[inline]
    pub fn row(self) -> usize {
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
    /// Whether the handle refers to a live entity.
    pub(crate) fn contains(&self, entity: Entity) -> bool {
        self.location(entity).is_ok()
    }

    /// Where the entity lives.
    ///
    /// A handle whose index was never allocated, whose generation was bumped by
    /// a despawn, or whose index is reserved but not spawned yet answers
    /// [`EntityAccessError::NotAlive`].
    #[inline]
    pub(crate) fn location(&self, entity: Entity) -> Result<Location, EntityAccessError> {
        let Some(meta) = self.meta.get(entity.index as usize) else {
            return Err(EntityAccessError::NotAlive { entity });
        };
        if meta.generation != entity.generation {
            return Err(EntityAccessError::NotAlive { entity });
        }
        meta.location.ok_or(EntityAccessError::NotAlive { entity })
    }

    /// Record where an entity lives, replacing any earlier location.
    ///
    /// Panics when the handle is not live: a location is only ever recorded for
    /// an entity that was just allocated or reserved.
    pub(crate) fn set_location(&mut self, entity: Entity, location: Location) {
        let meta = self
            .meta
            .get_mut(entity.index as usize)
            .unwrap_or_else(|| panic!("{entity:?} names no entity index"));
        assert_eq!(
            meta.generation, entity.generation,
            "{entity:?} is a stale handle"
        );
        meta.location = Some(location);
    }

    /// Reuse a freed index, or grow `meta` for a fresh one.
    fn index(&mut self) -> usize {
        match self.free.pop() {
            Some(index) => index as usize,
            None => {
                self.meta.push(Meta {
                    generation: 0,
                    location: None,
                    reserved: false,
                });
                self.meta.len() - 1
            }
        }
    }

    /// Hand out a fresh handle and mark the index as reserved.
    pub(crate) fn reserve(&mut self, name: &'static str) -> Entity {
        let index = self.index();
        self.meta[index].reserved = true;
        self.reserved += 1;
        Entity {
            index: index as u32,
            generation: self.meta[index].generation,
            name,
        }
    }

    /// Hand out a handle for a new entity, reusing a free index when possible.
    pub(crate) fn alloc(&mut self, name: &'static str) -> Entity {
        let index = self.index();
        Entity {
            index: index as u32,
            generation: self.meta[index].generation,
            name,
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
    ///
    /// Returns whether the handle was reserved; a handle that is not is left
    /// untouched.
    pub(crate) fn take_reserved(&mut self, entity: Entity) -> bool {
        if !self.is_reserved(entity) {
            return false;
        }
        self.meta[entity.index as usize].reserved = false;
        self.reserved -= 1;
        true
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
        let first = entities.alloc("test");
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

        let second = entities.alloc("test");
        assert_eq!(second.index(), 0, "the index is reused");
        assert_ne!(second.generation(), first.generation(), "but not the id");
        assert!(!entities.contains(first), "the old handle is dead");
    }

    #[test]
    fn reserved_entities_are_not_alive_until_spawned() {
        let mut entities = Entities::default();
        let entity = entities.reserve("test");
        assert!(entities.is_reserved(entity));
        assert!(!entities.contains(entity));

        entities.set_location(
            entity,
            Location {
                archetype: 0,
                row: 0,
            },
        );
        assert!(
            entities.take_reserved(entity),
            "the reservation is consumed"
        );
        assert!(entities.contains(entity));
        assert!(!entities.take_reserved(entity), "and only once");
    }

    #[test]
    fn freeing_a_stale_handle_fails() {
        let mut entities = Entities::default();
        let entity = entities.alloc("test");
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
    fn freeing_retires_a_handle_and_keeps_the_other() {
        let mut entities = Entities::default();
        let first = entities.alloc("test");
        let second = entities.alloc("test");
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
        assert!(entities.free(first));
        assert!(!entities.contains(first));
        assert!(entities.contains(second));
    }

    #[test]
    fn entity_bits_round_trip() {
        let entity = Entity::from_raw(7, 3);
        assert_eq!(entity.index(), 7);
        assert_eq!(entity.generation(), 3);
        assert_eq!(entity.to_bits(), (3u64 << 32) | 7);
    }

    #[test]
    fn the_name_is_a_label_and_not_part_of_the_identity() {
        let mut entities = Entities::default();
        let named = entities.reserve("ball");
        let nameless = Entity::from_raw(named.index(), named.generation());
        assert_eq!(named, nameless, "a name does not change the handle");
        assert_eq!(named.to_bits(), nameless.to_bits(), "nor its bits");
        assert_eq!(named.name(), "ball");
        assert_eq!(nameless.name(), "", "a restored handle has no name");
        assert_eq!(
            Entity::from_bits(named.to_bits()).name(),
            "",
            "bits carry no name"
        );
    }

    #[test]
    fn a_debug_message_names_the_entity() {
        let mut entities = Entities::default();
        let printed = format!("{:?}", entities.reserve("ball"));
        assert!(printed.contains("ball"), "{printed} names the entity");
    }
}
