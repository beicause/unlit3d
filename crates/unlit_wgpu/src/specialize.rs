//! Variant caching for pipeline-like values.
//!
//! Three types carry it:
//!
//! - a [SpecializerKey] names one configuration,
//! - a [Specializer] rewrites a blueprint (a [PipelineDescriptor]) for a key
//!   and reports the key's canonical form, and
//! - [Variants] caches the [SpecializedPipeline]s that result, handing the same
//!   index back the next time a key is asked for.
//!
//! The blueprint is supplied on each call rather than stored, so one cache can
//! serve keys that each carry their own base, and a cache hit does not clone
//! one.
//!
//! [PipelineDescriptor] is generic over the pipeline it compiles, so one cache
//! serves a render pipeline and a compute pipeline alike: the descriptor is the
//! only thing that knows which of the two it builds.
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
//! A key that is injective sets [SpecializerKey::IS_CANONICAL] to true and its
//! [SpecializerKey::Canonical] to itself: [SurfaceKey], the render target a
//! pipeline is built for, is one such key. A key that carries information no
//! descriptor depends on keeps the two apart, as the built-in unlit family does
//! for a mesh's raw vertex attributes.

use core::hash::Hash;
use std::sync::Arc;

use arrayvec::ArrayVec;
use hashbrown::HashMap;
use smallvec::SmallVec;

use crate::render_attachments::RenderAttachments;
use crate::scene::MAX_VERTEX_BUFFERS;
use crate::util::Hashed;

/// A blueprint that compiles into a pipeline of type `P`.
///
/// A specializer rewrites one of these in place, and [Variants] caches the
/// result. Comparable so a cached variant can be checked against a freshly
/// specialized one.
///
/// Deliberately not `Send`/`Sync`: a descriptor holds wgpu handles, which are
/// not thread-safe on the web. A cache and the device it creates against live
/// together and never cross threads, so requiring it would only rule the web
/// backend out.
pub trait PipelineDescriptor<P>: Clone + PartialEq {
    /// Compile this blueprint into a pipeline.
    fn create(&self, device: &wgpu::Device) -> P;
}

/// A compiled pipeline together with the descriptor it came from.
///
/// This is what a cache hands out: the pipeline to bind, and the descriptor it
/// was built from, which the renderer reads to discover the bind-group layouts
/// and vertex streams the pipeline expects.
#[derive(Clone, Debug)]
pub struct SpecializedPipeline<P, D> {
    /// The compiled pipeline.
    pub pipeline: P,
    /// The descriptor it was compiled from.
    pub descriptor: D,
}

impl<P, D: PipelineDescriptor<P>> SpecializedPipeline<P, D> {
    /// Compile `descriptor` into a pipeline.
    pub fn create(device: &wgpu::Device, descriptor: D) -> Self {
        Self {
            pipeline: descriptor.create(device),
            descriptor,
        }
    }

    /// The descriptor this pipeline was compiled from.
    pub fn descriptor(&self) -> &D {
        &self.descriptor
    }
}

/// A pure function from a small key to a descriptor rewrite.
pub trait Specializer<D>: 'static {
    /// The key that names one configuration.
    type Key: SpecializerKey;

    /// Rewrite the descriptor for the key and return the key in canonical
    /// form.
    fn specialize(&self, key: Self::Key, descriptor: &mut D) -> Canonical<Self::Key>;
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

/// A cache of the [SpecializedPipeline]s one descriptor family compiles. At
/// most one pipeline is created per key.
///
/// The variants are stored in creation order; [Self::specialize] returns their
/// index. The cache never evicts: keys are assumed bounded, so a family that
/// specializes on the mesh's vertex layout, say, holds one variant per
/// distinct layout it has seen.
pub struct Variants<P, D: PipelineDescriptor<P>, S: Specializer<D>> {
    /// The device variants are created on.
    device: wgpu::Device,
    /// The rewrite applied to every key.
    specializer: S,
    /// The primary cache: caller key to variant index.
    primary: HashMap<S::Key, u32>,
    /// The canonical cache: canonical key to variant index.
    canonical: HashMap<Canonical<S::Key>, u32>,
    /// The created variants, indexed by the returned variant.
    variants: Vec<SpecializedPipeline<P, D>>,
}

