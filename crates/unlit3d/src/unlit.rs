//! The built-in unlit pipeline, gathered in one module.
//!
//! Nothing here has a private path into the renderer: the family is registered
//! through the same [`MeshSource::register_family`] a caller's own family uses,
//! its instance record is an ordinary [`InstanceData`], and its meshes and
//! materials go through the same pools and graph. The module exists to keep the
//! built-in implementation together, not to give it privilege.

use core::mem::size_of;
use std::rc::Rc;

use arrayvec::ArrayVec;
use unlit_ecs::World;
use unlit_wgpu::mesh::{
    ChannelEncoding, MeshInstance, MeshMetadata, PositionStreamChannels, UvColorFlags,
    compress_weights, index_fits_u16,
};
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, GlobalBindings, INSTANCE_SLOT,
    POSITION_SLOT, UV_COLOR_SLOT, UnlitOptions, UnlitVariant, UnlitVertexChannels,
};
use unlit_wgpu::resources::{ResourceId, TextureView};
use unlit_wgpu::scene::MAX_VERTEX_BUFFERS;
use unlit_wgpu::specialize::{
    SpecializedPipeline, SurfaceKey, VertexAttributes, VertexBufferLayoutDesc, VertexLayout,
};
use unlit_wgpu::util::Hashed;
use unlit_wgpu::vertex_pool::VertexStreamPool;
use zerocopy::IntoBytes;

use crate::bounds::Aabb;
use crate::components::{GpuMaterial, GpuMesh, GpuRenderPipeline};
use crate::mesh::{MeshDesc, MorphDeltas};
use crate::mesh_source::MeshSource;
use crate::pipeline::{
    DrawContext, FamilyContext, InstanceContext, InstanceData, InstanceStreamDesc,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineKey,
};

/// The byte size of one index of `format`.
///
/// A pooled index range starts at a multiple of [`wgpu::COPY_BUFFER_ALIGNMENT`],
/// which is a multiple of either format's size, so dividing its byte offset by
/// this yields a whole number of indices.
fn index_format_size(format: wgpu::IndexFormat) -> u32 {
    match format {
        wgpu::IndexFormat::Uint16 => size_of::<u16>() as u32,
        wgpu::IndexFormat::Uint32 => size_of::<u32>() as u32,
    }
}

/// Where a mesh's vertex-pool offset goes, given whether the device has
/// `base_vertex`.
///
/// A mesh's vertices sit somewhere inside the pool its streams share, so a draw
/// has to be offset to reach them. A device with `base_vertex` names the offset
/// on the draw, which leaves the indices mesh-local and keeps them narrow; a
/// device without it — WebGL2, whose GLES 3.0 has no such draw — has to add the
/// offset to the indices at upload, where the draw's own stays zero. The two
/// are complementary, and exactly one is non-zero.
///
/// Baking can widen the indices when the offset pushes one past `u16::MAX`,
/// which is the cost of binding the whole pool instead of rebinding a vertex
/// buffer per mesh. A draw's `base_vertex` has no such cost, but is a signed
/// 32-bit integer, so the offset has to fit one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexOffset {
    /// Added to every index at upload.
    baked: u32,
    /// Carried by the draw as its `base_vertex`.
    draw_base_vertex: u32,
}

impl IndexOffset {
    /// Split `vertex_offset` between the indices and the draw.
    fn new(supports_base_vertex: bool, vertex_offset: u32) -> Self {
        if supports_base_vertex {
            Self {
                baked: 0,
                draw_base_vertex: vertex_offset,
            }
        } else {
            Self {
                baked: vertex_offset,
                draw_base_vertex: 0,
            }
        }
    }

