use super::*;

/// The sky family's uniform block, as the shader reads it.
///
/// The example writes this every frame: the three band colours, the sun's
/// direction and colour, the view-to-world rotation the ray is built through,
/// and the projection's half-angle tangents. The last two are the reason the
/// sky needs no camera of its own — it derives the ray from the frame's camera
/// rather than from a hard-coded field of view.
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
pub(super) struct SkyUniforms {
    /// The colour overhead.
    pub(super) zenith: glam::Vec4,
    /// The colour at the horizon.
    pub(super) horizon: glam::Vec4,
    /// The colour below the horizon.
    pub(super) ground: glam::Vec4,
    /// The sun's world-space direction, with the cloud opacity in w.
    pub(super) sun_direction: glam::Vec4,
    /// The sun's colour.
    pub(super) sun_color: glam::Vec4,
    /// The rotation the view ray is taken into world space through.
    pub(super) view_rotation: glam::Mat4,
    /// The projection's half-angle tangents in x and y.
    pub(super) projection: glam::Vec4,
}

/// The scene's sky: the mesh that covers the target and the pipeline that
/// shades it.
///
/// The pipeline is compiled for the mesh's vertex layout, so the two are
/// spawned as one object rather than as two independent components.
pub(super) struct Sky {
    pub(super) mesh: GpuMesh,
    pub(super) pipeline: GpuRenderPipeline,
}

impl Bundle for Sky {
    fn into_bundle(self) -> DynamicBundle {
        bundle![self.mesh, self.pipeline]
    }
}

/// The identity of the sky family's pipeline, keyed on the compiled descriptor.
///
/// The descriptor is immutable once built, so two keys that share it are the
/// same pipeline; the key compares the allocation rather than the contents.
#[derive(Clone)]
pub(super) struct SkyKey {
    pub(super) descriptor: Arc<RenderPipelineDesc>,
}

impl PartialEq for SkyKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for SkyKey {}

impl core::hash::Hash for SkyKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

/// The sky family's blueprint, specialised for one render target.
#[derive(Clone, Debug)]
pub(super) struct SkyVariant {
    /// The pipeline as built, before the target is applied.
    pub(super) descriptor: Arc<RenderPipelineDesc>,
    /// The target the pipeline is compiled for.
    pub(super) surface: SurfaceKey,
}

impl PartialEq for SkyVariant {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface && Arc::ptr_eq(&self.descriptor, &other.descriptor)
    }
}

impl Eq for SkyVariant {}

impl core::hash::Hash for SkyVariant {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
        state.write_usize(Arc::as_ptr(&self.descriptor) as usize);
    }
}

impl RenderPipelineKey for SkyKey {
    type Variant = SkyVariant;

    fn variant(&self, draw: &DrawContext<'_>) -> SkyVariant {
        SkyVariant {
            descriptor: self.descriptor.clone(),
            surface: draw.surface,
        }
    }
}

impl PipelineVariant<wgpu::RenderPipeline> for SkyVariant {
    type Descriptor = RenderPipelineDesc;

    fn descriptor(&self, _device: &wgpu::Device) -> RenderPipelineDesc {
        // The stored descriptor carries the format the example draws its own
        // captures with; rewriting the target here is what lets the same family
        // draw into a window's swap chain, whose colour format differs.
        let mut descriptor = self.descriptor.as_ref().clone();
        descriptor.set_surface(self.surface);
        descriptor
    }
}

/// The sky family's factory.
///
/// The family owns its whole group 0: its shader reads a uniform block, a cube
/// map and a sampler the example keeps, and nothing of the frame's. The recipe
/// it hands the renderer therefore declares exactly those three as its inputs
/// ([GlobalRecipe::own]), so the group is rebuilt when the example replaces one
/// of them and never because a draw moved to another camera.
pub(super) struct SkyFactory {
    /// The layout the group must match.
    pub(super) layout: wgpu::BindGroupLayout,
    /// The uniform block the group binds.
    pub(super) uniforms: ResHandle<wgpu::Buffer>,
    /// The cube map's view.
    pub(super) view: ResHandle<TextureView>,
    /// The cube map's sampler.
    pub(super) sampler: ResHandle<wgpu::Sampler>,
}

impl RenderPipelineFactory<RenderPipelineDesc> for SkyFactory {
    fn descriptor(
        &self,
        _context: &FamilyContext<'_>,
        value: &SpecializedPipeline<wgpu::RenderPipeline, RenderPipelineDesc>,
    ) -> RegisteredRenderPipeline {
        let layout = self.layout.clone();
        let uniforms = self.uniforms.clone();
        let view = self.view.clone();
        let sampler = self.sampler.clone();
        let inputs = vec![uniforms.clone().erase(), view.clone().erase()];
        let mut inputs = inputs;
        inputs.push(sampler.clone().erase());
        let build =
            move |device: &wgpu::Device, graph: &ResourceGraph, _uniforms: &FrameUniforms| {
                let uniforms = graph
                    .get(&uniforms)
                    .expect("the sky's uniform buffer is in the graph");
                let view = graph
                    .get(&view)
                    .expect("the sky's cube view is in the graph");
                let sampler = graph
                    .get(&sampler)
                    .expect("the sky's sampler is in the graph");
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("unlit3d_examples::custom_pipeline::sky"),
                    layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(view.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                })
            };
        RegisteredRenderPipeline {
            pipeline: value.pipeline.clone(),
            global: Some(GlobalRecipe::own(build, inputs)),
        }
    }
}

