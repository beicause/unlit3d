//! Small helpers that have no better home.

use core::hash::{BuildHasher, Hash, Hasher};
use core::ops::Deref;
use foldhash::fast::FixedState;

/// The hasher [`Hashed`] computes its words with.
///
/// Hashbrown's own hasher, in its fixed-seed state: the crate's hash tables are
/// hashbrown's and hash with foldhash, so a word stored this way is already in
/// the shape those tables mix, and independently built hashers agree on the
/// hash of equal values. [`Hashed`]'s equality reads that word, so a hasher
/// seeded per instance — `hashbrown::DefaultHashBuilder`, whose default is a
/// fresh `RandomState`, say — would make equal values compare unequal.
type FixedHasher = FixedState;

/// A value whose hash is computed once, up front.
///
/// Hashing one of these writes the stored word instead of walking the value,
/// and comparing two of them settles inequality on the word alone. That is
/// worth doing where a value is large, shared, and hashed once per entity per
/// frame — see [`VertexLayout`](crate::specialize::VertexLayout).
///
/// Equality checks the hash before the value. That is sound because [`Hash`]
/// requires equal values to hash alike: a hash mismatch therefore means the
/// values differ, and a hash collision only costs the comparison that would
/// have run anyway. The hasher is fixed rather than chosen per instance for the
/// same reason — equal values have to keep hashing alike for the check to hold.
#[derive(Clone, Debug)]
pub struct Hashed<V> {
    /// The value's hash.
    hash: u64,
    /// The value the hash was taken from.
    value: V,
}

impl<V: Hash> Hashed<V> {
    /// Compute `value`'s hash once and keep it beside the value.
    pub fn new(value: V) -> Self {
        Self {
            hash: FixedHasher::default().hash_one(&value),
            value,
        }
    }

    /// The hash [`Self::new`] computed.
    pub fn hash(&self) -> u64 {
        self.hash
    }
}

impl<V> Deref for Hashed<V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<V: PartialEq> PartialEq for Hashed<V> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.value == other.value
    }
}

impl<V: Eq> Eq for Hashed<V> {}

impl<V> Hash for Hashed<V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Equal values share this word, so writing it satisfies the contract
        // without touching the value.
        state.write_u64(self.hash);
    }
}

/// Headless device setup shared by the crate's unit tests.
#[cfg(test)]
pub(crate) mod test {
    /// A `(device, queue)` pair on wgpu's noop backend.
    ///
    /// The noop backend stubs every GPU operation out — except buffer creation
    /// and mapping — so it needs no adapter and works everywhere, including
    /// machines with no GPU at all. That is enough for the unit tests, which
    /// only exercise validation, resource bookkeeping and command encoding.
    pub(crate) fn noop_device() -> (wgpu::Device, wgpu::Queue) {
        wgpu::Device::noop(&wgpu::DeviceDescriptor::default())
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn equal_values_are_equal_and_hash_alike() {
        let one = Hashed::new((1u32, "a"));
        let other = Hashed::new((1u32, "a"));

        assert_eq!(one, other);
        assert_eq!(
            one.hash(),
            other.hash(),
            "separately built hashers agree on equal values"
        );
    }

    #[test]
    fn different_values_are_not_equal() {
        assert_ne!(Hashed::new(1u32), Hashed::new(2u32));
    }

    #[test]
    fn a_hashed_value_derefs_to_the_value() {
        let hashed = Hashed::new(vec![1u32, 2, 3]);

        assert_eq!(*hashed, vec![1, 2, 3]);
        assert_eq!(hashed.len(), 3);
    }

    /// The stored word is the hash of the value, computed the same way
    /// [`Hashed::new`] computes it.
    #[test]
    fn the_stored_hash_is_the_value_hash() {
        let hashed = Hashed::new("abc");

        assert_eq!(hashed.hash(), FixedHasher::default().hash_one("abc"));
    }

    /// A hasher that records the words written into it, and rejects anything
    /// that would mean the value itself was walked.
    #[derive(Default)]
    struct CaptureHasher {
        written: Vec<u64>,
    }

    impl Hasher for CaptureHasher {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, _bytes: &[u8]) {
            panic!("hashing a Hashed must write its stored word, not the value's bytes");
        }

        fn write_u64(&mut self, word: u64) {
            self.written.push(word);
        }
    }

    /// Hashing through the trait writes the stored word and stops there, which
    /// is the whole point of the type.
    #[test]
    fn hashing_writes_only_the_stored_word() {
        let hashed = Hashed::new(vec!["a", "b", "c"]);
        let mut hasher = CaptureHasher::default();
        Hash::hash(&hashed, &mut hasher);

        assert_eq!(hasher.written, vec![hashed.hash()]);
    }

    /// A value that counts how often it is compared, so the hash-first shortcut
    /// in [`Hashed`]'s equality can be observed.
    #[derive(Debug)]
    struct Counted {
        compares: Rc<Cell<usize>>,
        value: u32,
    }

    impl Counted {
        fn new(compares: &Rc<Cell<usize>>, value: u32) -> Self {
            Self {
                compares: Rc::clone(compares),
                value,
            }
        }
    }

    impl Hash for Counted {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.value.hash(state);
        }
    }

    impl PartialEq for Counted {
        fn eq(&self, other: &Self) -> bool {
            self.compares.set(self.compares.get() + 1);
            self.value == other.value
        }
    }

    impl Eq for Counted {}

    /// Values that hash differently are settled by the word alone, so they must
    /// not be compared.
    #[test]
    fn unequal_hashes_settle_equality_without_comparing_the_values() {
        let compares = Rc::new(Cell::new(0));
        let one = Hashed::new(Counted::new(&compares, 1));
        let other = Hashed::new(Counted::new(&compares, 2));
        assert_ne!(one.hash(), other.hash(), "the values hash differently");

        assert_ne!(one, other);
        assert_eq!(compares.get(), 0, "the values were not compared");
    }

    /// Equal hashes do reach the value comparison, which is what keeps a hash
    /// collision from being reported as equality.
    #[test]
    fn equal_hashes_still_compare_the_values() {
        let compares = Rc::new(Cell::new(0));
        let one = Hashed::new(Counted::new(&compares, 1));
        let other = Hashed::new(Counted::new(&compares, 1));

        assert_eq!(one, other);
        assert_eq!(compares.get(), 1, "the values were compared once");
    }
}
