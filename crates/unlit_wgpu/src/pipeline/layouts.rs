use super::{
    CAMERA_BINDING, CLUSTER_BINDING, FRAME_BINDING, JOINTS_BINDING, LIGHT_CELLS_BINDING,
    LIGHT_VERTICES_BINDING, LIGHTING_BINDING, LIGHTS_BINDING, LitVariant, MESH_METADATA_BINDING,
    MORPH_DELTAS_BINDING, MORPH_WEIGHTS_BINDING, VIEW_BINDING,
};

/// Which of a source's global buffers a pipeline reads.
///
/// The camera, frame-globals and view uniforms are always bound; the arrays are
/// optional and a pipeline declares only the ones it reads. A caller's own
/// pipeline passes the same description to `global_bind_group_layout` as the
/// built-in lit variants do, so binding the frame's shared inputs is not a
/// privilege of the built-in shader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GlobalBindings {
    /// Whether the pipeline reads the frame's mesh-metadata array.
    pub metadata: bool,
    /// Whether the pipeline reads the frame's joint matrices.
    pub joints: bool,
    /// Whether the pipeline reads the frame's morph weights and displacements.
    pub morphs: bool,
    /// Whether the pipeline reads the frame's lights: the bounded-light arrays,
    /// the cluster uniform and the frame-wide lighting uniform.
    ///
    /// One flag for all five bindings: a pipeline that lights its fragments
    /// reads the three arrays and the two uniforms together, so a finer split
    /// would only describe combinations no pipeline can have.
    pub lights: bool,
}

/// The layout entry binding an array of `element_size`-byte elements.
///
/// On the storage path it is a read-only storage buffer whose binding minimum is
/// one element, so the array may grow without invalidating the layout. On the
/// texel path it is a non-filterable float texture, which is what
/// `textureLoad` reads.
pub fn array_entry(
    binding: u32,
    element_size: u64,
    visibility: wgpu::ShaderStages,
    texel_arrays: bool,
) -> wgpu::BindGroupLayoutEntry {
    if texel_arrays {
        return crate::texel_array::TexelArrayLayout::binding(binding, visibility);
    }
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: Some(
                core::num::NonZeroU64::new(element_size)
                    .expect("an element has a non-zero size, which is a valid binding minimum"),
            ),
        },
        count: None,
    }
}

/// The resources in the shared global bind group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalSlot {
    /// The camera uniform.
    Camera,
    /// The frame uniform.
    Frame,
    /// The pass's view uniform.
    View,
    /// The mesh metadata array.
    Metadata,
    /// The joint matrix array.
    Joints,
    /// The morph weight array.
    MorphWeights,
    /// The morph displacement array.
    MorphDeltas,
    /// The light records.
    Lights,
    /// The light cluster cells.
    LightCells,
    /// The polygon-light vertices.
    LightVertices,
    /// The cluster grid uniform.
    Cluster,
    /// The ambient and directional light uniform.
    Lighting,
}

impl GlobalSlot {
    /// The shader binding number of this resource.
    pub const fn binding(self) -> u32 {
        match self {
            Self::Camera => CAMERA_BINDING,
            Self::Frame => FRAME_BINDING,
            Self::View => VIEW_BINDING,
            Self::Metadata => MESH_METADATA_BINDING,
            Self::Joints => JOINTS_BINDING,
            Self::MorphWeights => MORPH_WEIGHTS_BINDING,
            Self::MorphDeltas => MORPH_DELTAS_BINDING,
            Self::Lights => LIGHTS_BINDING,
            Self::LightCells => LIGHT_CELLS_BINDING,
            Self::LightVertices => LIGHT_VERTICES_BINDING,
            Self::Cluster => CLUSTER_BINDING,
            Self::Lighting => LIGHTING_BINDING,
        }
    }
}

#[derive(Clone, Copy)]
enum BindingKind {
    Uniform(core::num::NonZeroU64),
    Array(u64),
}

use BindingKind::{Array, Uniform};
use GlobalSlot::{
    Camera, Cluster, Frame, Joints, LightCells, LightVertices, Lighting, Lights, Metadata,
    MorphDeltas, MorphWeights, View,
};
use const_shader_layout::ShaderLayout;
use wgpu::ShaderStages as Stages;