impl<P, D: PipelineDescriptor<P>, S: Specializer<D>> Variants<P, D, S> {
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
    pub fn specialize(&mut self, base: impl FnOnce() -> D, key: S::Key) -> u32 {
        if let Some(&index) = self.primary.get(&key) {
            return index;
        }

        let mut descriptor = base();
        let canonical_key = self.specializer.specialize(key.clone(), &mut descriptor);

        let index = if S::Key::IS_CANONICAL {
            self.create_variant(descriptor)
        } else if let Some(&index) = self.canonical.get(&canonical_key) {
            debug_assert!(
                self.variants[index as usize].descriptor() == &descriptor,
                "a specializer produced descriptors that differ for one canonical key; \
                 the key must not carry information the descriptor does not depend on"
            );
            index
        } else {
            let index = self.create_variant(descriptor);
            self.canonical.insert(canonical_key, index);
            index
        };

        self.primary.insert(key, index);
        index
    }

    /// The variant at the given index.
    ///
    /// # Panics
    /// If the index was not returned by [Self::specialize].
    pub fn get(&self, variant: u32) -> &SpecializedPipeline<P, D> {
        &self.variants[variant as usize]
    }

    /// Create a variant from the descriptor and return its index.
    fn create_variant(&mut self, descriptor: D) -> u32 {
        let variant = SpecializedPipeline::create(&self.device, descriptor);
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

/// A mesh's vertex-buffer layout, slot by slot, shared rather than copied.
///
/// The layout is part of the key a family specializes on, and that key is built
/// and hashed once per visible entity per frame. A mesh's layout never changes
/// after upload, so it is interned behind an [`Arc`] with its hash taken once by
/// [`Hashed`]: cloning a key copies a pointer, hashing it writes one word, and
/// comparing two keys settles on that word before it looks at an attribute.
///
/// Equality is by the buffers themselves — the shared-allocation fast path
/// aside — so two independently uploaded meshes with the same layout are one
/// key; equal layouts always carry equal hashes, which is what lets [`Hash`] be
/// the stored word.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VertexLayout {
    /// The interned buffers and their hash.
    ///
    /// [`Hashed`] sits inside the [`Arc`] rather than outside it: hashing a
    /// `Hashed<Arc<_>>` would forward through the `Arc` to the buffers and walk
    /// every attribute, which is the cost this type exists to avoid.
    inner: Arc<Hashed<ArrayVec<(u32, VertexBufferLayoutDesc), MAX_VERTEX_BUFFERS>>>,
}

impl VertexLayout {
    /// Intern `buffers`, slot by slot.
    ///
    /// # Panics
    ///
    /// If more than [MAX_VERTEX_BUFFERS] buffers are given: a draw cannot bind
    /// more than that.
    pub fn new(buffers: impl IntoIterator<Item = (u32, VertexBufferLayoutDesc)>) -> Self {
        let buffers: ArrayVec<_, MAX_VERTEX_BUFFERS> = buffers.into_iter().collect();
        Self {
            inner: Arc::new(Hashed::new(buffers)),
        }
    }
}

impl Default for VertexLayout {
    /// A layout naming no buffer.
    fn default() -> Self {
        Self::new([])
    }
}

impl core::ops::Deref for VertexLayout {
    type Target = [(u32, VertexBufferLayoutDesc)];