/// The layout of the sky family's one bind group.
pub(super) fn sky_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        <SkyUniforms as const_shader_layout::ShaderLayout>::SIZE.get(),
                    ),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Compile the sky family's pipeline against the family's bind group layout.
pub(super) fn sky_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> RenderPipelineDesc {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        source: wgpu::ShaderSource::Wgsl(SKY_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            // The triangle is generated from the vertex index.
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: unlit_wgpu::render_attachments::default_depth_stencil_format(device),
            // The sky is the backdrop: it is tested against the depth buffer so
            // anything opaque in front of it wins, but it writes nothing, so it
            // never hides a draw that comes after it.
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
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
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    };
    RenderPipelineDesc::from_wgpu(&descriptor)
}

/// Create the procedural cube map and fill each of its six faces.
///
/// The faces are generated on the CPU from a deterministic hash of the
/// direction each texel looks along, so the pattern is continuous across face
/// seams and identical on every machine — which is what lets a snapshot pin it.
pub(super) fn upload_sky_cubemap(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let size = SKY_FACE_SIZE;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unlit3d_examples::custom_pipeline::sky_cube"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SKY_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texel_size = SKY_FORMAT
        .block_copy_size(None)
        .expect("the sky format copies whole texels") as usize;
    let mut face = vec![0u8; (size * size) as usize * texel_size];
    for layer in 0..6u32 {
        for y in 0..size {
            for x in 0..size {
                let u = (x as f32 + 0.5) / size as f32;
                let v = (y as f32 + 0.5) / size as f32;
                let texel = sky_texel(cube_direction(layer, u, v));
                let start = ((y * size + x) as usize) * texel_size;
                for (channel, value) in texel.iter().enumerate() {
                    face[start + channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &face,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * texel_size as u32),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
    }
    texture
}

/// The direction a cube-map texel looks along.
///
/// The faces are in the order a cube map samples them — +X, -X, +Y, -Y, +Z, -Z
/// — and the pair (u, v) runs across the face from its top-left.
pub(super) fn cube_direction(face: u32, u: f32, v: f32) -> glam::Vec3 {
    let sc = 2.0 * u - 1.0;
    let tc = 2.0 * v - 1.0;
    let direction = match face {
        0 => glam::Vec3::new(1.0, -tc, -sc),
        1 => glam::Vec3::new(-1.0, -tc, sc),
        2 => glam::Vec3::new(sc, 1.0, tc),
        3 => glam::Vec3::new(sc, -1.0, -tc),
        4 => glam::Vec3::new(sc, -tc, 1.0),
        _ => glam::Vec3::new(-sc, -tc, -1.0),
    };
    direction.normalize()
}

/// The colour one cube-map texel takes, from the direction it looks along.
///
/// The map holds two flat cloud colours and nothing else: a texel is either
/// inside a cloud or it is not, so the shader can lay it over the sky with one
/// blend and no falloff. The alpha of the result says whether there is a cloud
/// there at all.
pub(super) fn sky_texel(direction: glam::Vec3) -> [f32; 4] {
    let puff = value_noise(direction * 9.0);
    if puff < 0.66 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    // Two flat tones, picked by the same noise, so a cloud reads as a drawn
    // shape rather than as a shaded one.
    let colour = if puff > 0.72 {
        glam::Vec3::new(1.00, 0.86, 0.92)
    } else {
        glam::Vec3::new(0.72, 0.62, 0.88)
    };
    [colour.x, colour.y, colour.z, 1.0]
}

/// Trilinearly interpolated value noise over the integer lattice.
pub(super) fn value_noise(point: glam::Vec3) -> f32 {
    let base = point.floor();
    let fraction = point - base;
    // The interpolation is smoothed so a cloud's outline is a clean curve
    // rather than the staircase of the lattice it was sampled on.
    let smooth = fraction * fraction * (glam::Vec3::splat(3.0) - fraction * 2.0);
    let (x, y, z) = (base.x as i32, base.y as i32, base.z as i32);
    let mut value = 0.0;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let corner = glam::Vec3::new(
                    if dx == 1 { smooth.x } else { 1.0 - smooth.x },
                    if dy == 1 { smooth.y } else { 1.0 - smooth.y },
                    if dz == 1 { smooth.z } else { 1.0 - smooth.z },
                );
                value += corner.x * corner.y * corner.z * hash3(x + dx, y + dy, z + dz);
            }
        }
    }
    value
}

/// A deterministic hash of three integers into the unit interval.
pub(super) fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut hash = (x as u32).wrapping_mul(0x9E37_79B9)
        ^ (y as u32).wrapping_mul(0x85EB_CA6B)
        ^ (z as u32).wrapping_mul(0xC2B2_AE35);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2C1B_3C6D);
    hash ^= hash >> 12;
    hash = hash.wrapping_mul(0x297A_2D39);
    hash ^= hash >> 15;
    (hash >> 8) as f32 / (1u32 << 24) as f32
}