const VERTEX_FRAGMENT: Stages = Stages::VERTEX.union(Stages::FRAGMENT);
const UNIFORMS: &[(GlobalSlot, Stages, BindingKind)] = &[
    (
        Camera,
        VERTEX_FRAGMENT,
        Uniform(crate::globals::Camera::SIZE),
    ),
    (
        Frame,
        Stages::VERTEX,
        Uniform(crate::globals::Globals::SIZE),
    ),
    (View, VERTEX_FRAGMENT, Uniform(crate::globals::View::SIZE)),
];
const MESH: &[(GlobalSlot, Stages, BindingKind)] = &[(
    Metadata,
    Stages::VERTEX,
    Array(crate::mesh::MeshMetadata::SIZE.get()),
)];
const SKIN: &[(GlobalSlot, Stages, BindingKind)] = &[(
    Joints,
    Stages::VERTEX,
    Array(crate::mesh::JointMatrix::SIZE.get()),
)];
const MORPH: &[(GlobalSlot, Stages, BindingKind)] = &[
    (MorphWeights, Stages::VERTEX, Array(size_of::<f32>() as u64)),
    (MorphDeltas, Stages::VERTEX, Array(size_of::<f32>() as u64)),
];
const LIGHT: &[(GlobalSlot, Stages, BindingKind)] = &[
    (
        Lights,
        Stages::FRAGMENT,
        Array(crate::light::LightGpu::SIZE.get()),
    ),
    (
        LightCells,
        Stages::FRAGMENT,
        Array((crate::light::CELL_WORDS * size_of::<u32>()) as u64),
    ),
    (
        LightVertices,
        Stages::FRAGMENT,
        Array(size_of::<glam::Vec4>() as u64),
    ),
    (
        Cluster,
        Stages::FRAGMENT,
        Uniform(crate::light::ClusterUniform::SIZE),
    ),
    (
        Lighting,
        Stages::FRAGMENT,
        Uniform(crate::light::LightingUniform::SIZE),
    ),
];

/// Inline capacity for all shared global bindings enabled together.
pub const MAX_GLOBAL_BINDINGS: usize =
    UNIFORMS.len() + MESH.len() + SKIN.len() + MORPH.len() + LIGHT.len();

impl GlobalBindings {
    fn descriptors(self) -> impl Iterator<Item = (GlobalSlot, Stages, BindingKind)> {
        [
            Some(UNIFORMS),
            self.metadata.then_some(MESH),
            self.joints.then_some(SKIN),
            self.morphs.then_some(MORPH),
            self.lights.then_some(LIGHT),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .copied()
    }

    /// The enabled resources, in shader binding order.
    pub fn slots(self) -> impl Iterator<Item = GlobalSlot> {
        self.descriptors().map(|(slot, _, _)| slot)
    }
}

/// Build the shared global bind-group layout for the requested resources.
///
/// The camera, frame and view uniforms are always present. `texel_arrays`
/// selects texture-backed arrays instead of storage buffers on devices that
/// do not support the latter, or when explicitly requested.
pub fn global_bind_group_layout(
    device: &wgpu::Device,
    bindings: GlobalBindings,
    texel_arrays: bool,
) -> wgpu::BindGroupLayout {
    let entries: arrayvec::ArrayVec<_, MAX_GLOBAL_BINDINGS> = bindings
        .descriptors()
        .map(|(slot, visibility, kind)| {
            let binding = slot.binding();
            match kind {
                Array(size) => array_entry(binding, size, visibility, texel_arrays),
                Uniform(size) => wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(size),
                    },
                    count: None,
                },
            }
        })
        .collect();
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit_wgpu::lit::globals"),
        entries: &entries,
    })
}

#[cfg(test)]
#[test]
fn the_shared_layout_contains_each_binding_once_in_shader_order() {
    let bindings = GlobalBindings {
        metadata: true,
        joints: true,
        morphs: true,
        lights: true,
    };
    assert!(
        bindings
            .slots()
            .map(GlobalSlot::binding)
            .eq(0..MAX_GLOBAL_BINDINGS as u32)
    );
}

/// The bind-group layouts a [`SpecializedLitPipeline`] variant declares.
///
/// Built by [`LitVariant::bind_group_layouts`] from the same variant a
/// pipeline is built from, so the two agree: the global group always exists and
/// the material group only for a variant that samples a base-color texture. The
/// mesh group is always `None` — the built-in variants bind nothing there — but
/// stays in the structure because the index is a general extension point.
#[derive(Clone, Debug)]
pub struct LitBindGroupLayouts {
    /// Layout of the global bind group (index 0).
    pub global: wgpu::BindGroupLayout,
    /// Layout of the material bind group (index 1), for a variant that
    /// samples a base-color texture.
    pub material: Option<wgpu::BindGroupLayout>,
    /// Layout of the mesh bind group (index 2).
    ///
    /// Always `None` for the built-in variants: every input they read is in the
    /// global group or on the instance stream.
    pub mesh: Option<wgpu::BindGroupLayout>,
}

/// The built-in lit pipeline: a compiled [wgpu::RenderPipeline] together
/// with the [`LitVariant`] it was built from.
///
/// [`LitVariant`] is its own [`PipelineVariant`](crate::specialize::PipelineVariant) and
/// [`PipelineDescriptor`](crate::specialize::PipelineDescriptor),
/// so the built-in shader compiles through the same cache a caller's own
/// variant does, and the layouts it declares are derived from the variant
/// rather than stored beside the pipeline.
pub type SpecializedLitPipeline =
    crate::specialize::SpecializedPipeline<wgpu::RenderPipeline, LitVariant>;

/// Whether `device` can read a storage buffer from a shader.
///
/// WebGL2 cannot: it has no storage buffers at all, and a bind-group layout
/// naming one is rejected outright. Such a device reports a limit of zero,
/// which is the only signal available — [`wgpu::Device`] exposes its limits and
/// features but not the downlevel capabilities that would say so directly.
///
/// A device that can bind storage buffers reports a non-zero limit, so this is
/// what decides between the built-in shader's buffer and texel array paths.
pub fn supports_storage_buffers(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage > 0 && limits.max_storage_buffer_binding_size > 0
}