    fn deref(&self) -> &Self::Target {
        &self.inner
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
/// A specializer rewrites these fields in place, and a
/// [SpecializedPipeline] stores the result alongside the pipeline it compiles
/// into.
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
}

impl PipelineDescriptor<wgpu::RenderPipeline> for RenderPipelineDesc {
    /// Create the pipeline this descriptor describes.
    ///
    /// The borrowed wgpu descriptor is assembled and consumed inside this
    /// function, so the owned vecs it borrows never outlive the call.
    fn create(&self, device: &wgpu::Device) -> wgpu::RenderPipeline {
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

/// A descriptor whose pipeline is only valid for one render target.
///
/// A pipeline's color format, sample count and depth-stencil format must match
/// the attachments its pass renders into, so a descriptor that names them has
/// to be rewritten when the target changes. Implementing this is what lets a
/// descriptor be specialized by [SurfaceSpecializer], whichever kind of
/// pipeline it compiles.
pub trait SurfaceTarget {
    /// Rewrite this descriptor's target-dependent fields for `surface`.
    fn set_surface(&mut self, surface: SurfaceKey);
}

/// Specializes any [SurfaceTarget] descriptor for a render target.
///
/// The descriptor decides which fields the target reaches; this only supplies
/// the key and its canonical form, so one specializer serves every kind of
/// pipeline that draws into an attachment.
#[derive(Clone, Copy, Debug, Default)]
pub struct SurfaceSpecializer;

impl<D: SurfaceTarget> Specializer<D> for SurfaceSpecializer {
    type Key = SurfaceKey;

    fn specialize(&self, key: SurfaceKey, descriptor: &mut D) -> SurfaceKey {
        descriptor.set_surface(key);
        key
    }
}

impl SurfaceTarget for RenderPipelineDesc {
    /// The color format and sample count reach every fragment target, and the
    /// depth format the depth-stencil state.
    ///
    /// A target with no depth attachment yields a descriptor with no depth
    /// state: `wgpu` rejects a pipeline that declares one against a pass that
    /// has none, so the state follows the target rather than keeping a stale
    /// format. A target that has one keeps whatever state the descriptor
    /// carried — the comparison, the write mask — and only takes the format.
    fn set_surface(&mut self, surface: SurfaceKey) {
        self.multisample.count = surface.sample_count;
        if let Some(fragment) = &mut self.fragment {
            for target in fragment.targets.iter_mut().flatten() {
                target.format = surface.color_format;
            }
        }
        match surface.depth_stencil_format {
            Some(format) => {
                if let Some(depth_stencil) = &mut self.depth_stencil {
                    depth_stencil.format = format;
                }
            }
            None => self.depth_stencil = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;

    use super::*;

    /// A blueprint a specializer rewrites.
    #[derive(Clone, Debug, PartialEq)]
    struct TestDescriptor {
        /// The value every variant starts from.
        base: u32,
        /// The value the specializer writes.
        rewritten: u32,
    }

    /// A pipeline the descriptor compiles into. The noop backend makes a
    /// stand-in value cheap, and the descriptor is what the tests assert on.
    type TestPipeline = u64;

    impl PipelineDescriptor<TestPipeline> for TestDescriptor {
        fn create(&self, _device: &wgpu::Device) -> TestPipeline {
            u64::from(self.rewritten)
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

    impl Specializer<TestDescriptor> for TestSpecializer {
        type Key = TestKey;

        fn specialize(&self, key: TestKey, descriptor: &mut TestDescriptor) -> u32 {
            descriptor.rewritten = key.id;
            key.id
        }
    }

    /// Writes the key's canonical form into the descriptor.
    struct CanonicalizingSpecializer;

    impl Specializer<TestDescriptor> for CanonicalizingSpecializer {
        type Key = NonCanonicalKey;

        fn specialize(&self, key: NonCanonicalKey, descriptor: &mut TestDescriptor) -> u32 {
            descriptor.rewritten = key.canonical;
            key.canonical
        }
    }

    /// Writes the key's raw value into the descriptor while reporting the
    /// canonical one: two keys with one canonical form disagree.
    struct LiarSpecializer;

    impl Specializer<TestDescriptor> for LiarSpecializer {
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

    fn variants<S: Specializer<TestDescriptor>>(
        specializer: S,
    ) -> Variants<TestPipeline, TestDescriptor, S> {
        let (device, _queue) = crate::util::test::noop_device();
        Variants::new(&device, specializer)
    }

    /// How many variants the cache has created.
    fn created<P, D: PipelineDescriptor<P>, S: Specializer<D>>(
        variants: &Variants<P, D, S>,
    ) -> usize {
        variants.variants.len()
    }

    #[test]
    fn variant_is_created_once_per_key() {
        let mut variants = variants(TestSpecializer);

        let first = variants.specialize(base, TestKey { id: 1 });
        let again = variants.specialize(base, TestKey { id: 1 });
        assert_eq!(first, again, "the same key reuses its variant");
        assert_eq!(created(&variants), 1);

        let second = variants.specialize(base, TestKey { id: 2 });
        assert_ne!(first, second, "a different key gets a new variant");
        assert_eq!(created(&variants), 2);
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
        assert_eq!(created(&variants), 1);

        let third = variants.specialize(
            base,
            NonCanonicalKey {
                raw: 3,
                canonical: 10,
            },
        );
        assert_ne!(first, third, "a new canonical form gets a new variant");
        assert_eq!(created(&variants), 2);
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
        assert_eq!(created(&variants), 2);
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

    /// The compute shader the pipeline-agnostic cache test compiles.
    const COMPUTE_WGSL: &str = "\
@compute @workgroup_size(1)
fn main() {
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

    /// A key that names no configuration, for a specializer that rewrites
    /// nothing.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct UnitKey;

    impl SpecializerKey for UnitKey {
        const IS_CANONICAL: bool = true;
        type Canonical = Self;
    }

    /// A specializer that leaves the descriptor alone.
    struct IdentitySpecializer;

    impl Specializer<RenderPipelineDesc> for IdentitySpecializer {
        type Key = UnitKey;

        fn specialize(&self, key: UnitKey, _descriptor: &mut RenderPipelineDesc) -> UnitKey {
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

        let first = variants.specialize(|| descriptor.clone(), UnitKey);
        let again = variants.specialize(|| descriptor.clone(), UnitKey);
        assert_eq!(first, again);
        assert_eq!(created(&variants), 1);
    }

    /// The pipeline type parameter is what makes the cache pipeline-agnostic:
    /// a descriptor that compiles a compute pipeline caches through the same
    /// [Variants] a render descriptor does.
    #[test]
    fn a_compute_pipeline_variant_is_cached() {
        /// A blueprint for a compute pipeline, which knows only a workgroup
        /// size.
        #[derive(Clone, Debug, PartialEq)]
        struct ComputeDesc {
            workgroup: u32,
        }

        impl PipelineDescriptor<wgpu::ComputePipeline> for ComputeDesc {
            fn create(&self, device: &wgpu::Device) -> wgpu::ComputePipeline {
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("test::compute"),
                    source: wgpu::ShaderSource::Wgsl(COMPUTE_WGSL.into()),
                });
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("test::compute::layout"),
                    bind_group_layouts: &[],
                    immediate_size: 0,
                });
                let _ = self.workgroup;
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("test::compute"),
                    layout: Some(&layout),
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                })
            }
        }

        /// Rewrites the workgroup size, reporting it as the canonical form.
        struct WorkgroupSpecializer;

        impl Specializer<ComputeDesc> for WorkgroupSpecializer {
            type Key = TestKey;

            fn specialize(&self, key: TestKey, descriptor: &mut ComputeDesc) -> u32 {
                descriptor.workgroup = key.id;
                key.id
            }
        }

        let (device, _queue) = crate::util::test::noop_device();
        let mut variants = Variants::new(&device, WorkgroupSpecializer);
        let base = || ComputeDesc { workgroup: 1 };

        let first = variants.specialize(base, TestKey { id: 8 });
        assert_eq!(
            variants.specialize(base, TestKey { id: 8 }),
            first,
            "one variant per workgroup size"
        );
        assert_ne!(variants.specialize(base, TestKey { id: 16 }), first);
        assert_eq!(created(&variants), 2);
    }

    fn layout(array_stride: u64) -> VertexBufferLayoutDesc {
        VertexBufferLayoutDesc {
            array_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: VertexAttributes::new(),
        }
    }

    fn hash_of(layout: &VertexLayout) -> u64 {
        let mut hasher = DefaultHasher::new();
        layout.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn layouts_with_equal_buffers_are_equal_and_hash_alike() {
        let one = VertexLayout::new([(0, layout(12)), (1, layout(24))]);
        let other = VertexLayout::new([(0, layout(12)), (1, layout(24))]);

        assert_eq!(one, other, "separately built layouts compare equal");
        assert_eq!(
            hash_of(&one),
            hash_of(&other),
            "equal layouts must hash alike, since the hash is precomputed"
        );
    }

    #[test]
    fn layouts_that_differ_are_not_equal() {
        let one = VertexLayout::new([(0, layout(12))]);
        let other = VertexLayout::new([(0, layout(16))]);

        assert_ne!(one, other);
    }

    /// The hash is stored, so it is only correct if it was computed over the
    /// buffers: an empty layout must not collide with a populated one by
    /// construction.
    #[test]
    fn an_empty_layout_is_not_equal_to_a_populated_one() {
        assert_ne!(
            VertexLayout::default(),
            VertexLayout::new([(0, layout(12))])
        );
    }
}
