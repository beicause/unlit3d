//! Variant caching for pipeline-like values.
//!
//! A [Specializable] value -- a compiled render pipeline, say -- is expensive
//! to build and only valid for one exact configuration. A [Specializer]
//! takes a small key that names one configuration, rewrites a blueprint (the
//! value's descriptor) for that key, and reports the key's canonical form.
//! [Variants] ties the two together: it creates the value the first time a
//! key is asked for and hands the same index back afterwards. The blueprint
//! is supplied on each call rather than stored, so one cache can serve keys
//! that each carry their own base, and a cache hit does not clone one.
//!
//! # The two-level cache
//!
//! A key is not always injective. A key may carry information that does not
//! reach the descriptor -- the raw vertex attributes of a mesh, say, of which
//! only the formats the shader reads matter. Two such keys must share one
//! compiled value, so [Variants] keeps two maps:
//!
//! - the primary map from the caller's key to a variant index, and
//! - the canonical map from the key's canonical form to a variant index.
//!
//! When [SpecializerKey::IS_CANONICAL] is true the key is injective, the
//! canonical map is never consulted, and the primary map alone is the cache.
//! When it is false, a primary miss consults the canonical map: on a hit the
//! variant is reused, and in debug builds the cached descriptor is checked
//! against the freshly specialized one, which catches a specializer that
//! derived different descriptors from one canonical key.
//!
//! There is no full-descriptor cache. A [RenderPipelineDesc] cannot be hashed
//! -- its compilation constants hold f64 -- and memoizing on a small key is
//! the point of the type: a family has finitely many keys, and a lookup is
//! cheaper than comparing whole descriptors. Cached variants are never
//! evicted, matching the bounded-key assumption.
//!
//! # Canonical keys
//!
//! Keys that are injective implement [SpecializerKey] with
//! [SpecializerKey::IS_CANONICAL] set to true. The simplest such keys -- plain
//! hashable types -- are declared with the
//! [`impl_canonical_specializer_key!`](crate::impl_canonical_specializer_key) macro, which routes
//! [canonical_specializer_key] through the same place as the hand-written
//! implementations. A [SurfaceKey] is one of these. Multiple orthogonal
//! specialization dimensions compose as a tuple of keys, for which
//! [SpecializerKey] is implemented up to arity eight.

use core::hash::Hash;
use core::marker::PhantomData;

use hashbrown::HashMap;
use smallvec::SmallVec;

use crate::render_attachments::RenderAttachments;

/// A type that can be compiled from a descriptor and cached one-per-key.
pub trait Specializable: Sized {
    /// The blueprint a specializer rewrites. Must be comparable so a cached
    /// variant can be checked against a freshly specialized one.
    ///
    /// Deliberately not `Send`/`Sync`: a descriptor holds wgpu handles, which
    /// are not thread-safe on the web. A cache and the device it creates
    /// against live together and never cross threads, so requiring it would
    /// only rule the web backend out.
    type Descriptor: Clone + PartialEq;

    /// Compile the descriptor into a value.
    fn create(device: &wgpu::Device, descriptor: &Self::Descriptor) -> Self;

    /// The descriptor this value was created from.
    fn descriptor(&self) -> &Self::Descriptor;
}

/// A pure function from a small key to a descriptor rewrite.
pub trait Specializer<T: Specializable>: 'static {
    /// The key that names one configuration.
    type Key: SpecializerKey;

    /// Rewrite the descriptor for the key and return the key in canonical
    /// form.
    fn specialize(&self, key: Self::Key, descriptor: &mut T::Descriptor) -> Canonical<Self::Key>;
}

/// A key a [Specializer] accepts.
pub trait SpecializerKey: Clone + Hash + Eq {
    /// True iff [Self::Canonical] = Self, i.e. distinct keys always mean
    /// distinct descriptors and the secondary cache can be skipped.
    const IS_CANONICAL: bool;

    /// The part of the key that actually reaches the descriptor. Two keys with
    /// equal canonical forms map to the same variant.
    type Canonical: Hash + Eq;
}