    /// The format `indices` pack into and the number of bytes they occupy,
    /// with the pool offset applied.
    ///
    /// An offset carried by the draw leaves the indices mesh-local, so they
    /// stay as narrow as the mesh's own vertex count allows; an offset baked in
    /// here can push an index past `u16::MAX`, which widens *every* index of
    /// the mesh to `Uint32`. Either way the width is decided on the baked
    /// values.
    ///
    /// The length is padded to [`wgpu::COPY_BUFFER_ALIGNMENT`], which is the
    /// alignment an index pool hands its ranges out at, so it is both the size
    /// to allocate and the number of bytes [`Self::write`] fills.
    ///
    /// The slice must not be empty: an empty mesh has no index buffer at all.
    fn packed_len(self, indices: &[u32]) -> (wgpu::IndexFormat, usize) {
        let widened = indices
            .iter()
            .any(|&index| !index_fits_u16(index + self.baked));
        let element = if widened {
            size_of::<u32>()
        } else {
            size_of::<u16>()
        };
        let format = if widened {
            wgpu::IndexFormat::Uint32
        } else {
            wgpu::IndexFormat::Uint16
        };
        let padded_len =
            (indices.len() * element).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize);
        (format, padded_len)
    }

    /// Write `indices` with the pool offset applied into `out`, which must be
    /// exactly [`Self::packed_len`]`(indices)` bytes.
    ///
    /// The offset is applied as the bytes are written, so no intermediate index
    /// buffer exists; the trailing alignment padding is zeroed.
    ///
    /// The slice must not be empty: an empty mesh has no index buffer at all.
    fn write(self, indices: &[u32], out: wgpu::WriteOnly<'_, [u8]>) {
        let (format, padded_len) = self.packed_len(indices);
        assert_eq!(
            out.len(),
            padded_len,
            "the target must hold exactly the packed index buffer"
        );
        let element = index_format_size(format) as usize;
        let (data, mut padding) = out.split_at(indices.len() * element);
        match format {
            wgpu::IndexFormat::Uint16 => {
                let (chunks, _remainder) = data.into_chunks::<{ size_of::<u16>() }>();
                chunks.write_iter(
                    indices
                        .iter()
                        .map(|&index| (index + self.baked) as u16)
                        .map(u16::to_ne_bytes),
                );
            }
            wgpu::IndexFormat::Uint32 => {
                let (chunks, _remainder) = data.into_chunks::<{ size_of::<u32>() }>();
                chunks.write_iter(
                    indices
                        .iter()
                        .map(|&index| index + self.baked)
                        .map(u32::to_ne_bytes),
                );
            }
        }
        padding.fill(0);
    }

    /// [`Self::write`] into a fresh `Vec`, for tests that inspect the bytes.
    #[cfg(test)]
    fn pack(self, indices: &[u32]) -> (wgpu::IndexFormat, Vec<u8>) {
        let (format, padded_len) = self.packed_len(indices);
        let mut data = vec![0u8; padded_len];
        self.write(indices, wgpu::WriteOnly::from_mut(data.as_mut_slice()));
        (format, data)
    }
}

/// The per-entity options the built-in unlit family draws with.
///
/// Two entities drawing the same family may start from different options, so
/// the options live on the entity's key rather than on the source. The key's
/// type is how the source finds the family it draws with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnlitPipelineKey {
    /// The options the entity's variants are specialized from, with their hash
    /// computed once. Every per-frame lookup clones this record whole, so the
    /// variants it resolves to share the stored word instead of recomputing
    /// it; rewrite the options through [`Hashed::update`], never the value
    /// itself.
    pub options: Hashed<UnlitOptions>,
}

impl UnlitPipelineKey {
    /// A key whose variants start from `options`.
    pub fn new(options: UnlitOptions) -> Self {
        Self {
            options: Hashed::new(options),
        }
    }
}

impl RenderPipelineKey for UnlitPipelineKey {
    type Variant = UnlitVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> UnlitVariant {
        // Every fact the pipeline depends on beyond the caller own options
        // is resolved here, from the draw itself: the frame target, the
        // channels the mesh vertex layout carries, whether the draw binds a
        // material, whether the mesh morphs, and the width of the index
        // buffer a strip topology has to declare.
        let index_format = draw
            .mesh
            .parts
            .index_buffer
            .as_ref()
            .map(|(_, format)| *format);
        UnlitVariant {
            options: self.options.clone(),
            surface: draw.surface,
            channels: UnlitVertexChannels::for_vertex_layout(&draw.mesh.vertex_layout),
            base_color_texture: draw.material.is_some(),
            morph: draw.mesh.morph_targets > 0,
            strip_index_format: strip_index_format(self.options.primitive.topology, index_format),
        }
    }
}

/// The strip index format a draw's pipeline has to declare.
///
/// Only a strip topology reads one, and there it has to equal the format of
/// the index buffer the draw binds, so a mesh's format is passed through for a
/// strip and dropped for every other topology — a pipeline that declared one
/// for a non-strip topology is invalid.
fn strip_index_format(
    topology: wgpu::PrimitiveTopology,
    index_format: Option<wgpu::IndexFormat>,
) -> Option<wgpu::IndexFormat> {
    index_format.filter(|_| topology.is_strip())
}

/// Describes a specialized
/// [SpecializedUnlitPipeline](unlit_wgpu::pipeline::SpecializedUnlitPipeline)
/// the way the source registers it.
///
/// This is what keeps the built-in pipeline an ordinary client of the family
/// machinery: it packages the shader layouts and a closure over the
/// shader global bindings — the camera, globals and, for variants that read
/// a compressed channel, the metadata buffer.
struct UnlitFactory;

impl RenderPipelineFactory<UnlitVariant> for UnlitFactory {
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, UnlitVariant>,
    ) -> RegisteredRenderPipeline {
        // The layouts are a function of the variant the pipeline was compiled
        // from, so they are derived here rather than stored with the pipeline.
        // Only the global group is registered: the material group is the
        // material own, built by `allocate_unlit_material`, and the built-in
        // variants bind nothing at the mesh group.
        //
        // The layout and the recipe are the source's own general facilities,
        // the same ones a caller's factory calls: the unlit family has no
        // private path to the frame's shared buffers.
        let variant = value.descriptor();
        let bindings = GlobalBindings {
            metadata: variant.needs_metadata(),
            joints: variant.needs_joints(),
            morphs: variant.needs_morphs(),
        };
        let layout = context.resources.layout(context.device, bindings);
        let rebuild = context.resources.rebuild(context.device, layout, bindings);
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(rebuild),
        }
    }
}

