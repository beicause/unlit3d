//! The scene every lit test renders: a cube, its texture and the variant that draws it.

use super::*;

/// A uniformly scaled, Y-rotated instance at `translation`.
pub(super) fn placed(
    scale: f32,
    rotation_y: f32,
    translation: glam::Vec3,
    base_color: [f32; 4],
) -> MeshInstance {
    MeshInstance::new(
        glam::Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(scale),
            glam::Quat::from_rotation_y(rotation_y),
            translation,
        ),
        glam::Vec4::from(base_color),
    )
}

/// A mesh uploaded in the built-in pipeline's compressed vertex layout.
pub(super) struct GpuMesh {
    /// Slot 0: `Snorm16x4` positions; `None` when the variant declares no
    /// position stream.
    pub(super) positions: Option<wgpu::Buffer>,
    /// Slot 1: the secondary stream, interleaved by the variant's
    /// [`unlit_wgpu::mesh::SecondaryStreamWriter`]: `Snorm16x2` UVs,
    /// `Unorm8x4` colors and `Snorm16x2` normals, in attribute order; `None`
    /// when the variant declares no channel.
    pub(super) secondary: Option<wgpu::Buffer>,
    /// Index buffer and its index count, when the mesh is drawn indexed.
    pub(super) indices: Option<(wgpu::Buffer, u32)>,
    /// Number of vertices.
    pub(super) vertex_count: u32,
    /// Per-mesh decode parameters.
    pub(super) metadata: MeshMetadata,
}

impl GpuMesh {
    /// Compress and upload the channels `variant` declares, narrowing
    /// `indices` to `u16`. Channels the variant does not declare are neither
    /// compressed nor uploaded, so a position-less variant has no position
    /// buffer at all. An empty `indices` slice uploads no index buffer.
    #[expect(clippy::too_many_arguments, reason = "one parameter per mesh channel")]
    fn upload(
        ctx: &Ctx,
        label: &str,
        variant: &LitVariant,
        positions: &[[f32; 3]],
        uvs: &[[f32; 2]],
        colors: &[[u8; 4]],
        normals: &[[f32; 3]],
        indices: &[u32],
    ) -> Self {
        let stream = variant.secondary_stream();
        let mut metadata = MeshMetadata::default();
        let compressed_positions = matches!(
            variant.channels.position.position,
            Some(ChannelEncoding::CompressedPosition)
        );
        let packed_positions: Vec<_> = if compressed_positions {
            compress_positions(positions, &mut metadata).collect()
        } else {
            Vec::new()
        };

        let uploaded = |ctx: &Ctx, contents: &[u8], usage: wgpu::BufferUsages, name: String| {
            upload_buffer(ctx, &name, contents, usage)
        };

        let vertex_usage = wgpu::BufferUsages::VERTEX;
        // An uncompressed position needs no decode parameters, so it is
        // uploaded exactly as the caller supplies it.
        let positions_buffer = variant.channels.position.position.is_some().then(|| {
            let bytes = if compressed_positions {
                packed_positions.as_bytes()
            } else {
                positions.as_bytes()
            };
            uploaded(ctx, bytes, vertex_usage, format!("{label}::positions"))
        });
        // The stream writer compresses and writes into the mapped buffer in
        // one step, so no intermediate byte buffer exists.
        let secondary_buffer = (!stream.is_empty()).then(|| {
            let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label}::secondary")),
                size: stream.byte_len(positions.len()) as u64,
                usage: vertex_usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            {
                let mut view = buffer.slice(..).get_mapped_range_mut().expect("mapped");
                stream.write(
                    uvs,
                    colors,
                    normals,
                    &mut metadata,
                    view.slice(..).into_slice(..),
                );
            }
            buffer.unmap();
            buffer
        });

        // Indices only address geometry, so a position-less variant draws its
        // point range unindexed.
        let indices =
            (variant.channels.position.position.is_some() && !indices.is_empty()).then(|| {
                let narrowed: Vec<u16> = compress_indices(indices).expect("indices").collect();
                let buffer = uploaded(
                    ctx,
                    narrowed.as_bytes(),
                    wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    format!("{label}::indices"),
                );
                (buffer, narrowed.len() as u32)
            });

        Self {
            positions: positions_buffer,
            secondary: secondary_buffer,
            indices,
            vertex_count: positions.len() as u32,
            metadata,
        }
    }
}

