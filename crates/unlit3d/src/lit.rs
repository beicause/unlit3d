//! The built-in lit pipeline, gathered in one module.
//!
//! Nothing here has a private path into the renderer: the family is registered
//! through the same [`MeshSource::register_family`](crate::mesh_source::MeshSource::register_family)
//! a caller's own family uses,
//! its instance record is an ordinary [`InstanceData`](crate::pipeline::InstanceData),
//! and its meshes and
//! materials go through the same pools and graph. The module exists to keep the
//! built-in implementation together, not to give it privilege.
//!
//! # Lit or unlit
//!
//! One family draws both: [`LitOptions::unlit`](unlit_wgpu::pipeline::LitOptions::unlit)
//! is a flag of the caller's options, and a variant that does not set it
//! multiplies its base colour by the frame's lights while an unlit one draws
//! the colour unchanged and declares none of the lighting bindings. The two are
//! one family rather than two because everything else is shared -- the vertex
//! channels, the material group, the instance stream -- so a mesh allocated for
//! either draws with both, and a scene that adds a light re-keys nothing.
//!
//! The lights themselves, and the clustering a frame does of them, live in
//! [crate::light].

use core::mem::size_of;

use arrayvec::ArrayVec;
use unlit_ecs::Scope;
use unlit_wgpu::mesh::{
    ChannelEncoding, MeshInstance, PositionStreamChannels, SecondaryFlags, compress_weights,
};
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, GlobalBindings, INSTANCE_SLOT,
    LitOptions, LitVariant, LitVertexChannels, PRIMARY_SLOT, SECONDARY_SLOT,
};
use unlit_wgpu::resources::{ResHandle, TextureView};
use unlit_wgpu::specialize::{
    SpecializedPipeline, SurfaceKey, VertexAttributes, VertexBufferLayoutDesc,
};
use unlit_wgpu::util::Hashed;
use zerocopy::IntoBytes;

use crate::components::{GpuMaterial, GpuMesh};
use crate::mesh::MorphDeltas;
use crate::mesh_source::MeshSource;
use crate::pipeline::{
    DrawContext, FamilyContext, InstanceContext, InstanceData, InstanceStreamDesc,
    RegisteredRenderPipeline, RenderPipelineFactory, RenderPipelineKey,
};

/// The key of the built-in family, lit or unlit.
///
/// Two entities drawing the same family may start from different options, so
/// the options live on the entity's key rather than on the source. The key's
/// type is how the source finds the family it draws with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LitPipelineKey {
    /// The options the entity's variants are specialized from, with their hash
    /// computed once. Every per-frame lookup clones this record whole, so the
    /// variants it resolves to share the stored word instead of recomputing
    /// it; rewrite the options through [`Hashed::update`], never the value
    /// itself.
    ///
    /// [`LitOptions::unlit`] is the one flag of them a key picks on its own:
    /// every other fact a variant depends on comes from the draw.
    pub options: Hashed<LitOptions>,
}

impl LitPipelineKey {
    /// A key whose variants start from `options`.
    pub fn new(options: LitOptions) -> Self {
        Self {
            options: Hashed::new(options),
        }
    }
}

impl RenderPipelineKey for LitPipelineKey {
    type Variant = LitVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> LitVariant {
        lit_variant(&self.options, draw)
    }
}

/// The variant a draw resolves to from the caller's options, lit or not.
///
/// Every fact the variant depends on beyond the caller's own options is
/// resolved here, from the draw itself: the frame target, the channels the
/// mesh vertex layout carries, whether the draw binds a material, whether the
/// mesh morphs, and the width of the index buffer a strip topology has to
/// declare. The lighting flag is the one exception, because it is the caller's
/// choice rather than the draw's — which is why a lit and an unlit draw share
/// this function and the [LitFactory] that describes the result.
pub(crate) fn lit_variant(options: &Hashed<LitOptions>, draw: &DrawContext<'_>) -> LitVariant {
    let index_format = draw
        .mesh
        .parts
        .index_buffer
        .as_ref()
        .map(|(_, format)| *format);
    LitVariant {
        options: options.clone(),
        surface: draw.surface,
        channels: LitVertexChannels::for_vertex_layout(&draw.mesh.vertex_layout),
        base_color_texture: draw.material.is_some(),
        morph: draw.mesh.morph_targets > 0,
        strip_index_format: strip_index_format(options.primitive.topology, index_format),
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
/// [SpecializedLitPipeline](unlit_wgpu::pipeline::SpecializedLitPipeline)
/// the way the source registers it.
///
/// This is what keeps the built-in pipeline an ordinary client of the family
/// machinery: it packages the shader layouts and a closure over the
/// shader global bindings — the camera, globals and, for variants that read
/// a compressed channel, the metadata buffer.
pub(crate) struct LitFactory;

impl RenderPipelineFactory<LitVariant> for LitFactory {
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, LitVariant>,
    ) -> RegisteredRenderPipeline {
        // The layouts are a function of the variant the pipeline was compiled
        // from, so they are derived here rather than stored with the pipeline.
        // Only the global group is registered: the material group is the
        // material own, built by `allocate_lit_material`, and the built-in
        // variants bind nothing at the mesh group.
        //
        // The layout and the recipe are the source's own general facilities,
        // the same ones a caller's factory calls: the lit family has no
        // private path to the frame's shared buffers.
        let variant = value.descriptor();
        let bindings = GlobalBindings {
            metadata: variant.needs_metadata(),
            joints: variant.needs_joints(),
            morphs: variant.needs_morphs(),
            lights: !variant.options.unlit,
        };
        let layout = context.resources.layout(context.device, bindings);
        let recipe = context.resources.recipe(layout, bindings);
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(recipe),
        }
    }
}

