//! Specialized hash containers.
//!
//! Two keys in this crate already carry enough entropy to skip the default
//! hasher:
//!
//! - [`Entity`] is a pair of integers packed into one `u64`. [`EntityHash`]
//!   spreads that value upward with a Fibonacci multiply, which is where the
//!   SwissTable needs the bits, while leaving the low bits so that entities
//!   spawned together land together.
//! - [`TypeId`] is itself a high-quality hash, so [`TypeIdHashMap`] uses
//!   [`NoOpHash`] and [`NoOpHasher`] forwards the `u64` the `Hash` impl
//!   writes.
//!
//! [`EntityHasher`] panics on a key that is not `u64`-shaped, which is a
//! programming error in this crate's own use of it. [`NoOpHasher`] does not:
//! it folds byte-wise writes rather than panicking, so a container that hashes
//! a non-`u64` key still works, if with a worse hash.

use core::any::TypeId;
use core::hash::{BuildHasher, Hash, Hasher};

use hashbrown::{HashMap, HashSet};

use crate::entity::Entity;

/// A [`BuildHasher`] that passes `u64` keys through unchanged.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpHash;

impl BuildHasher for NoOpHash {
    type Hasher = NoOpHasher;

    fn build_hasher(&self) -> Self::Hasher {
        NoOpHasher(0)
    }
}

/// The hasher of [`NoOpHash`].
#[derive(Debug, Default)]
pub struct NoOpHasher(u64);

impl Hasher for NoOpHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Consumers should use `write_u64`; fall back instead of panicking.
        self.0 = bytes.iter().fold(self.0, |hash, byte| {
            hash.rotate_left(8).wrapping_add(*byte as u64)
        });
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// A [`BuildHasher`] for generational indices.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntityHash;

impl BuildHasher for EntityHash {
    type Hasher = EntityHasher;

    fn build_hasher(&self) -> Self::Hasher {
        EntityHasher(0)
    }
}

/// The hasher of [`EntityHash`].
///
/// Multiplying by the fractional part of the golden ratio spreads the index's
/// entropy upward, where hashbrown reads the bits that pick a probe slot, while
/// leaving the low bits so that entities spawned together tend to land
/// together.
#[derive(Debug, Default)]
pub struct EntityHasher(u64);

impl Hasher for EntityHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _bytes: &[u8]) {
        panic!("EntityHasher only hashes u64 keys");
    }

    fn write_u64(&mut self, bits: u64) {
        const UPPER_PHI: u64 = 0x9e37_79b9_0000_0001;
        self.0 = bits.wrapping_mul(UPPER_PHI);
    }
}

impl Hash for Entity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.to_bits());
    }
}

/// A hash map keyed by [`TypeId`].
pub type TypeIdHashMap<V> = HashMap<TypeId, V, NoOpHash>;

/// A hash set of [`TypeId`]s.
pub type TypeIdHashSet = HashSet<TypeId, NoOpHash>;

/// A hash map keyed by [`Entity`].
pub type EntityHashMap<V> = HashMap<Entity, V, EntityHash>;

/// A hash set of [`Entity`]s.
pub type EntityHashSet = HashSet<Entity, EntityHash>;
#[cfg(test)]
mod tests {
    use super::*;
    use core::hash::BuildHasher;

    #[test]
    fn the_no_op_hasher_passes_u64_through() {
        let mut hasher = NoOpHash.build_hasher();
        hasher.write_u64(0x1234_5678_9abc_def0);
        assert_eq!(hasher.finish(), 0x1234_5678_9abc_def0);
    }

    #[test]
    fn the_entity_hasher_is_a_bijection_on_indices() {
        // The golden-ratio multiply has a modular inverse, so distinct keys
        // keep distinct hashes.
        let mut hashes: Vec<u64> = (0..64u64)
            .map(|index| {
                let mut hasher = EntityHash.build_hasher();
                hasher.write_u64(index);
                hasher.finish()
            })
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), 64, "no collisions in this range");
    }

    #[test]
    fn entity_maps_key_by_handle() {
        let mut map: EntityHashMap<u32> = EntityHashMap::default();
        let a = Entity::from_raw(1, 0);
        let b = Entity::from_raw(1, 1);
        map.insert(a, 10);
        map.insert(b, 20);
        assert_eq!(map.get(&a), Some(&10));
        assert_eq!(map.get(&b), Some(&20), "the generation is part of the key");
    }

    #[test]
    fn type_id_maps_key_by_type() {
        let mut map: TypeIdHashMap<&'static str> = TypeIdHashMap::default();
        map.insert(TypeId::of::<u32>(), "u32");
        map.insert(TypeId::of::<bool>(), "bool");
        assert_eq!(map.get(&TypeId::of::<u32>()), Some(&"u32"));
        assert_eq!(map.get(&TypeId::of::<bool>()), Some(&"bool"));
    }
}