/// The canonical form of a [SpecializerKey].
pub type Canonical<T> = <T as SpecializerKey>::Canonical;

/// Declare a key whose distinct values always produce distinct descriptors, so
/// the secondary cache can be skipped.
///
/// Used by the [`impl_canonical_specializer_key!`](crate::impl_canonical_specializer_key) macro; prefer the macro for
/// readability.
pub const fn canonical_specializer_key<T>() -> (bool, PhantomData<T>) {
    (true, PhantomData)
}

/// Implement [SpecializerKey] for the listed types as canonical keys: every
/// distinct value produces a distinct descriptor, so [Variants] skips the
/// secondary cache.
///
///     use unlit_wgpu::impl_canonical_specializer_key;
///
///     #[derive(Clone, Copy, PartialEq, Eq, Hash)]
///     struct MaterialId(u32);
///
///     impl_canonical_specializer_key!(MaterialId);
#[macro_export]
macro_rules! impl_canonical_specializer_key {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl $crate::specialize::SpecializerKey for $ty {
                const IS_CANONICAL: bool =
                    $crate::specialize::canonical_specializer_key::<$ty>().0;
                type Canonical = $ty;
            }
        )+
    };
}

macro_rules! impl_specializer_key_tuple {
    () => {
        impl SpecializerKey for () {
            const IS_CANONICAL: bool = true;
            type Canonical = ();
        }
    };
    ($($name:ident),+ $(,)?) => {
        impl<$($name: SpecializerKey),+> SpecializerKey for ($($name,)+) {
            const IS_CANONICAL: bool = true $(&& $name::IS_CANONICAL)+;
            type Canonical = ($(Canonical<$name>,)+);
        }
    };
}

impl_specializer_key_tuple!();
impl_specializer_key_tuple!(A);
impl_specializer_key_tuple!(A, B);
impl_specializer_key_tuple!(A, B, C);
impl_specializer_key_tuple!(A, B, C, D);
impl_specializer_key_tuple!(A, B, C, D, E);
impl_specializer_key_tuple!(A, B, C, D, E, F);
impl_specializer_key_tuple!(A, B, C, D, E, F, G);
impl_specializer_key_tuple!(A, B, C, D, E, F, G, H);

/// A cache for variants of a [Specializable] type. At most one value is
/// created per key.
///
/// The variants are stored in creation order; [Self::specialize] returns their
/// index. The cache never evicts: keys are assumed bounded, so a family that
/// specializes on the mesh's vertex layout, say, holds one variant per
/// distinct layout it has seen.
pub struct Variants<T: Specializable, S: Specializer<T>> {
    /// The device variants are created on.
    device: wgpu::Device,
    /// The rewrite applied to every key.
    specializer: S,
    /// The primary cache: caller key to variant index.
    primary: HashMap<S::Key, u32>,
    /// The canonical cache: canonical key to variant index.
    canonical: HashMap<Canonical<S::Key>, u32>,
    /// The created variants, indexed by the returned variant.
    variants: Vec<T>,
}

impl<T: Specializable, S: Specializer<T>> Variants<T, S> {
    /// Create an empty cache.
    pub fn new(device: &wgpu::Device, specializer: S) -> Self {
        Self {
            device: device.clone(),
            specializer,
            primary: HashMap::new(),
            canonical: HashMap::new(),
            variants: Vec::new(),
        }
    }

    /// The variant for the key, creating it on first use.
    ///
    /// Creates at most one value per key. For a non-canonical key, keys that
    /// share a canonical form share a variant. `base` supplies the blueprint
    /// the key is specialized from and is called only on a cache miss, so a
    /// hit never clones one.
    pub fn specialize(&mut self, base: impl FnOnce() -> T::Descriptor, key: S::Key) -> u32 {
        if let Some(&index) = self.primary.get(&key) {
            return index;
        }

        let mut descriptor = base();
        let canonical_key = self.specializer.specialize(key.clone(), &mut descriptor);

        let index = if S::Key::IS_CANONICAL {
            self.create_variant(&descriptor)
        } else if let Some(&index) = self.canonical.get(&canonical_key) {
            debug_assert!(
                T::descriptor(&self.variants[index as usize]) == &descriptor,
                "a specializer produced descriptors that differ for one canonical key; \
                 the key must not carry information the descriptor does not depend on"
            );
            index
        } else {
            let index = self.create_variant(&descriptor);
            self.canonical.insert(canonical_key, index);
            index
        };

        self.primary.insert(key, index);
        index
    }