/// The channels of one mesh uploaded through
/// [`MeshSource::allocate_lit_mesh`](crate::mesh_source::MeshSource::allocate_lit_mesh).
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
pub struct LitMeshDesc<'a> {
    /// Per-vertex positions, in the mesh's local space.
    pub positions: &'a [[f32; 3]],
    /// Per-vertex UVs, for a variant that reads them.
    pub uvs: Option<&'a [[f32; 2]]>,
    /// Per-vertex linear RGBA colors, for a variant that reads them.
    pub colors: Option<&'a [[u8; 4]]>,
    /// Per-vertex normals, in the mesh's local space, for a variant that
    /// shades with them.
    ///
    /// A normal is octahedral-encoded into the secondary stream, so it needs
    /// no per-mesh range. A mesh without normals is shaded by the geometric
    /// normal a lit variant derives from screen-space derivatives, which is
    /// why the slice is optional rather than required.
    pub normals: Option<&'a [[f32; 3]]>,
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
/// [`LitOptions::alpha_cutoff`](unlit_wgpu::pipeline::LitOptions::alpha_cutoff)
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

/// Per-instance inverted-hull width, expanding the mesh along its normals.
///
/// The renderer packs it into the per-instance vertex stream next to the model
/// matrix, so two entities drawing one mesh can expand by different amounts —
/// which is the whole point of the outline: an entity is drawn twice, once as
/// itself and once as a shell, and only the shell carries this component.
///
/// The component only supplies the distance the shader pushes along; a
/// pipeline only reads it when its
/// [`LitOptions::expanded`](unlit_wgpu::pipeline::LitOptions::expanded) is set,
/// which decides whether the stream carries the attribute at all. Entities
/// without this component expand by nothing, so a mesh drawn as itself is left
/// exactly where it is.
///
/// The distance is in world units, and the normals are transformed before the
/// push, so a scaled instance keeps the same outline thickness as an unscaled
/// one. An outline therefore costs the same at any distance from the camera,
/// which is what a hand-drawn line looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
pub struct InstanceHullWidth {
    /// Distance along the normal, in world units.
    pub width: f32,
}

impl Default for InstanceHullWidth {
    fn default() -> Self {
        Self { width: 0.0 }
    }
}

impl InstanceHullWidth {
    /// A hull component expanding the mesh by `width` along its normals.
    pub const fn new(width: f32) -> Self {
        Self { width }
    }
}

/// The built-in lit family's instance record: the resolved [MeshInstance].
///
/// It is an ordinary [InstanceData] implementation, with no more privilege than
/// a caller's: it reads the entity's transform, tint, cutoff, hull width, mesh
/// metadata and skin/morph state from the same world and the same pack methods
/// any family uses, and writes the [MeshInstance] layout the built-in shader
/// declares.
#[derive(Clone, Copy, Debug, Default)]
pub struct LitInstance;

impl InstanceData for LitInstance {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: INSTANCE_SLOT,
            array_stride: size_of::<MeshInstance>() as u32,
            label: Some("unlit3d::lit::instances"),
        }
    }

    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]) {
        let world = &context.world;
        let entity = context.entity;
        let mut mesh_row = world.query_one::<&GpuMesh>(entity).unwrap_or_else(|_| {
            panic!(
                "every visible entity carries the mesh culling resolved, but {entity:?} does not"
            )
        });
        let mesh = mesh_row.get();
        let base_color = world
            .query_one::<&InstanceColor>(entity)
            .map_or(glam::Vec4::ONE, |mut color| color.get().color);
        let cutoff = world
            .query_one::<&InstanceCutoff>(entity)
            .map_or(0.0, |mut cutoff| cutoff.get().cutoff);
        let hull_width = world
            .query_one::<&InstanceHullWidth>(entity)
            .map_or(0.0, |mut hull| hull.get().width);
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
            .with_cutoff(cutoff)
            .with_hull_width(hull_width);
        out.copy_from_slice(instance.as_bytes());
    }
}

mod allocate;

#[cfg(test)]
mod tests;