/// The channels of one mesh uploaded through
/// [`MeshSource::allocate_unlit_mesh`](crate::mesh_source::MeshSource::allocate_unlit_mesh).
///
/// Every slice describes the same vertices, in the same order; only
/// [`Self::positions`] is required, because a variant without a position
/// stream draws a single point and needs no geometry at all.
///
/// The pose a mesh is drawn with is not here: joint matrices and morph weights
/// are CPU-driven per-frame state, so they live on the entities a
/// [`SkinBinding`](crate::components::SkinBinding) and a
/// [`MorphBinding`](crate::components::MorphBinding) name and the renderer
/// uploads them every frame.
#[derive(Clone, Debug, Default)]
pub struct UnlitMeshDesc<'a> {
    /// Per-vertex positions, in the mesh's local space.
    pub positions: &'a [[f32; 3]],
    /// Per-vertex UVs, for a variant that reads them.
    pub uvs: Option<&'a [[f32; 2]]>,
    /// Per-vertex linear RGBA colors, for a variant that reads them.
    pub colors: Option<&'a [[u8; 4]]>,
    /// Triangle indices, for an indexed draw.
    pub indices: Option<&'a [u32]>,
    /// The joints each vertex is bound to, for a variant that reads joints.
    pub joints: Option<&'a [[u16; 4]]>,
    /// The joint weights of each vertex, four per vertex, for a variant that
    /// reads joints.
    ///
    /// Weights are normalized on upload, so a caller may pass unnormalized
    /// ones; a vertex whose weights sum to zero is left undeformed.
    pub weights: Option<&'a [[f32; 4]]>,
    /// The morph displacements that deform the mesh, for a variant that reads
    /// them.
    ///
    /// Only positions displace: a target carries no normal or tangent, so the
    /// packed displacements are all this mesh has. How much of them applies is
    /// the mesh's [`MorphWeights`](crate::components::MorphWeights) component,
    /// not the mesh's: the same displacements can be weighted differently by
    /// two meshes.
    pub morph_deltas: Option<MorphDeltas>,
}

/// The built-in unlit pipeline component.
///
/// It carries an [UnlitPipelineKey], which names the built-in unlit family and
/// supplies the options the entity's variants are specialized from.
pub type UnlitPipeline = GpuRenderPipeline<UnlitPipelineKey>;

/// Per-instance color, multiplying the base color.
///
/// The renderer packs it into the per-instance vertex stream next to the model
/// matrix, so two entities sharing a mesh can still be tinted differently.
/// Entities without this component are drawn white.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct InstanceColor {
    /// Base color (RGBA, unpremultiplied).
    #[cfg_attr(feature = "reflect", facet(opaque, proxy = crate::reflect::Vec4Proxy))]
    pub color: glam::Vec4,
}

impl Default for InstanceColor {
    fn default() -> Self {
        Self {
            color: glam::Vec4::ONE,
        }
    }
}

impl InstanceColor {
    /// A color component carrying `color`.
    pub const fn new(color: glam::Vec4) -> Self {
        Self { color }
    }
}

/// Per-instance alpha cutoff, discarding fragments below it.
///
/// The renderer packs it into the per-instance vertex stream next to the model
/// matrix, so two entities sharing a mesh and a cut-off pipeline can still cut
/// at different alphas. Entities without this component leave the cutoff at
/// zero, which no alpha falls below, and so discard nothing.
///
/// The component only supplies the value the shader compares against; a
/// pipeline only reads it when its
/// [`UnlitOptions::alpha_cutoff`](unlit_wgpu::pipeline::UnlitOptions::alpha_cutoff)
/// is set, which decides whether the stream carries the attribute at all.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct InstanceCutoff {
    /// Alpha a fragment has to reach to be drawn.
    pub cutoff: f32,
}

impl Default for InstanceCutoff {
    fn default() -> Self {
        Self { cutoff: 0.0 }
    }
}

impl InstanceCutoff {
    /// A cutoff component discarding fragments below `cutoff`.
    pub const fn new(cutoff: f32) -> Self {
        Self { cutoff }
    }
}

/// The built-in unlit family's instance record: the resolved [MeshInstance].
///
/// It is an ordinary [InstanceData] implementation, with no more privilege than
/// a caller's: it reads the entity's transform, tint, cutoff, mesh metadata and
/// skin/morph state from the same world and the same pack methods any family
/// uses, and writes the [MeshInstance] layout the built-in shader declares.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnlitInstance;