    /// The already-cached variant for the key, if any. Does not create.
    pub fn lookup(&self, key: &S::Key) -> Option<u32> {
        self.primary.get(key).copied()
    }

    /// The variant at the given index.
    ///
    /// # Panics
    /// If the index was not returned by [Self::specialize].
    pub fn get(&self, variant: u32) -> &T {
        &self.variants[variant as usize]
    }

    /// The number of created variants.
    pub fn len(&self) -> usize {
        self.variants.len()
    }

    /// Whether no variant has been created yet.
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty()
    }

    /// Create a variant from the descriptor and return its index.
    fn create_variant(&mut self, descriptor: &T::Descriptor) -> u32 {
        let variant = T::create(&self.device, descriptor);
        let index = u32::try_from(self.variants.len()).expect("variant count fits in u32");
        self.variants.push(variant);
        index
    }
}

/// The attributes of one vertex-buffer layout.
///
/// A vertex buffer carries a handful of attributes in practice, so the inline
/// capacity keeps the ordinary layout free of a heap allocation.
pub type VertexAttributes = SmallVec<[wgpu::VertexAttribute; 8]>;

/// An owned vertex-buffer layout. The attributes are owned rather than
/// borrowed, so the layout outlives the descriptor it was read from.
///
/// Eq + Hash (beyond the descriptor's own PartialEq) so it can be part of a
/// specializer key, which is what lets a family re-specialize when a mesh's
/// vertex layout changes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VertexBufferLayoutDesc {
    /// The stride, in bytes, between elements of this buffer.
    pub array_stride: wgpu::BufferAddress,
    /// How often this vertex buffer is stepped forward.
    pub step_mode: wgpu::VertexStepMode,
    /// The attributes that make up one element of this buffer.
    pub attributes: VertexAttributes,
}

impl VertexBufferLayoutDesc {
    /// Own a borrowed layout, cloning its attributes.
    pub fn from_wgpu(layout: &wgpu::VertexBufferLayout<'_>) -> Self {
        Self {
            array_stride: layout.array_stride,
            step_mode: layout.step_mode,
            attributes: layout.attributes.into(),
        }
    }

    /// Borrow this layout as a wgpu one, for the duration of a pipeline
    /// creation.
    pub(crate) fn as_wgpu(&self) -> wgpu::VertexBufferLayout<'_> {
        wgpu::VertexBufferLayout {
            array_stride: self.array_stride,
            step_mode: self.step_mode,
            attributes: &self.attributes,
        }
    }
}

/// Owned pipeline compilation options.
#[derive(Clone, Debug, PartialEq)]
pub struct CompilationOptions {
    /// Values of the shader's pipeline-overridable constants.
    pub constants: Vec<(String, f64)>,
    /// Whether workgroup-scoped memory is zero-initialized for this stage.
    pub zero_initialize_workgroup_memory: bool,
}

impl CompilationOptions {
    /// Own a borrowed set of compilation options.
    fn from_wgpu(options: &wgpu::PipelineCompilationOptions<'_>) -> Self {
        Self {
            constants: options
                .constants
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
            zero_initialize_workgroup_memory: options.zero_initialize_workgroup_memory,
        }
    }
}

impl Default for CompilationOptions {
    fn default() -> Self {
        Self {
            constants: Vec::new(),
            zero_initialize_workgroup_memory: true,
        }
    }
}

