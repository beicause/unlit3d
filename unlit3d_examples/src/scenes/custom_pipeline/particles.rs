use super::*;

/// The particle family's uniform block, as the shader reads it.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayoutCompat,
)]
pub(super) struct ParticleUniforms {
    /// The scene clock in x, in seconds.
    pub(super) clock: glam::Vec4,
}

/// One particle's record on the instance stream.
///
/// The layout is the example's own: three vec4s where the built-in family's
/// record has a transform, a colour and a handful of indices.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
)]
pub(super) struct ParticleRecord {
    /// The orbit's centre in xyz, with the phase offset in w.
    pub(super) origin: [f32; 4],
    /// The orbit's radius in x, its speed in y, the sprite's size in z and the
    /// vertical bob's amplitude in w.
    pub(super) motion: [f32; 4],
    /// The sprite's colour in rgb and its shape in a.
    pub(super) tint: [f32; 4],
}

/// The per-particle data the family reads, as a plain world component.
#[derive(Clone, Copy, Debug)]
pub(super) struct Particle {
    /// The centre the particle orbits.
    pub(super) origin: glam::Vec3,
    /// The offset into the orbit's cycle.
    pub(super) phase: f32,
    /// The radius of the orbit.
    pub(super) radius: f32,
    /// The angular speed of the orbit, in radians per second.
    pub(super) speed: f32,
    /// The sprite's half-size, in metres.
    pub(super) size: f32,
    /// How far the particle bobs vertically.
    pub(super) bob: f32,
    /// Which flat shape the sprite is: a disc, a star or a ring.
    pub(super) shape: f32,
    /// The sprite's colour.
    pub(super) tint: glam::Vec4,
}

/// One particle of the cloud.
///
/// The mesh, the pipeline and the material are shared by the whole cloud — one
/// of each, so the renderer folds every sprite into a single instanced draw —
/// while the record is what places this one; a spawn states all four together.
pub(super) struct Sprite {
    pub(super) mesh: GpuMesh,
    pub(super) pipeline: GpuRenderPipeline,
    pub(super) material: GpuMaterial,
    pub(super) particle: Particle,
}

impl Bundle for Sprite {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.mesh, self.pipeline, self.material, self.particle]
    }
}

/// The identity of the particle family's pipeline.
#[derive(Clone)]
pub(super) struct ParticleKey {
    pub(super) descriptor: Arc<RenderPipelineDesc>,
}

impl PartialEq for ParticleKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for ParticleKey {}

impl core::hash::Hash for ParticleKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

/// The particle family's blueprint, specialised for one render target.
#[derive(Clone, Debug)]
pub(super) struct ParticleVariant {
    /// The pipeline as built, before the target is applied.
    pub(super) descriptor: Arc<RenderPipelineDesc>,
    /// The target the pipeline is compiled for.
    pub(super) surface: SurfaceKey,
}