/// Upload `bytes` as a `usage`-flagged buffer, writing into the buffer's own
/// memory so no staging copy is involved.
pub(super) fn upload_buffer(
    ctx: &Ctx,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .expect("mapped at creation")
        .copy_from_slice(bytes);
    buffer.unmap();
    buffer
}

/// A perspective world -> clip matrix looking at the origin from above and in
/// front, with reverse-z (the built-in pipeline compares with `Greater`).
///
/// Uses the WebGPU clip convention (Z in `[0, 1]`, Y-up).
pub(super) fn camera(aspect: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = glam::Vec3::new(0.0, 1.2, 3.2);
    let view =
        glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::new(0.0, 0.2, 0.0), glam::Vec3::Y);
    Camera::new(view, projection, eye)
}

/// A CPU-side mesh: positions, UVs, vertex colors, normals and `u32` indices.
pub(super) type MeshData = (
    Vec<[f32; 3]>,
    Vec<[f32; 2]>,
    Vec<[u8; 4]>,
    Vec<[f32; 3]>,
    Vec<u32>,
);

/// A unit cube centred on the origin, with UVs, vertex colors and normals that
/// exercise every decode path.
pub(super) fn cube() -> MeshData {
    // Per face: outward normal and tangent axis. The bitangent is derived as
    // normal x tangent, which winds every face counter-clockwise as seen from
    // outside so it survives back-face culling.
    let faces = [
        ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, 1.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ];

    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();

    for (normal, tangent) in faces {
        let base = positions.len() as u32;
        let face_normal = normal;
        let normal = glam::Vec3::from(normal);
        let tangent = glam::Vec3::from(tangent);
        let bitangent = normal.cross(tangent);
        for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let position = normal + tangent * u + bitangent * v;
            positions.push(position.to_array());
            uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            // Color from the position itself, not the per-face UV: a corner
            // shared by three faces then carries one color, so the gradients
            // meet seamlessly across faces.
            let color = (position + 1.0) * 0.5;
            colors.push([color.x, color.y, color.z, 1.0]);
            normals.push(face_normal);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // The vertex stream stores colors as `Unorm8x4`; quantize them once
    // here so the fixture hands the writer the width it uploads at.
    let colors = quantize_colors(&colors).collect();
    (positions, uvs, colors, normals, indices)
}

/// A procedurally generated base-color texture plus its sampler.
pub(super) struct BaseColorTexture {
    pub(super) view: TextureView,
    pub(super) sampler: wgpu::Sampler,
}

/// An 8x8 checkerboard: sampling it makes the UV decode visible in the frame.
pub(super) fn checkerboard_texture(ctx: &Ctx) -> BaseColorTexture {
    const SIZE: u32 = 8;
    let mut texels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let value = if (x + y) % 2 == 0 { 255u8 } else { 40 };
            texels.extend_from_slice(&[value, value, value, 255]);
        }
    }

    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test::base_color"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    ctx.queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );

    BaseColorTexture {
        view: TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default()),
        sampler: ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("test::base_color_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        }),
    }
}

/// Everything one test frame needs: the pipeline variant and the meshes
/// drawn with it.
pub(super) struct SceneFixture {
    /// Whether the pipeline declares a depth-stencil state, which the render
    /// pass must then match: `wgpu` rejects a mismatch when binding it.
    pub(super) has_depth: bool,
    pub(super) pipeline: SpecializedLitPipeline,
    pub(super) mesh: GpuMesh,
    pub(super) material: Option<wgpu::BindGroup>,
    /// Multisample state the pipeline was compiled for; the render pass's
    /// attachments must match it.
    pub(super) multisample: wgpu::MultisampleState,
}