/// Owned vertex stage of a [RenderPipelineDesc].
#[derive(Clone, Debug, PartialEq)]
pub struct VertexStateDesc {
    /// The compiled shader module for this stage.
    pub module: wgpu::ShaderModule,
    /// The name of the entry point to use, or the module's only one.
    pub entry_point: Option<String>,
    /// Advanced options for when this stage is compiled.
    pub compilation_options: CompilationOptions,
    /// The vertex buffers this stage reads.
    pub buffers: Vec<Option<VertexBufferLayoutDesc>>,
}

impl VertexStateDesc {
    /// Own a borrowed vertex state.
    fn from_wgpu(state: &wgpu::VertexState<'_>) -> Self {
        Self {
            module: state.module.clone(),
            entry_point: state.entry_point.map(str::to_owned),
            compilation_options: CompilationOptions::from_wgpu(&state.compilation_options),
            buffers: state
                .buffers
                .iter()
                .map(|buffer| buffer.as_ref().map(VertexBufferLayoutDesc::from_wgpu))
                .collect(),
        }
    }
}

/// Owned fragment stage of a [RenderPipelineDesc].
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentStateDesc {
    /// The compiled shader module for this stage.
    pub module: wgpu::ShaderModule,
    /// The name of the entry point to use, or the module's only one.
    pub entry_point: Option<String>,
    /// Advanced options for when this stage is compiled.
    pub compilation_options: CompilationOptions,
    /// The color targets this stage writes.
    pub targets: Vec<Option<wgpu::ColorTargetState>>,
}

impl FragmentStateDesc {
    /// Own a borrowed fragment state.
    fn from_wgpu(state: &wgpu::FragmentState<'_>) -> Self {
        Self {
            module: state.module.clone(),
            entry_point: state.entry_point.map(str::to_owned),
            compilation_options: CompilationOptions::from_wgpu(&state.compilation_options),
            targets: state.targets.to_vec(),
        }
    }
}

/// An owned mirror of wgpu's render-pipeline descriptor, with every borrowed
/// slice owned so it can outlive the descriptor it was read from.
///
/// A specializer rewrites these fields in place, and [CachedRenderPipeline]
/// stores the result alongside the compiled pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderPipelineDesc {
    /// Debug label of the pipeline.
    pub label: Option<String>,
    /// The bind group layouts this pipeline uses.
    pub layout: Option<wgpu::PipelineLayout>,
    /// The vertex stage.
    pub vertex: VertexStateDesc,
    /// Primitive assembly and rasterization state.
    pub primitive: wgpu::PrimitiveState,
    /// Depth-stencil state, if the pass has a depth attachment.
    pub depth_stencil: Option<wgpu::DepthStencilState>,
    /// Multisampling state.
    pub multisample: wgpu::MultisampleState,
    /// The fragment stage, if the pipeline writes color.
    pub fragment: Option<FragmentStateDesc>,
    /// The multiview mask, if the pipeline renders with multiview.
    pub multiview_mask: Option<core::num::NonZeroU32>,
    /// The pipeline cache to compile against.
    pub cache: Option<wgpu::PipelineCache>,
}

impl RenderPipelineDesc {
    /// Own a borrowed render-pipeline descriptor, cloning every slice and
    /// handle.
    pub fn from_wgpu(descriptor: &wgpu::RenderPipelineDescriptor<'_>) -> Self {
        Self {
            label: descriptor.label.map(str::to_owned),
            layout: descriptor.layout.cloned(),
            vertex: VertexStateDesc::from_wgpu(&descriptor.vertex),
            primitive: descriptor.primitive,
            depth_stencil: descriptor.depth_stencil.clone(),
            multisample: descriptor.multisample,
            fragment: descriptor
                .fragment
                .as_ref()
                .map(FragmentStateDesc::from_wgpu),
            multiview_mask: descriptor.multiview_mask,
            cache: descriptor.cache.cloned(),
        }
    }

