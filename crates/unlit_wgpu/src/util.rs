//! Small helpers that have no better home.

use core::hash::{BuildHasher, Hash, Hasher};
use core::ops::Deref;
use foldhash::fast::FixedState;
use std::borrow::Cow;

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
///
/// There is no `DerefMut`: editing the value through one would leave the
/// stored word stale, and equal values with different words would compare
/// unequal. A mutation goes through [`Hashed::update`] instead, which
/// recomputes the word after the value changes.
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

    /// Rewrite the value in place and recompute the stored word.
    ///
    /// The closure receives the value itself, so a caller can apply one of the
    /// value type's own mutating methods — a `set_`-style rewrite, say — and
    /// leave the word in step with the value it describes. The recomputation
    /// costs what [`Self::new`] does, so this is the way to change a value
    /// that is already built, not a cheaper one.
    pub fn update(&mut self, rewrite: impl FnOnce(&mut V)) {
        rewrite(&mut self.value);
        self.hash = FixedHasher::default().hash_one(&self.value);
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

/// Create a shader module from composed WGSL.
///
/// The module goes through
/// [`Device::create_shader_module`](wgpu::Device::create_shader_module), so
/// wgpu's runtime checks — shader-side bounds checks, loop bounding and the
/// like — all apply. The WGSL is composed at build time, but a caller composes a
/// module of its own against the package and `unlit3d` hands whole composed
/// modules in, so the source being build-time is no reason to skip them: those
/// checks are what keeps a caller's shader inside its bindings.
///
/// The label goes through `shader_label` first. See WORKAROUND.md, “DXC reads
/// a shader module's label as a source file path”.
pub fn shader_module(device: &wgpu::Device, wgsl: &str, label: &str) -> wgpu::ShaderModule {
    let label = shader_label(label);
    let desc = wgpu::ShaderModuleDescriptor {
        label: Some(&label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    };
    device.create_shader_module(desc)
}

/// A shader module's label with the characters DXC reads as path syntax
/// replaced by a hyphen.
///
/// DXC takes a module's label as the path of the source file it compiles, so a
/// colon in one reads as a drive designator and a separator names a file below a
/// directory that does not exist: on Windows a label like `unlit_wgpu::lit`
/// fails to compile, and the error reports only that DXC could not read the
/// path. One hyphen per character keeps the label naming the same module. See
/// WORKAROUND.md for the bug behind this.
fn shader_label(label: &str) -> Cow<'_, str> {
    const PATH_SYNTAX: [char; 3] = [':', '/', '\\'];
    if !label.contains(PATH_SYNTAX) {
        return Cow::Borrowed(label);
    }
    Cow::Owned(label.replace(PATH_SYNTAX, "-"))
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

    /// An update rewrites the value and recomputes the word, so the record
    /// stays equal to one built fresh from the rewritten value.
    #[test]
    fn an_update_rehashes_the_rewritten_value() {
        let mut hashed = Hashed::new(1u32);
        hashed.update(|value| *value = 2);

        assert_eq!(hashed, Hashed::new(2u32));
        assert_eq!(hashed.hash(), FixedHasher::default().hash_one(2u32));
        assert_eq!(*hashed, 2);
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

    /// A colon reads as a drive designator to DXC and a separator as a
    /// directory, so both become a hyphen and the label keeps naming the same
    /// module.
    #[test]
    fn a_shader_label_loses_the_path_it_names() {
        assert_eq!(shader_label("unlit_wgpu::lit"), "unlit_wgpu--lit");
        assert_eq!(shader_label("test\\custom\\shader"), "test-custom-shader");
    }

    /// A label with no path syntax is handed on untouched rather than copied.
    #[test]
    fn a_shader_label_without_path_syntax_is_borrowed() {
        assert!(matches!(shader_label("flat"), Cow::Borrowed("flat")));
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