impl PartialEq for ParticleVariant {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface && Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for ParticleVariant {}

impl core::hash::Hash for ParticleVariant {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

impl RenderPipelineKey for ParticleKey {
    type Variant = ParticleVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> ParticleVariant {
        ParticleVariant {
            descriptor: self.descriptor.clone(),
            surface: draw.surface,
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for ParticleVariant {
    type Descriptor = RenderPipelineDesc;

    fn descriptor(&self, _device: &wgpu::Device) -> RenderPipelineDesc {
        let mut descriptor = self.descriptor.as_ref().clone();
        descriptor.set_surface(self.surface);
        descriptor
    }
}

/// The particle family's factory.
///
/// The family keeps the renderer's shared global group, so its group is built
/// from the frame's own camera and globals exactly as a built-in family's is.
pub(super) struct ParticleFactory;

impl RenderPipelineFactory<RenderPipelineDesc> for ParticleFactory {
    fn descriptor(
        &self,
        context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, RenderPipelineDesc>,
    ) -> RegisteredRenderPipeline {
        let bindings = GlobalBindings::default();
        let layout = context.resources.layout(context.device, bindings);
        let global = context.resources.recipe(layout, bindings);
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(global),
        }
    }
}

/// The particle family's per-instance stream.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ParticleInstances;

impl InstanceData for ParticleInstances {
    fn stream(&self) -> InstanceStreamDesc {
        InstanceStreamDesc {
            slot: PARTICLE_INSTANCE_SLOT,
            array_stride: core::mem::size_of::<ParticleRecord>() as u32,
            label: None,
        }
    }

    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]) {
        let mut particle = context
            .world
            .query_one::<&Particle>(context.entity)
            .expect("every visible particle carries a Particle component");
        let record = ParticleRecord {
            origin: [
                particle.get().origin.x,
                particle.get().origin.y,
                particle.get().origin.z,
                particle.get().phase,
            ],
            motion: [
                particle.get().radius,
                particle.get().speed,
                particle.get().size,
                particle.get().bob,
            ],
            tint: [
                particle.get().tint.x,
                particle.get().tint.y,
                particle.get().tint.z,
                particle.get().shape,
            ],
        };
        out.copy_from_slice(record.as_bytes());
    }
}

/// The layout of the particle family's material group.
pub(super) fn particle_material_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(
                    <ParticleUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
                ),
            },
            count: None,
        }],
    })
}

/// Compile the particle family's pipeline.
///
/// Slot 0 is left empty — the mesh has no vertex buffer — and slot 1 is the
/// family's own instance stream. Group 0 is the renderer's shared global group
/// and group 1 is the family's uniform block.
pub(super) fn particle_pipeline(
    device: &wgpu::Device,
    material: &wgpu::BindGroupLayout,
) -> RenderPipelineDesc {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        source: wgpu::ShaderSource::Wgsl(PARTICLE_SHADER.into()),
    });
    let global = global_bind_group_layout(
        device,
        GlobalBindings::default(),
        !supports_storage_buffers(device),
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        bind_group_layouts: &[Some(&global), Some(material)],
        immediate_size: 0,
    });
    let descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::particles"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[None, Some(particle_instance_layout())],
        },
        primitive: wgpu::PrimitiveState {
            // A billboard's winding flips with the camera, so neither face is
            // culled.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: unlit_wgpu::render_attachments::default_depth_stencil_format(device),
            // The sprites are opaque, so they write depth and occlude one
            // another; the shapes are flat, so overlapping them would only
            // flatten the cloud into one silhouette.
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TEST_FORMAT,
                // Opaque: a sprite's colour is exactly its tint, with no
                // blending to soften it.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    RenderPipelineDesc::from_wgpu(&descriptor)
}

/// The instance-step vertex layout of one particle record.
pub(super) fn particle_instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<ParticleRecord>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

/// The particle cloud: a ring of sprites, each with its own orbit, size, hue
/// and shape.
pub(super) fn particle_cloud() -> [Particle; PARTICLE_COUNT] {
    core::array::from_fn(|index| {
        let slot = index as f32;
        let angle = slot * core::f32::consts::TAU / PARTICLE_COUNT as f32;
        // Alternating radii keep the cloud from reading as a flat ring.
        let ring = if index.is_multiple_of(2) { 1.35 } else { 2.05 };
        Particle {
            origin: glam::Vec3::new(
                angle.cos() * ring,
                1.05 + (index % 3) as f32 * 0.28,
                angle.sin() * ring,
            ),
            phase: slot * 0.9,
            radius: 0.22 + (index % 3) as f32 * 0.11,
            speed: 0.75 + (index % 4) as f32 * 0.22,
            size: 0.20 + (index % 3) as f32 * 0.06,
            bob: 0.16 + (index % 2) as f32 * 0.12,
            shape: (index % 3) as f32,
            tint: PARTICLE_TINTS[index % PARTICLE_TINTS.len()],
        }
    })
}