    /// Create the pipeline this descriptor describes.
    ///
    /// The borrowed wgpu descriptor is assembled and consumed inside this
    /// function, so the owned vecs it borrows never outlive the call.
    pub fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
        let buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = self
            .vertex
            .buffers
            .iter()
            .map(|buffer| buffer.as_ref().map(VertexBufferLayoutDesc::as_wgpu))
            .collect();
        let vertex_constants: Vec<(&str, f64)> = self
            .vertex
            .compilation_options
            .constants
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect();
        let fragment_constants: Vec<(&str, f64)> = self
            .fragment
            .as_ref()
            .map(|fragment| {
                fragment
                    .compilation_options
                    .constants
                    .iter()
                    .map(|(name, value)| (name.as_str(), *value))
                    .collect()
            })
            .unwrap_or_default();

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: self.label.as_deref(),
            layout: self.layout.as_ref(),
            vertex: wgpu::VertexState {
                module: &self.vertex.module,
                entry_point: self.vertex.entry_point.as_deref(),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &vertex_constants,
                    zero_initialize_workgroup_memory: self
                        .vertex
                        .compilation_options
                        .zero_initialize_workgroup_memory,
                },
                buffers: &buffers,
            },
            primitive: self.primitive,
            depth_stencil: self.depth_stencil.clone(),
            multisample: self.multisample,
            fragment: self.fragment.as_ref().map(|fragment| wgpu::FragmentState {
                module: &fragment.module,
                entry_point: fragment.entry_point.as_deref(),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &fragment_constants,
                    zero_initialize_workgroup_memory: fragment
                        .compilation_options
                        .zero_initialize_workgroup_memory,
                },
                targets: &fragment.targets,
            }),
            multiview_mask: self.multiview_mask,
            cache: self.cache.as_ref(),
        })
    }
}

/// A compiled pipeline together with the descriptor it came from.
#[derive(Clone, Debug)]
pub struct CachedRenderPipeline {
    /// The compiled pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// The descriptor it was compiled from.
    pub descriptor: RenderPipelineDesc,
}

impl Specializable for CachedRenderPipeline {
    type Descriptor = RenderPipelineDesc;

    fn create(device: &wgpu::Device, descriptor: &Self::Descriptor) -> Self {
        Self {
            pipeline: descriptor.create(device),
            descriptor: descriptor.clone(),
        }
    }

    fn descriptor(&self) -> &Self::Descriptor {
        &self.descriptor
    }
}

/// The render target a pipeline is specialized for.
///
/// Width and height are excluded: a pipeline does not depend on the size of
/// the target it draws into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    /// The format of the color attachment.
    pub color_format: wgpu::TextureFormat,
    /// The format of the depth-stencil attachment, if the pass has one.
    pub depth_stencil_format: Option<wgpu::TextureFormat>,
    /// The sample count the pass renders with.
    pub sample_count: u32,
}

impl SurfaceKey {
    /// The surface a set of attachments describes.
    ///
    /// The color format and sample count are read from the same attachment
    /// path, so they stay consistent: the MSAA view when one is set, the color
    /// view otherwise. A depth-only attachment set has no color view, so its
    /// key reports Rgba8UnormSrgb as the color format; a depth-only pipeline
    /// is the caller's to build.
    pub fn from_attachments(attachments: &RenderAttachments) -> Self {
        Self {
            color_format: attachments
                .color_format()
                .unwrap_or(wgpu::TextureFormat::Rgba8UnormSrgb),
            depth_stencil_format: attachments.depth_stencil_format(),
            sample_count: attachments.sample_count(),
        }
    }
}

impl SpecializerKey for SurfaceKey {
    // Every field lands in the descriptor, so distinct keys are distinct
    // descriptors and the secondary cache is never consulted.
    const IS_CANONICAL: bool = true;
    type Canonical = Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blueprint a specializer rewrites.
    #[derive(Clone, Debug, PartialEq)]
    struct TestDescriptor {
        /// The value every variant starts from.
        base: u32,
        /// The value the specializer writes.
        rewritten: u32,
    }

    /// A variant that stores the descriptor it was created from.
    struct TestVariant {
        descriptor: TestDescriptor,
    }

    impl Specializable for TestVariant {
        type Descriptor = TestDescriptor;