/// Build the built-in pipeline for `variant`, upload the cube and — when the
/// variant samples a base-color texture — create its material bind group.
///
/// The variant's surface is rewritten for the test's render target: the color
/// format and sample count come from here, while the depth attachment is the
/// caller surface's word — a depth-less surface stays depth-less, which is
/// what a color-only target needs from the pipelines bound into it.
pub(super) fn fixture(ctx: &Ctx, variant: &LitVariant, sample_count: u32) -> SceneFixture {
    let mut variant = variant.clone();
    variant.surface = SurfaceKey {
        color_format: COLOR_FORMAT,
        // The caller's surface states the depth attachment, and a depth-less
        // one stays depth-less: that is exactly what a color-only target
        // needs from the pipelines bound into it.
        depth_stencil_format: variant.surface.depth_stencil_format,
        sample_count,
    };
    let pipeline =
        SpecializedPipeline::create(&ctx.device, variant.clone(), &mut ShaderCache::new());

    let (positions, uvs, colors, normals, indices) = cube();
    let mesh = GpuMesh::upload(
        ctx,
        "test::cube",
        &variant,
        &positions,
        &uvs,
        &colors,
        &normals,
        &indices,
    );

    let material = variant.base_color_texture.then(|| {
        let texture = checkerboard_texture(ctx);
        ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test::material"),
            layout: &pipeline
                .descriptor()
                .bind_group_layouts(&ctx.device)
                .material
                .expect("a textured variant has a material layout"),
            entries: &[
                bg_entry(
                    BASE_COLOR_TEXTURE_BINDING,
                    wgpu::BindingResource::TextureView(texture.view.view()),
                ),
                bg_entry(
                    BASE_COLOR_SAMPLER_BINDING,
                    wgpu::BindingResource::Sampler(&texture.sampler),
                ),
            ],
        })
    });

    SceneFixture {
        has_depth: variant.surface.depth_stencil_format.is_some(),
        pipeline,
        mesh,
        material,
        multisample: wgpu::MultisampleState {
            count: sample_count,
            ..Default::default()
        },
    }
}

/// Build a render target for the test: a persistent color texture, a transient
/// depth texture, and — when `sample_count > 1` — a transient multisample
/// texture. Returns the attachment set and the color texture (for readback).
pub(super) fn render_target(ctx: &Ctx, sample_count: u32) -> (RenderAttachments, wgpu::Texture) {
    render_target_with_depth(ctx, sample_count, true)
}

/// Like [`render_target`], but may omit the depth-stencil attachment — which a
/// pipeline must then also omit.
pub(super) fn render_target_with_depth(
    ctx: &Ctx,
    sample_count: u32,
    with_depth: bool,
) -> (RenderAttachments, wgpu::Texture) {
    let ft = create_render_target(&ctx.device, COLOR_FORMAT, WIDTH, HEIGHT, sample_count);
    if with_depth {
        return (ft.attachments, ft.color);
    }
    let attachments = RenderAttachments::from_views(
        ft.attachments.color_view().cloned(),
        None,
        ft.attachments.msaa_view().cloned(),
    );
    (attachments, ft.color)
}

