use super::*;

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

/// Build the global bind-group layout a pipeline with `bindings` declares.
///
/// The camera, frame-globals and view uniforms are always present; the arrays
/// are present exactly when `bindings` names them. The array path is the
/// device's answer rather than the pipeline's, so `texel_arrays` is passed in: a
/// device with storage buffers binds each array as one, a device without them —
/// WebGL2 — binds the same bytes as a texture.
///
/// The built-in lit variants build their global layout here, so a caller's own
/// pipeline declares the same group by the same rules.
pub fn global_bind_group_layout(
    device: &wgpu::Device,
    bindings: GlobalBindings,
    texel_arrays: bool,
) -> wgpu::BindGroupLayout {
    let mut entries = arrayvec::ArrayVec::<wgpu::BindGroupLayoutEntry, 12>::new();
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: CAMERA_BINDING,
        // The near plane and the eye position are view values a fragment stage
        // may want to reason with: the lit pipeline derives its depth slices
        // and its fallback geometric normal from them.
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: Some(
                <crate::globals::Camera as const_shader_layout::ShaderLayout>::SIZE,
            ),
        },
        count: None,
    });
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: FRAME_BINDING,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: Some(
                <crate::globals::Globals as const_shader_layout::ShaderLayout>::SIZE,
            ),
        },
        count: None,
    });
    // The pass's view. Every pipeline declares it beside the camera and the
    // frame clock, because the group is the frame's shared one and the viewport
    // belongs to it as much as the other two: a pipeline that never measures in
    // pixels simply does not name the binding in its shader.
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: VIEW_BINDING,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: Some(
                <crate::globals::View as const_shader_layout::ShaderLayout>::SIZE,
            ),
        },
        count: None,
    });
    if bindings.metadata {
        entries.push(array_entry(
            MESH_METADATA_BINDING,
            <crate::mesh::MeshMetadata as const_shader_layout::ShaderLayout>::SIZE.get(),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.joints {
        entries.push(array_entry(
            JOINTS_BINDING,
            <crate::mesh::JointMatrix as const_shader_layout::ShaderLayout>::SIZE.get(),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.morphs {
        entries.push(array_entry(
            MORPH_WEIGHTS_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<f32>() as u64),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
        // One position component of one target: like every other array entry
        // the storage binding minimum is a single element, so a mesh joining or
        // leaving the pool never invalidates the layout.
        entries.push(array_entry(
            MORPH_DELTAS_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<f32>() as u64),
            wgpu::ShaderStages::VERTEX,
            texel_arrays,
        ));
    }
    if bindings.lights {
        // Lighting happens in the fragment stage, where the view-space position
        // and normal are; the vertex stage reads none of these, so declaring
        // them there would only spend the vertex stage's array quota.
        let visibility = wgpu::ShaderStages::FRAGMENT;
        entries.push(array_entry(
            LIGHTS_BINDING,
            <crate::light::LightGpu as const_shader_layout::ShaderLayout>::SIZE.get(),
            visibility,
            texel_arrays,
        ));
        // One cell is a fixed number of words rather than one: the record lists
        // the lights assigned to the cell, so the binding minimum is a whole
        // record and a fragment reads the cell's own slice.
        entries.push(array_entry(
            LIGHT_CELLS_BINDING,
            wgpu::BufferAddress::from(
                (crate::light::CELL_WORDS * core::mem::size_of::<u32>()) as u64,
            ),
            visibility,
            texel_arrays,
        ));
        entries.push(array_entry(
            LIGHT_VERTICES_BINDING,
            wgpu::BufferAddress::from(core::mem::size_of::<glam::Vec4>() as u64),
            visibility,
            texel_arrays,
        ));
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: CLUSTER_BINDING,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: Some(
                    <crate::light::ClusterUniform as const_shader_layout::ShaderLayout>::SIZE,
                ),
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: LIGHTING_BINDING,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: Some(
                    <crate::light::LightingUniform as const_shader_layout::ShaderLayout>::SIZE,
                ),
            },
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit_wgpu::lit::globals"),
        entries: &entries,
    })
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
/// [`LitVariant`] is its own [`PipelineVariant`] and [`PipelineDescriptor`],
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