        fn create(_device: &wgpu::Device, descriptor: &Self::Descriptor) -> Self {
            Self {
                descriptor: descriptor.clone(),
            }
        }

        fn descriptor(&self) -> &Self::Descriptor {
            &self.descriptor
        }
    }

    /// An injective key: its id is exactly what reaches the descriptor.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct TestKey {
        id: u32,
    }

    impl SpecializerKey for TestKey {
        const IS_CANONICAL: bool = true;
        type Canonical = u32;
    }

    /// A key that is not injective: only its canonical form reaches the
    /// descriptor.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct NonCanonicalKey {
        raw: u32,
        canonical: u32,
    }

    impl SpecializerKey for NonCanonicalKey {
        const IS_CANONICAL: bool = false;
        type Canonical = u32;
    }

    /// Writes the key's id into the descriptor.
    struct TestSpecializer;

    impl Specializer<TestVariant> for TestSpecializer {
        type Key = TestKey;

        fn specialize(&self, key: TestKey, descriptor: &mut TestDescriptor) -> u32 {
            descriptor.rewritten = key.id;
            key.id
        }
    }

    /// Writes the key's canonical form into the descriptor.
    struct CanonicalizingSpecializer;

    impl Specializer<TestVariant> for CanonicalizingSpecializer {
        type Key = NonCanonicalKey;

        fn specialize(&self, key: NonCanonicalKey, descriptor: &mut TestDescriptor) -> u32 {
            descriptor.rewritten = key.canonical;
            key.canonical
        }
    }

    /// Writes the key's raw value into the descriptor while reporting the
    /// canonical one: two keys with one canonical form disagree.
    struct LiarSpecializer;

    impl Specializer<TestVariant> for LiarSpecializer {
        type Key = NonCanonicalKey;

        fn specialize(&self, key: NonCanonicalKey, descriptor: &mut TestDescriptor) -> u32 {
            descriptor.rewritten = key.raw;
            key.canonical
        }
    }

    fn base() -> TestDescriptor {
        TestDescriptor {
            base: 7,
            rewritten: 0,
        }
    }

    fn variants<S: Specializer<TestVariant>>(specializer: S) -> Variants<TestVariant, S> {
        let (device, _queue) = crate::util::test::noop_device();
        Variants::new(&device, specializer)
    }

    #[test]
    fn variant_is_created_once_per_key() {
        let mut variants = variants(TestSpecializer);

        let first = variants.specialize(base, TestKey { id: 1 });
        let again = variants.specialize(base, TestKey { id: 1 });
        assert_eq!(first, again, "the same key reuses its variant");
        assert_eq!(variants.len(), 1);

        let second = variants.specialize(base, TestKey { id: 2 });
        assert_ne!(first, second, "a different key gets a new variant");
        assert_eq!(variants.len(), 2);
    }

    #[test]
    fn lookup_does_not_create() {
        let mut variants = variants(TestSpecializer);

        assert_eq!(variants.lookup(&TestKey { id: 1 }), None);
        assert_eq!(variants.len(), 0, "a lookup miss creates nothing");

        let index = variants.specialize(base, TestKey { id: 1 });
        assert_eq!(variants.lookup(&TestKey { id: 1 }), Some(index));
        assert_eq!(variants.lookup(&TestKey { id: 2 }), None);
        assert_eq!(variants.len(), 1);
    }

    #[test]
    fn variants_do_not_mutate_the_base() {
        let mut variants = variants(TestSpecializer);

        let first = variants.specialize(base, TestKey { id: 1 });
        let second = variants.specialize(base, TestKey { id: 2 });

        // Each create saw base plus its own key, not the previous rewrite.
        assert_eq!(
            variants.get(first).descriptor(),
            &TestDescriptor {
                base: 7,
                rewritten: 1,
            }
        );
        assert_eq!(
            variants.get(second).descriptor(),
            &TestDescriptor {
                base: 7,
                rewritten: 2,
            }
        );
    }

    #[test]
    fn non_canonical_keys_share_a_canonical_variant() {
        let mut variants = variants(CanonicalizingSpecializer);

        let first = variants.specialize(
            base,
            NonCanonicalKey {
                raw: 1,
                canonical: 9,
            },
        );
        let second = variants.specialize(
            base,
            NonCanonicalKey {
                raw: 2,
                canonical: 9,
            },
        );

        assert_eq!(first, second, "one canonical form, one variant");
        assert_eq!(variants.len(), 1);

        let third = variants.specialize(
            base,
            NonCanonicalKey {
                raw: 3,
                canonical: 10,
            },
        );
        assert_ne!(first, third, "a new canonical form gets a new variant");
        assert_eq!(variants.len(), 2);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "differ for one canonical key")]
    fn canonical_variant_is_checked_against_the_descriptor() {
        let mut variants = variants(LiarSpecializer);

        variants.specialize(
            base,
            NonCanonicalKey {
                raw: 1,
                canonical: 5,
            },
        );
        variants.specialize(
            base,
            NonCanonicalKey {
                raw: 2,
                canonical: 5,
            },
        );
    }

    #[test]
    fn variant_is_created_once_per_canonical_key() {
        let mut variants = variants(TestSpecializer);

        let first = variants.specialize(base, TestKey { id: 3 });
        let again = variants.specialize(base, TestKey { id: 3 });
        let other = variants.specialize(base, TestKey { id: 4 });

        assert_eq!(first, again);
        assert_ne!(first, other);
        assert_eq!(variants.len(), 2);
    }

    const MINIMAL_WGSL: &str = "\
@vertex
fn vs_main() -> @builtin(position) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
";

    /// A pipeline descriptor borrowing its module and targets.
    fn source_descriptor<'a>(
        module: &'a wgpu::ShaderModule,
        targets: &'a [Option<wgpu::ColorTargetState>],
    ) -> wgpu::RenderPipelineDescriptor<'a> {
        wgpu::RenderPipelineDescriptor {
            label: Some("test"),
            layout: None,
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets,
            }),
            multiview_mask: None,
            cache: None,
        }
    }

    #[test]
    fn render_pipeline_desc_survives_its_source() {
        let (device, _queue) = crate::util::test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test::shader"),
            source: wgpu::ShaderSource::Wgsl(MINIMAL_WGSL.into()),
        });

        let owned = {
            let targets = [Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })];
            let source = source_descriptor(&module, &targets);
            RenderPipelineDesc::from_wgpu(&source)
        };

        // The source descriptor and the slices it borrowed are gone; the owned
        // descriptor still creates a pipeline.
        let _pipeline = owned.create(&device);
    }

    #[test]
    fn vertex_buffer_layout_desc_round_trips() {
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 12,
                shader_location: 1,
            },
        ];
        let layout = wgpu::VertexBufferLayout {
            array_stride: 16,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        };

        let owned = VertexBufferLayoutDesc::from_wgpu(&layout);
        let round_tripped = owned.as_wgpu();

        assert_eq!(round_tripped.array_stride, layout.array_stride);
        assert_eq!(round_tripped.step_mode, layout.step_mode);
        assert_eq!(round_tripped.attributes, layout.attributes);
    }

    /// A specializer that leaves the descriptor alone.
    struct IdentitySpecializer;

    impl Specializer<CachedRenderPipeline> for IdentitySpecializer {
        type Key = ();

        fn specialize(&self, key: (), _descriptor: &mut RenderPipelineDesc) {
            key
        }
    }

    #[test]
    fn a_render_pipeline_variant_is_cached() {
        let (device, _queue) = crate::util::test::noop_device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test::shader"),
            source: wgpu::ShaderSource::Wgsl(MINIMAL_WGSL.into()),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let source = source_descriptor(&module, &targets);
        let descriptor = RenderPipelineDesc::from_wgpu(&source);
        let mut variants = Variants::new(&device, IdentitySpecializer);

        let first = variants.specialize(|| descriptor.clone(), ());
        let again = variants.specialize(|| descriptor.clone(), ());
        assert_eq!(first, again);
        assert_eq!(variants.len(), 1);
    }
}