impl InstanceData for UnlitInstance {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: INSTANCE_SLOT,
            array_stride: size_of::<MeshInstance>() as u32,
        }
    }

    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]) {
        let world = context.world;
        let entity = context.entity;
        let mesh = world
            .get::<GpuMesh>(entity)
            .expect("every visible entity carries the mesh culling resolved");
        let base_color = world
            .get::<InstanceColor>(entity)
            .map_or(glam::Vec4::ONE, |color| color.color);
        let cutoff = world
            .get::<InstanceCutoff>(entity)
            .map_or(0.0, |cutoff| cutoff.cutoff);
        // Skinning and morphing are independent: a mesh may have either, both
        // or neither, and each packs into its own array.
        let joints_base = if mesh.skinned {
            context.pack_joints()
        } else {
            0
        };
        let morph_base = if mesh.morph_targets > 0 {
            context.pack_morph_weights(mesh.morph_targets)
        } else {
            0
        };
        let instance = MeshInstance::new(context.world_from_local, base_color)
            .with_joints_base(joints_base)
            .with_morph_base(morph_base)
            .with_metadata_index(mesh.parts.metadata_index)
            .with_cutoff(cutoff);
        out.copy_from_slice(instance.as_bytes());
    }
}

/// Mesh allocation and family registration for the built-in unlit pipeline.
///
/// The methods are inherent work the source already does; the trait only
/// gathers them next to the shader they serve. A caller's own family uses
/// [`MeshSource::register_family`] and [`MeshSource::allocate_mesh`] the same
/// way these use them.
pub trait MeshSourceUnlitExt {
    /// Register the built-in unlit shader as a family.
    ///
    /// The family specializes each entity's options on the frame's render
    /// target and the mesh's vertex layout: two draws that agree on both share
    /// one compiled pipeline, and a draw whose mesh layout implies different
    /// channels compiles its own. The options are the entity's: a mesh or
    /// material is built against the [UnlitPipelineKey] it will be drawn with,
    /// through [`MeshSourceUnlitExt::allocate_unlit_mesh`] and
    /// [`MeshSourceUnlitExt::allocate_unlit_material`].
    ///
    /// This compiles nothing; like any family, its first concrete pipeline is
    /// built when an entity that uses it is first drawn.
    ///
    /// # Panics
    ///
    /// If the unlit family is already registered.
    fn register_unlit_family(&mut self, world: &World);
    /// Upload raw mesh channels in the layout the built-in unlit shader
    /// expects, and return a [GpuMesh] handle.
    ///
    /// Positions and UVs are compressed to the compact vertex formats the
    /// shader decodes, packed into a position buffer and an interleaved
    /// UV-and-colour buffer, and bound with the mesh-metadata bind group the
    /// shader reads its decode parameters from. Colours are already stored in
    /// the width they are uploaded at, so they are copied through unchanged.
    ///
    /// A skinned mesh's joint indices and weights are packed into the *same*
    /// stream as its positions, right after them. A morphed mesh's per-vertex
    /// displacements go into a buffer of its own.
    ///
    /// Neither mesh carries a pose: the joint matrices and morph weights a
    /// frame deforms by are per-instance state the CPU writes, so they live in
    /// the [`SkinPose`](crate::components::SkinPose) and
    /// [`MorphWeights`](crate::components::MorphWeights) components the mesh's
    /// entity names through a [`SkinBinding`](crate::components::SkinBinding)
    /// and a [`MorphBinding`](crate::components::MorphBinding). The frame packs
    /// them once per frame, so animating a mesh costs a component write and no
    /// re-upload.
    ///
    /// This is a convenience over [`MeshSource::allocate_mesh`]: it builds the
    /// same [`MeshDesc`] a caller could build by hand, and shares the
    /// compression in [unlit_wgpu::mesh] with anyone else who wants it.
    ///
    /// Which channels are packed follows the slices the caller passes: a
    /// channel with no slice is left out, and the mesh's vertex layout records
    /// exactly the channels that were packed, so the pipeline it is later
    /// drawn with reads back the same shape.
    /// # Panics
    ///
    /// If the input slices are empty or of mismatched length (see the
    /// compressors in [unlit_wgpu::mesh]); if only one of
    /// [`UnlitMeshDesc::joints`] and [`UnlitMeshDesc::weights`] is `Some`; or
    /// if [`UnlitMeshDesc::morph_deltas`] is present but does not hold three
    /// components per vertex and target.
    fn allocate_unlit_mesh(
        &mut self,
        world: &World,
        key: &UnlitPipelineKey,
        desc: UnlitMeshDesc<'_>,
    ) -> GpuMesh;
    /// Allocate the unlit material bind group from an existing base-colour
    /// texture view and sampler, and return its [GpuMaterial] handle.
    ///
    /// Only the bind group is built. `view_id` and `sampler_id` name resources
    /// the caller has already put in the graph, and they become the group
    /// dependencies, so replacing either marks it dirty. Whether a draw
    /// samples the texture is the draw own answer — it is what decides the
    /// variant — so a caller that wants a material calls this, and one that
    /// does not leaves it out.
    ///
    /// # Panics
    ///
    /// If `view_id` or `sampler_id` is not a texture view or sampler in the
    /// graph.
    fn allocate_unlit_material(
        &mut self,
        world: &World,
        key: &UnlitPipelineKey,
        view_id: ResourceId<TextureView>,
        sampler_id: ResourceId<wgpu::Sampler>,
    ) -> GpuMaterial;
}