/// Render `instances` of `fixture`'s mesh and read the frame back.
pub(super) async fn render(ctx: &Ctx, fixture: &SceneFixture, instances: &[MeshInstance]) -> Frame {
    // The target follows the pipeline: a pass must declare a depth attachment
    // exactly when the pipelines it binds declare a depth state.
    let (context, target) =
        render_target_with_depth(ctx, fixture.multisample.count, fixture.has_depth);

    // Global group: camera, frame globals, the view and the mesh-metadata
    // array. The lit shader measures in world units, so the view is bound but
    // never read; it still has to be uploaded, since the layout declares it.
    let camera = camera(WIDTH as f32 / HEIGHT as f32);
    let globals = Globals::default();
    let view = View::new(WIDTH as f32, HEIGHT as f32);
    let camera_buffer = upload_buffer(
        ctx,
        "test::camera",
        camera.as_bytes(),
        wgpu::BufferUsages::UNIFORM,
    );
    let globals_buffer = upload_buffer(
        ctx,
        "test::globals",
        globals.as_bytes(),
        wgpu::BufferUsages::UNIFORM,
    );
    let view_buffer = upload_buffer(
        ctx,
        "test::view",
        view.as_bytes(),
        wgpu::BufferUsages::UNIFORM,
    );
    // The metadata bindings (and the mesh group that indexes them) exist only
    // while a channel is compressed.
    let metadata = fixture.pipeline.descriptor().needs_metadata();
    // Held in whichever resource the pipeline's layout declares — a storage
    // buffer, or a texel array on a device without storage buffers — so the
    // fixture follows the variant rather than assuming a buffer. The
    // descriptor's own answer decides, which is what keeps the resource and
    // the layout it is bound through in agreement.
    let mut metadata_array = Array::new(
        &ctx.device,
        Some("test::mesh_meta"),
        size_of::<MeshMetadata>() as u64,
        1,
        (!supports_storage_buffers(&ctx.device))
            .then(|| ctx.device.limits().max_texture_dimension_2d),
    );
    metadata_array.write(&ctx.queue, fixture.mesh.metadata.as_bytes());
    let mut global_entries = vec![
        bg_entry(CAMERA_BINDING, camera_buffer.as_entire_binding()),
        bg_entry(FRAME_BINDING, globals_buffer.as_entire_binding()),
        bg_entry(VIEW_BINDING, view_buffer.as_entire_binding()),
    ];
    let metadata_handle = metadata_array.handle();
    if metadata {
        global_entries.push(bg_entry(
            MESH_METADATA_BINDING,
            metadata_handle.binding_resource(),
        ));
    }
    let global_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test::globals"),
        layout: &fixture
            .pipeline
            .descriptor()
            .bind_group_layouts(&ctx.device)
            .global,
        entries: &global_entries,
    });

    let instance_data = upload_buffer(
        ctx,
        "test::instances",
        instances.as_bytes(),
        wgpu::BufferUsages::VERTEX,
    );
    let instance_count = instances.len() as u32;
    let range = match &fixture.mesh.indices {
        Some((_, count)) => DrawRange::Indexed {
            indices: 0..*count,
            base_vertex: 0,
            instances: 0..instance_count,
        },
        None => DrawRange::Vertices {
            vertices: 0..fixture.mesh.vertex_count,
            instances: 0..instance_count,
        },
    };

    let mut draw = DrawEntry::new(&fixture.pipeline.pipeline, range)
        .with_bind_group(GLOBAL_GROUP, &global_group);
    if let Some(bind_group) = &fixture.material {
        draw = draw.with_bind_group(MATERIAL_GROUP, bind_group);
    }
    // No mesh group: the variants these tests build read no morph
    // displacements, and every other per-mesh input rides the instance stream
    // or the global group. A morphed variant would bind its displacements at
    // [MESH_GROUP] here.
    // Per-instance data places the geometry; every built-in variant declares
    // and reads the instance stream, so the slot is always bound.
    draw = draw.with_vertex_buffer(INSTANCE_SLOT, &instance_data);
    if let Some(positions) = &fixture.mesh.positions {
        draw = draw.with_vertex_buffer(PRIMARY_SLOT, positions);
    }
    if let Some(secondary) = &fixture.mesh.secondary {
        draw = draw.with_vertex_buffer(SECONDARY_SLOT, secondary);
    }
    if let Some((buffer, _)) = &fixture.mesh.indices {
        draw = draw.with_index_buffer(buffer, wgpu::IndexFormat::Uint16);
    }
    let scene = Scene {
        draws: vec![draw],
        ..Scene::new()
    };

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
    {
        // A target without a depth attachment has no depth to clear; the load
        // ops are ignored for the attachment the pass does not declare.
        let mut pass = context.begin_pass(
            &mut encoder,
            "test pass",
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        scene.record(&mut pass, None);
    }
    ctx.queue.submit([encoder.finish()]);

    Frame {
        rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await,
        width: WIDTH,
        height: HEIGHT,
    }
}

/// The cube's default placement in every test: scaled, Y-rotated, raised.
pub(super) fn placed_cube(base_color: [f32; 4]) -> MeshInstance {
    placed(0.7, 0.6, glam::Vec3::new(0.0, 0.2, 0.0), base_color)
}

/// A variant built for the test's render target from `options` and the draw
/// facts the test supplies.
///
/// The surface is the caller's word now that the options carry no target
/// state: the fixture rewrites it per test, and a depth attachment at the
/// device's default format is what nearly every test renders into.
pub(super) fn variant(
    ctx: &Ctx,
    mut options: LitOptions,
    channels: LitVertexChannels,
    base_color_texture: bool,
    morph: bool,
) -> LitVariant {
    let surface = SurfaceKey {
        color_format: COLOR_FORMAT,
        depth_stencil_format: Some(default_depth_stencil_format(&ctx.device)),
        sample_count: 1,
    };
    // These tests draw without lights: their global group carries the camera,
    // the frame clock and the mesh metadata, and a lit variant would also
    // demand the light arrays and the cluster.
    options.unlit = true;
    LitVariant {
        options: Hashed::new(options),
        surface,
        channels,
        base_color_texture,
        morph,
        strip_index_format: None,
    }
}

/// The vertex-color variant the pixel tests use.
///
/// The standard policy with its channels narrowed to the vertex color alone —
/// no UV, so no base-color texture either.
pub(super) fn vertex_color_variant(ctx: &Ctx) -> LitVariant {
    variant(
        ctx,
        LitOptions::standard(),
        LitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: false,
            },
            secondary: SecondaryFlags::COLOR,
        },
        false,
        false,
    )
}
