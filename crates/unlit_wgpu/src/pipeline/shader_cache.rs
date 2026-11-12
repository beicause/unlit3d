use super::*;
use hashbrown::HashMap;
use smallvec::SmallVec;

/// The capacity a feature list is stored with: the widest flag set the built-in
/// families compose with.
///
/// It is a capacity hint rather than a limit — [`ShaderRecipe`] grows past it
/// when a caller composes with more — so the only thing it has to keep up
/// with is the largest list a family returns: the [`LitVariant`] `features`
/// list, which is declared with this same constant.
pub const FEATURE_COUNT: usize = 18;

/// The identity of one composed shader module: everything the WGSL depends on
/// and nothing else.
///
/// A pipeline's state — its blend, its cull mode, which surface it draws into —
/// changes nothing about the source a stage is compiled from. So the pipeline
/// key ([`PipelineVariant`]) is finer than
/// this one: many variants of one family compose the same module, and
/// [`ShaderCache`] builds it once for all of them.
///
/// The label is not part of the key. It names the module in a validation error
/// and on a capture, not the code it holds, so two keys that differ only in
/// label are one key; the label of whichever variant composed it first is the
/// one it keeps.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ShaderRecipe {
    /// The top-level module of the built-in WESL package composed, or the name
    /// a caller's stage module is mounted under.
    pub module: &'static str,
    /// The conditional-compilation flags the module composes with.
    pub features: SmallVec<[(&'static str, bool); FEATURE_COUNT]>,
    /// The caller's module mounted in place of the package's, if any.
    pub shader: Option<Shader>,
}

impl ShaderRecipe {
    /// The key of `module` composed with `features`, or, when `shader` is
    /// given, with that module in place of the package's copy.
    pub fn new(
        module: &'static str,
        features: &[(&'static str, bool)],
        shader: Option<&Shader>,
    ) -> Self {
        Self {
            module,
            features: features.iter().copied().collect(),
            shader: shader.cloned(),
        }
    }
}

impl core::fmt::Debug for ShaderRecipe {
    /// Prints the flags as the set they are, so a key reads in a dump the way
    /// it reads in the composition call.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut key = f.debug_struct("ShaderRecipe");
        key.field("module", &self.module);
        key.field(
            "features",
            &self
                .features
                .iter()
                .map(|(name, enabled)| format!("{name}={enabled}"))
                .collect::<Vec<_>>(),
        );
        key.field("shader", &self.shader);
        key.finish()
    }
}

/// The composed shader modules one holder is drawing with, keyed on the source
/// each was composed from.
///
/// Composing a WESL module is offline work — a resolver, a compiler, a
/// conditional-compilation pass — and building the module from the result is
/// another. Neither wgpu nor the pipeline cache pools the result, so a family
/// keyed on its whole pipeline state would compose afresh for a variant that
/// only turned off depth writes. Going through here instead, one `ShaderRecipe`
/// is composed once however many variants ask for it.
///
/// Who holds the cache decides how far that reaches. A
/// [`Variants`](crate::specialize::Variants) family
/// holds one, so every variant of that family shares its modules; a source that
/// builds pipelines outside a family — the UI, whose targets each bring their
/// own pipeline — holds one of its own across them. Two holders do not share
/// modules, so a module two of them both draw with is composed once per holder.
///
/// The cache never evicts: a family has finitely many variants, and their
/// modules are that many keys at most.
pub struct ShaderCache {
    /// Composed modules, by the source they were composed from.
    modules: HashMap<ShaderRecipe, wgpu::ShaderModule>,
}

impl ShaderCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    /// The module for `key`, composing it through `compose` on a miss.
    ///
    /// `compose` returns the module's WGSL and runs only on a miss; `label`
    /// names the module `key` is built from, which the caller knows and the key
    /// deliberately does not carry.
    pub fn get_or_create(
        &mut self,
        device: &wgpu::Device,
        key: &ShaderRecipe,
        label: &str,
        compose: impl FnOnce() -> String,
    ) -> wgpu::ShaderModule {
        if let Some(module) = self.modules.get(key) {
            return module.clone();
        }
        let module = crate::util::shader_module(device, &compose(), label);
        self.modules.insert(key.clone(), module.clone());
        module
    }

    /// The number of modules composed so far.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether no module has been composed yet.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    const MODULE: &str = "lit";
    /// The module the tests compose: minimal, so the device accepts it.
    const WGSL: &str = "@compute @workgroup_size(1) fn main() {}";

    /// A module of a label and a source, as the pipelines build one.
    fn shader(label: &str) -> Shader {
        Shader::new(label, "// a caller's module")
    }

    #[test]
    fn one_key_composes_once() {
        let (device, _queue) = crate::util::test::noop_device();
        let mut modules = ShaderCache::new();
        let composes = Cell::new(0);
        let key = ShaderRecipe::new(MODULE, &[("UNLIT", true)], None);
        let compose = || {
            composes.set(composes.get() + 1);
            WGSL.to_owned()
        };

        let first = modules.get_or_create(&device, &key, "first", compose);
        let second = modules.get_or_create(&device, &key, "second", compose);
        assert_eq!(composes.get(), 1, "the module is composed once");
        assert_eq!(first, second, "both calls return one module");
        assert_eq!(modules.len(), 1);
    }

    #[test]
    fn a_different_flag_set_composes_again() {
        let (device, _queue) = crate::util::test::noop_device();
        let mut modules = ShaderCache::new();
        let composes = Cell::new(0);
        let key = |unlit| ShaderRecipe::new(MODULE, &[("UNLIT", unlit)], None);
        let compose = || {
            composes.set(composes.get() + 1);
            WGSL.to_owned()
        };

        modules.get_or_create(&device, &key(true), "lit", compose);
        modules.get_or_create(&device, &key(false), "lit", compose);
        assert_eq!(composes.get(), 2, "each flag set is its own module");
        assert_eq!(modules.len(), 2);
    }

    #[test]
    fn a_different_shader_composes_again() {
        let (device, _queue) = crate::util::test::noop_device();
        let mut modules = ShaderCache::new();
        let composes = Cell::new(0);
        let compose = || {
            composes.set(composes.get() + 1);
            WGSL.to_owned()
        };

        let built_in = ShaderRecipe::new(MODULE, &[], None);
        let replaced = ShaderRecipe::new(MODULE, &[], Some(&shader("caller")));
        modules.get_or_create(&device, &built_in, "lit", compose);
        modules.get_or_create(&device, &replaced, "caller", compose);
        assert_eq!(composes.get(), 2, "a replaced module is its own");
        assert_eq!(modules.len(), 2);
    }

    #[test]
    fn a_label_alone_does_not_compose_again() {
        let (device, _queue) = crate::util::test::noop_device();
        let mut modules = ShaderCache::new();
        let composes = Cell::new(0);
        let key = ShaderRecipe::new(MODULE, &[], None);
        let compose = || {
            composes.set(composes.get() + 1);
            WGSL.to_owned()
        };

        modules.get_or_create(&device, &key, "unlit_wgpu::lit", compose);
        modules.get_or_create(&device, &key, "unlit3d::ui", compose);
        assert_eq!(
            composes.get(),
            1,
            "the label names the module, not its code"
        );
        assert_eq!(modules.len(), 1);
    }

    #[test]
    fn two_shaders_of_equal_source_are_two_keys() {
        assert_ne!(
            ShaderRecipe::new(MODULE, &[], Some(&shader("one"))),
            ShaderRecipe::new(MODULE, &[], Some(&shader("two"))),
            "a shader's label is part of the variant it belongs to"
        );
    }
}