impl MeshSourceUnlitExt for MeshSource {
    fn register_unlit_family(&mut self, world: &World) {
        self.register_family::<UnlitPipelineKey, _, _>(world, UnlitFactory, UnlitInstance);
    }

    fn allocate_unlit_mesh(
        &mut self,
        world: &World,
        key: &UnlitPipelineKey,
        desc: UnlitMeshDesc<'_>,
    ) -> GpuMesh {
        let device = self.device(world);
        let queue = self.queue(world);

        let UnlitMeshDesc {
            positions,
            uvs,
            colors,
            indices,
            joints,
            weights,
            morph_deltas,
        } = desc;

        // Which channels the mesh carries is the caller's answer, not the
        // key's: the options are pure policy, so the streams are derived from
        // the slices the caller actually passed. A channel with no slice is
        // left out, and the pipeline the mesh is later drawn with reads that
        // same layout back off the mesh.
        //
        // The streams are always packed in their compressed encodings: a
        // caller that wants to upload full-precision vertices builds its own
        // buffers and its own variant instead, the way the UI path does.
        assert_eq!(
            joints.is_some(),
            weights.is_some(),
            "a mesh's joints and weights come as a pair, so one cannot be present without the other"
        );
        let mut uv_color = UvColorFlags::empty();
        if uvs.is_some() {
            uv_color |= UvColorFlags::UV;
        }
        if colors.is_some() {
            uv_color |= UvColorFlags::COLOR;
        }
        let channels = UnlitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: joints.is_some() && weights.is_some(),
            },
            uv_color,
        };
        // The variant is built only to read the streams and layouts the
        // channels imply. Its surface, material and strip index format play no
        // part in any of those, so they are left at placeholders.
        let variant = UnlitVariant {
            options: key.options.clone(),
            surface: SurfaceKey {
                color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth_stencil_format: None,
                sample_count: 1,
            },
            channels,
            base_color_texture: false,
            morph: morph_deltas.is_some(),
            strip_index_format: None,
        };
        let position_stream = variant.position_stream();
        let uv_color_stream = variant.uv_color_stream();

        // The joint pair belongs to the position stream, so a skinned mesh's
        // stream is wider by it and no separate buffer exists. A variant that
        // deforms by joints reads them from that stream, so it needs the
        // caller's joint indices and weights exactly as a compressed variant
        // needs its UVs. A variant that reads none ignores joints it was
        // handed, as it ignores UVs it does not declare.
        let skinned = variant.needs_joints();
        let (joints, weights) = if skinned {
            let joints = joints.expect("a variant that reads joints needs the mesh's joints");
            let weights = weights.expect("a variant that reads joints needs the mesh's weights");
            assert_eq!(
                joints.len(),
                positions.len(),
                "the joint stream must describe the same vertices as the positions"
            );
            assert_eq!(
                weights.len(),
                positions.len(),
                "the weight stream must describe the same vertices as the positions"
            );
            (joints, weights)
        } else {
            // A variant that reads none ignores the joints it was handed, as
            // it ignores UVs it does not declare.
            (&[] as &[[u16; 4]], &[] as &[[f32; 4]])
        };

        // Both streams' lengths are known before anything is uploaded, so the
        // pool can be sized first and each stream packed straight into it. The
        // position stream's compression derives the mesh metadata as it runs.
        let mut meta = MeshMetadata::default();
        let vertex_count = positions.len();
        let position_len = position_stream.byte_len(vertex_count);
        let uv_color_len = uv_color_stream.byte_len(vertex_count);

        // The morph displacements, when the draw reads them. They are geometry
        // the mesh owns, written once here; the weights that blend them are
        // per-instance pose state the frame's global group binds. The source
        // pools them into the frame-wide array, so what it hands over is the
        // raw data rather than a resource.
        let morph_deltas = variant.needs_morphs().then(|| {
            let morph_deltas = morph_deltas.expect(
                "a variant that reads morph positions needs the mesh's morph displacements",
            );
            assert_ne!(
                morph_deltas.target_count, 0,
                "a morphing mesh needs at least one target"
            );
            assert_eq!(
                morph_deltas.deltas.len(),
                positions.len() * morph_deltas.target_count as usize * 3,
                "a morph displacement array must hold three components per vertex and target"
            );
            morph_deltas
        });

        // The vertex layout the key's options declare, slot for slot. An empty
        // stream still gets a buffer entry — the pipeline simply declares no
        // attributes for that slot — so the mesh's layout matches the key's.
        let vertex_layouts = variant.vertex_buffer_layouts();
        let layout_of = |slot: u32| -> VertexBufferLayoutDesc {
            vertex_layouts
                .get(slot as usize)
                .and_then(|layout| layout.as_ref())
                .cloned()
                .unwrap_or(VertexBufferLayoutDesc {
                    array_stride: 0,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: VertexAttributes::new(),
                })
        };
        let position_layout = layout_of(POSITION_SLOT);
        let uv_color_layout = layout_of(UV_COLOR_SLOT);

        // The mesh's streams are the vertex pool's: one element allocation
        // covers every stream of the mesh at the same element index, which is
        // what lets a draw address them all with one `firstVertex` or
        // `baseVertex`. A stream the variant declares nothing for — an empty
        // UV-and-colour stream, say — gets no buffer and no part in it.
        let stream_layouts = [position_layout.clone(), uv_color_layout.clone()];
        let vertices = self
            .vertex_pool
            .allocate(&device, &queue, &stream_layouts, vertex_count as u32)
            .expect("the vertex pool grows with the mesh");
        let vertex_offset = vertices.offset();
        // Each stream is packed straight into the pool through a queue write,
        // so no CPU-side buffer is built first. The write view is the only
        // mutable borrow, so the two streams are packed one after the other.
        for (slot, layout, len) in [
            (POSITION_SLOT, &position_layout, position_len),
            (UV_COLOR_SLOT, &uv_color_layout, uv_color_len),
        ] {
            if layout.array_stride == 0 {
                continue;
            }
            let id = self.vertex_node(world, layout);
            self.sync_vertex_node(world, &id, layout);
            let buffer = MeshSource::graph(world, self.context)
                .get(&id)
                .expect("the stream's node exists")
                .clone();
            let mut view = queue
                .write_buffer_with(
                    &buffer,
                    VertexStreamPool::byte_offset(layout, vertex_offset),
                    core::num::NonZeroU64::new(len as u64)
                        .expect("a declared stream is never empty"),
                )
                .expect("the stream's range fits its buffer");
            if slot == POSITION_SLOT {
                position_stream.write(
                    positions,
                    joints.iter().copied(),
                    compress_weights(weights),
                    &mut meta,
                    view.slice(..),
                );
            } else {
                uv_color_stream.write(
                    uvs.unwrap_or(&[]),
                    colors.unwrap_or(&[]),
                    &mut meta,
                    view.slice(..),
                );
            }
        }

        // Index buffer (optional): `Uint16` when every index fits, otherwise
        // `Uint32` — the same choice the compressor makes. The indices go into
        // the index pool, so every indexed mesh draws out of one shared buffer
        // and names its own slice through `GpuMesh::first`.
        //
        // A mesh's vertices are somewhere inside the pool its streams share, so
        // a draw has to be offset by `vertex_offset` to reach them. How it is
        // offset is the one thing that depends on the device: a draw's
        // `base_vertex` is the direct way, but WebGL2 has no `base_vertex`, so
        // there the offset is added to the indices themselves at upload and the
        // draw's stays zero. Baking widens the indices if they no longer fit
        // `u16`, which is the cost of binding the whole pool instead of
        // rebinding a vertex buffer per mesh.
        // A draw's `base_vertex` is a signed 32-bit integer, so an offset the
        // draw has to carry must fit one. A baked offset is a `u32` index like
        // any other and has no such limit.
        let supports_base_vertex = self.base_vertex(world);
        assert!(
            !supports_base_vertex || vertex_offset <= i32::MAX as u32,
            "a vertex offset has to fit the i32 a draw's base vertex is"
        );
        // Where the pool offset goes. A device with `base_vertex` leaves the
        // indices mesh-local and lets the draw add the offset; one without has
        // to bake it into the indices and keep the draw's at zero. The two are
        // complementary, and exactly one of them is non-zero.
        let offset = IndexOffset::new(supports_base_vertex, vertex_offset);
        let draw_base_vertex = offset.draw_base_vertex;
        let (index_buffer, count, indexed, index_allocation, first) = match indices {
            Some(indices) if !indices.is_empty() => {
                let index_count = indices.len() as u32;
                // The bytes the draw will read, offset where the device
                // requires and sized for the pool's own alignment. An offset
                // that pushes an index past `u16::MAX` has to widen the buffer
                // it is written into, so the format and the bytes are decided
                // together.
                let (format, packed_len) = offset.packed_len(indices);
                let range = self
                    .index_pool
                    .allocate(&device, &queue, packed_len as u32)
                    .expect("the index pool grows with the mesh");
                {
                    let mut graph = MeshSource::graph(world, self.context);
                    MeshSource::sync_pool_node(&self.index_pool, &self.index_pool_id, &mut graph);
                    let buffer = graph
                        .get(&self.index_pool_id)
                        .expect("the index pool node exists")
                        .clone();
                    let mut view = queue
                        .write_buffer_with(
                            &buffer,
                            u64::from(range.offset()),
                            core::num::NonZeroU64::new(packed_len as u64)
                                .expect("an empty index buffer is never allocated"),
                        )
                        .expect("the index range fits its buffer");
                    offset.write(indices, view.slice(..));
                }
                let first = range.offset() / index_format_size(format);
                (
                    Some((self.index_pool_id.clone(), format)),
                    index_count,
                    true,
                    Some(range.allocation()),
                    first,
                )
            }
            // A non-indexed draw names its first vertex directly, so the pool
            // offset goes on the draw's range. A non-zero `base_vertex` is
            // ignored for a non-indexed draw, so it cannot carry the offset.
            _ => (None, vertex_count as u32, false, None, vertex_offset),
        };

        // The mesh owns the metadata entry `meta` — the same AABB and UV
        // decode parameters the compression just derived, plus the addressing
        // the draw needs. The vertex offset is known here, and the mesh's own
        // morph count is taken from the displacements it uploaded.
        let aabb = Aabb::new(meta.aabb_center, meta.aabb_half_extents);
        let mut mesh = self.allocate_mesh_with_metadata(
            world,
            MeshDesc {
                // The streams are the vertex pool's, not the mesh's own: the
                // mesh names the pool's node for each slot and its own range,
                // so no buffer is registered under the mesh for them.
                vertex_buffers: ArrayVec::new(),
                // The indices are the index pool's, not the mesh's own: the
                // mesh names the pool's node and its own range, so no buffer
                // is registered under the mesh for them.
                index_buffer: None,
                count,
                indexed,
                aabb,
                // The built-in variants bind nothing at the mesh group, so the
                // mesh carries no group of its own.
                bind_group: None,
                morph_deltas,
            },
            meta,
            skinned,
            vertex_offset,
        );

        // The mesh's parts are complete only now: the pool-backed slots and
        // ranges are known once the pools have been allocated above. The handle
        // is freshly built and shared with nothing, so it is uniquely owned
        // here and can be patched in place.
        let parts = Rc::get_mut(&mut mesh.parts).expect("a fresh mesh handle is unshared");

        // The mesh's layout is only the vertex streams it draws from: the
        // per-instance stream is the family's own and is bound by the source
        // at draw time, so it is not part of the mesh's layout at all.
        let mut layouts = ArrayVec::<_, MAX_VERTEX_BUFFERS>::new();

        // The mesh's slices of the pools it shares: the draw names its ranges
        // by `first` and `base_vertex`, and the allocations are handed back on
        // removal. The layout is the key's own, slot by slot: the family keys
        // on it, so it is owned by the mesh even though the buffers behind it
        // are the pool's. A stream the variant declares nothing for is left
        // out, as it is for a mesh that owns its buffers.
        for (slot, layout) in [
            (POSITION_SLOT, &position_layout),
            (UV_COLOR_SLOT, &uv_color_layout),
        ] {
            if layout.array_stride > 0 {
                layouts.push((slot, layout.clone()));
                // Resolved before the parts are borrowed mutably below: the
                // lookup needs the source, which the borrow would rule out.
                let node = self.vertex_node(world, layout);
                parts.vertex_buffers.push((slot, node));
            }
        }
        // The pool-backed slots join the layout only here, once the pools have
        // been allocated; the mesh's handle is replaced with the complete one.
        parts.index_buffer = index_buffer;
        parts.index_allocation = index_allocation;
        parts.vertex_allocation = Some(vertices.allocation());
        mesh.vertex_layout = VertexLayout::new(layouts);
        mesh.first = first;
        // Zero whenever the offset was baked into the indices instead, which is
        // what a draw on a device without `base_vertex` requires. The metadata
        // entry carries `vertex_offset` either way: the shader reads
        // pool-global vertex ordinals on both paths.
        mesh.base_vertex = draw_base_vertex;
        mesh
    }

    fn allocate_unlit_material(
        &mut self,
        world: &World,
        key: &UnlitPipelineKey,
        view_id: ResourceId<TextureView>,
        sampler_id: ResourceId<wgpu::Sampler>,
    ) -> GpuMaterial {
        let layout = key.options.material_bind_group_layout(&self.device(world));

        let device = self.device(world).clone();
        // The recipe reads the view and the sampler back out of the graph each
        // time it runs, so replacing either rebuilds the group from the current
        // handle rather than the one captured here.
        let mut dependencies = ArrayVec::<ResourceId, 2>::new();
        dependencies.push(view_id.erase());
        dependencies.push(sampler_id.erase());
        self.allocate_material(
            world,
            move |graph| {
                let view = graph
                    .get(&view_id)
                    .expect("the view is in the graph")
                    .view()
                    .clone();
                let sampler = graph
                    .get(&sampler_id)
                    .expect("the sampler is in the graph")
                    .clone();
                let mut entries = ArrayVec::<wgpu::BindGroupEntry<'_>, 2>::new();
                entries.push(wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(&view),
                });
                entries.push(wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("unlit3d::material::bind_group"),
                    layout: &layout,
                    entries: &entries,
                })
            },
            dependencies,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_instance_color_is_white() {
        assert_eq!(InstanceColor::default().color, glam::Vec4::ONE);
    }

    #[test]
    fn a_strip_pipeline_declares_the_mesh_index_width() {
        use wgpu::{IndexFormat, PrimitiveTopology as Topology};
        // A strip's pipeline must declare the width its draw binds, so the
        // mesh's format reaches it whichever width the source picked.
        assert_eq!(
            strip_index_format(Topology::LineStrip, Some(IndexFormat::Uint16)),
            Some(IndexFormat::Uint16)
        );
        assert_eq!(
            strip_index_format(Topology::TriangleStrip, Some(IndexFormat::Uint32)),
            Some(IndexFormat::Uint32)
        );
        // A non-indexed strip binds no index buffer, so it declares none.
        assert_eq!(strip_index_format(Topology::LineStrip, None), None);
        assert_eq!(strip_index_format(Topology::TriangleStrip, None), None);
    }

    #[test]
    fn a_non_strip_pipeline_declares_no_index_width() {
        use wgpu::{IndexFormat, PrimitiveTopology as Topology};
        // `wgpu` rejects a strip index format on a non-strip topology, so an
        // indexed mesh of one must not leak its format into the pipeline.
        for topology in [
            Topology::PointList,
            Topology::LineList,
            Topology::TriangleList,
        ] {
            assert_eq!(
                strip_index_format(topology, Some(IndexFormat::Uint16)),
                None
            );
            assert_eq!(
                strip_index_format(topology, Some(IndexFormat::Uint32)),
                None
            );
        }
    }

    #[test]
    fn a_device_with_base_vertex_puts_the_offset_on_the_draw() {
        let offset = IndexOffset::new(true, 7);

        assert_eq!(offset.draw_base_vertex, 7, "the draw carries the offset");
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(
            format,
            wgpu::IndexFormat::Uint16,
            "the indices stay mesh-local, so they keep their width"
        );
        assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
    }

    #[test]
    fn a_device_without_base_vertex_bakes_the_offset_into_the_indices() {
        // WebGL2's GLES 3.0 has no `base_vertex`, and the backend would panic
        // rather than emulate it, so the offset has to be in the indices.
        let offset = IndexOffset::new(false, 7);

        assert_eq!(offset.draw_base_vertex, 0, "the draw must not name one");
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(packed_indices(format, &data, 3), vec![7, 8, 9]);
    }

    #[test]
    fn a_mesh_at_the_start_of_the_pool_needs_no_offset_either_way() {
        // The common case, and the one where both paths agree: nothing is
        // baked and the draw's `base_vertex` is zero.
        for supports_base_vertex in [true, false] {
            let offset = IndexOffset::new(supports_base_vertex, 0);
            assert_eq!(offset.baked, 0);
            assert_eq!(offset.draw_base_vertex, 0);
            let (format, data) = offset.pack(&[0, 1, 2]);
            assert_eq!(format, wgpu::IndexFormat::Uint16);
            assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
        }
    }

    #[test]
    fn a_baked_offset_can_widen_the_indices() {
        // Baking is what forces the wider format: an index that fitted `u16`
        // before the offset may not after it, and the compression is decided on
        // the baked values.
        let indices = [0u32, 1, 2];
        let offset = IndexOffset::new(false, u32::from(u16::MAX));
        let (format, data) = offset.pack(&indices);
        assert_eq!(
            format,
            wgpu::IndexFormat::Uint32,
            "an index past `u16::MAX` cannot be narrowed"
        );
        assert_eq!(packed_indices(format, &data, 3), vec![65535, 65536, 65537]);
    }

    #[test]
    fn packed_indices_are_padded_to_the_pool_alignment() {
        // The pool hands ranges out at `COPY_BUFFER_ALIGNMENT`, so `pack` pads
        // what it writes and its length is the size the pool is asked for.
        let offset = IndexOffset::new(true, 0);

        // Three `Uint16` indices are 6 bytes, rounded up to the alignment.
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(
            data.len(),
            (3 * size_of::<u16>()).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
        );
        assert!(
            data[3 * size_of::<u16>()..].iter().all(|&byte| byte == 0),
            "the padding is zeroed"
        );

        // Many indices round up by that alignment without a whole extra block
        // when they already land on one.
        let indices: Vec<u32> = (0..(u16::MAX as u32 - 1)).collect();
        let (format, data) = offset.pack(&indices);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(
            data.len(),
            (indices.len() * size_of::<u16>())
                .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
        );
    }

    /// The first `count` indices [`IndexOffset::pack`] wrote, as `u32`s.
    ///
    /// The bytes past them are the pool-alignment padding, which the caller
    /// sizes to but does not read.
    fn packed_indices(format: wgpu::IndexFormat, bytes: &[u8], count: usize) -> Vec<u32> {
        match format {
            wgpu::IndexFormat::Uint16 => bytes[..count * size_of::<u16>()]
                .as_chunks::<{ size_of::<u16>() }>()
                .0
                .iter()
                .map(|index| u32::from(u16::from_ne_bytes(*index)))
                .collect(),
            wgpu::IndexFormat::Uint32 => bytes[..count * size_of::<u32>()]
                .as_chunks::<{ size_of::<u32>() }>()
                .0
                .iter()
                .map(|index| u32::from_ne_bytes(*index))
                .collect(),
        }
    }
}
