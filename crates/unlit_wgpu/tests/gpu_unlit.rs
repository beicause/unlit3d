//! End-to-end GPU tests for the built-in unlit pipeline.
//!
//! Every test builds real resources through the library's public API — the
//! vertex compressors, the built-in [`SpecializedUnlitPipeline`] and the single-pass
//! [`Renderer`] — renders offscreen, and inspects the pixels that come back.
//! The snapshot tests additionally compare frames against stored references
//! with the SSIMULACRA2 perceptual metric.

// They exercise the built-in pipeline, so they are built with the feature that
// provides it.
#![cfg(feature = "unlit")]

mod common;

use common::*;
use unlit_wgpu::globals::{Globals, View};
use unlit_wgpu::mesh::{
    ChannelEncoding, MeshInstance, MeshMetadata, PositionStreamChannels, UvColorFlags,
    compress_indices, compress_positions, quantize_colors,
};
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_METADATA_BINDING, POSITION_SLOT,
    SpecializedUnlitPipeline, UV_COLOR_SLOT, UnlitOptions, UnlitVariant, UnlitVertexChannels,
    supports_storage_buffers,
};
use unlit_wgpu::render_attachments::{
    RenderAttachments, create_render_target, depth_clear, stencil_clear,
};
use unlit_wgpu::resources::{TextureExt, TextureView};
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use unlit_wgpu::specialize::{SpecializedPipeline, SurfaceKey, SurfaceTarget};
use unlit_wgpu::texel_array::Array;
use unlit_wgpu::util::Hashed;
use unlit_wgpu_test_util::{Tolerance, gpu_test_main, gpu_tests, snapshot};
use zerocopy::IntoBytes;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
/// Background the tests clear to, as normalized sRGB components.
const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The tolerance the two cube snapshots are compared within.
///
/// Both frames are cubes seen at an angle: their silhouette and the edges
/// between their faces are hard, and in `unlit_cubes_instanced` the cubes
/// overlap, so which of two cubes owns a pixel where they meet is the depth
/// test's to decide. Where two depths are equal, or where a multisampled edge
/// resolves differently, that is the implementation's choice rather than the
/// API's, so a second implementation moves a scatter of pixels along those
/// edges while drawing the same picture.
///
/// The perceptual score is weak on these frames, and alone would be the wrong
/// gate. A differing pixel sits on a hard edge, where the metric is most
/// sensitive, so the scatter costs far more than its area suggests — enough to
/// turn the metric's own ranking inside out: the WebGL2 frame of
/// `unlit_cubes_instanced`, which draws the cubes in the right places with the
/// right colors, scores 48.97, *below* the 57.38 that shifting that whole frame
/// one pixel scores. A gate the metric cannot order correctly must not be the
/// one deciding these frames, which is what the outlier allowance is for.
///
/// The score is still worth bounding, just loosely: 45 sits below the lowest
/// score a correct frame reached rather than just under it, so it catches a
/// frame that has collapsed — the picture missing, or every vertex in the wrong
/// place — without re-deciding the edge scatter the outlier allowance already
/// judges. 57.38 passing it is the accepted cost, and the outlier allowance
/// catches that regression instead.
///
/// Measured against the stored snapshots, as score / outliers:
///
/// | frame | `unlit_cubes_instanced` | `unlit_cube_textured` |
/// |-------|-------------------------|-----------------------|
/// | Vulkan (where they were captured) | 95.88 / 0.000% | 100.00 / 0.000% |
/// | native GL | 73.11 / 0.157% | 100.00 / 0.000% |
/// | Chromium on WebGL2 | 48.97 / 0.385% | 79.28 / 0.118% |
/// | regression: shifted one pixel | 57.38 / 2.979% | 3.20 / 5.015% |
/// | regression: red cube 0.05 nearer | — / 1.750% | — |
/// | regression: drawn 10/255 darker | 28.79 / 54.688% | 27.49 / 50.635% |
/// | regression: the frame blanked | -301.81 / 54.688% | -415.07 / 50.781% |
///
/// 1% sits in the widest gap the outlier measurements leave: every correct frame
/// stays under 0.4%, and the mildest regression measured is over four times past
/// it. The cost is that a change staying within `channel_delta` of nearly every
/// pixel is invisible here — moving one cube 0.02 units nearer reads 0.60%,
/// under this bound — so this tolerance is for a second implementation, not a
/// substitute for the test's own assertions. What covers the property the frames
/// are really about is the structural check beside each one: that the three
/// cubes land in the order their transforms put them, each in its own color.
const CUBE_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(45.0),
    max_outliers: Some(0.01),
    channel_delta: 8,
};

/// A uniformly scaled, Y-rotated instance at `translation`.
fn placed(
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
struct GpuMesh {
    /// Slot 0: `Snorm16x4` positions; `None` when the variant declares no
    /// position stream.
    positions: Option<wgpu::Buffer>,
    /// Slot 1: `Snorm16x2` UVs and/or `Unorm8x4` colors, interleaved by the
    /// variant's [`unlit_wgpu::mesh::MeshVertexStreamWriter`]; `None` when the
    /// variant declares no channel.
    uv_color: Option<wgpu::Buffer>,
    /// Index buffer and its index count, when the mesh is drawn indexed.
    indices: Option<(wgpu::Buffer, u32)>,
    /// Number of vertices.
    vertex_count: u32,
    /// Per-mesh decode parameters.
    metadata: MeshMetadata,
}

impl GpuMesh {
    /// Compress and upload the channels `variant` declares, narrowing
    /// `indices` to `u16`. Channels the variant does not declare are neither
    /// compressed nor uploaded, so a position-less variant has no position
    /// buffer at all. An empty `indices` slice uploads no index buffer.
    fn upload(
        ctx: &Ctx,
        label: &str,
        variant: &UnlitVariant,
        positions: &[[f32; 3]],
        uvs: &[[f32; 2]],
        colors: &[[u8; 4]],
        indices: &[u32],
    ) -> Self {
        let stream = variant.uv_color_stream();
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
        let uv_color_buffer = (!stream.is_empty()).then(|| {
            let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label}::uv_color")),
                size: stream.byte_len(positions.len()) as u64,
                usage: vertex_usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            {
                let mut view = buffer.slice(..).get_mapped_range_mut().expect("mapped");
                stream.write(uvs, colors, &mut metadata, view.slice(..).into_slice(..));
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
            uv_color: uv_color_buffer,
            indices,
            vertex_count: positions.len() as u32,
            metadata,
        }
    }
}

/// Upload `bytes` as a `usage`-flagged buffer, writing into the buffer's own
/// memory so no staging copy is involved.
fn upload_buffer(ctx: &Ctx, label: &str, bytes: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
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
fn camera(aspect: f32) -> View {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = glam::Vec3::new(0.0, 1.2, 3.2);
    let view =
        glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::new(0.0, 0.2, 0.0), glam::Vec3::Y);
    View::new(view, projection, eye)
}

/// A CPU-side mesh: positions, UVs, vertex colors and `u32` indices.
type MeshData = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A unit cube centred on the origin, with UVs and vertex colors that exercise
/// both decode paths (the unlit shader ignores normals).
fn cube() -> MeshData {
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
    let mut indices = Vec::new();

    for (normal, tangent) in faces {
        let base = positions.len() as u32;
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
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // The vertex stream stores colors as `Unorm8x4`; quantize them once
    // here so the fixture hands the writer the width it uploads at.
    let colors = quantize_colors(&colors).collect();
    (positions, uvs, colors, indices)
}

/// A procedurally generated base-color texture plus its sampler.
struct BaseColorTexture {
    view: TextureView,
    sampler: wgpu::Sampler,
}

/// An 8x8 checkerboard: sampling it makes the UV decode visible in the frame.
fn checkerboard_texture(ctx: &Ctx) -> BaseColorTexture {
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
struct SceneFixture {
    /// Whether the pipeline declares a depth-stencil state, which the render
    /// pass must then match: `wgpu` rejects a mismatch when binding it.
    has_depth: bool,
    pipeline: SpecializedUnlitPipeline,
    mesh: GpuMesh,
    material: Option<wgpu::BindGroup>,
    /// Multisample state the pipeline was compiled for; the render pass's
    /// attachments must match it.
    multisample: wgpu::MultisampleState,
}

/// Build the built-in pipeline for `variant`, upload the cube and — when the
/// variant samples a base-color texture — create its material bind group.
///
/// The variant's surface is rewritten for the test's render target: the color
/// format and sample count come from here, while the depth-stencil format
/// follows the variant's depth state, so a variant with one is built for a
/// target that has a depth attachment and a depth-less variant for one that
/// does not.
fn fixture(ctx: &Ctx, variant: &UnlitVariant, sample_count: u32) -> SceneFixture {
    let mut variant = variant.clone();
    variant.surface = SurfaceKey {
        color_format: COLOR_FORMAT,
        depth_stencil_format: variant
            .options
            .depth_stencil
            .as_ref()
            .map(|state| state.format),
        sample_count,
    };
    let pipeline = SpecializedPipeline::create(&ctx.device, variant.clone());

    let (positions, uvs, colors, indices) = cube();
    let mesh = GpuMesh::upload(
        ctx,
        "test::cube",
        &variant,
        &positions,
        &uvs,
        &colors,
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
        has_depth: variant.options.depth_stencil.is_some(),
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
fn render_target(ctx: &Ctx, sample_count: u32) -> (RenderAttachments, wgpu::Texture) {
    render_target_with_depth(ctx, sample_count, true)
}

/// Like [`render_target`], but may omit the depth-stencil attachment — which a
/// pipeline must then also omit.
fn render_target_with_depth(
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
fn render(ctx: &Ctx, fixture: &SceneFixture, instances: &[MeshInstance]) -> Frame {
    // The target follows the pipeline: a pass must declare a depth attachment
    // exactly when the pipelines it binds declare a depth state.
    let (context, target) =
        render_target_with_depth(ctx, fixture.multisample.count, fixture.has_depth);

    // Global group: camera, frame globals and the mesh-metadata array.
    let view = camera(WIDTH as f32 / HEIGHT as f32);
    let globals = Globals::default();
    let camera_buffer = upload_buffer(
        ctx,
        "test::camera",
        view.as_bytes(),
        wgpu::BufferUsages::UNIFORM,
    );
    let globals_buffer = upload_buffer(
        ctx,
        "test::globals",
        globals.as_bytes(),
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
        draw = draw.with_vertex_buffer(POSITION_SLOT, positions);
    }
    if let Some(uv_color) = &fixture.mesh.uv_color {
        draw = draw.with_vertex_buffer(UV_COLOR_SLOT, uv_color);
    }
    if let Some((buffer, _)) = &fixture.mesh.indices {
        draw = draw.with_index_buffer(buffer, wgpu::IndexFormat::Uint16);
    }
    let scene = Scene::new().with_draw(draw);

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
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        scene.record(&mut pass);
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    Frame {
        rgba: read_texture_bytes(ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    }
}

/// The cube's default placement in every test: scaled, Y-rotated, raised.
fn placed_cube(base_color: [f32; 4]) -> MeshInstance {
    placed(0.7, 0.6, glam::Vec3::new(0.0, 0.2, 0.0), base_color)
}

/// A variant built for the test's render target from `options` and the draw
/// facts the test supplies.
fn variant(
    options: UnlitOptions,
    channels: UnlitVertexChannels,
    base_color_texture: bool,
    morph: bool,
) -> UnlitVariant {
    let surface = SurfaceKey {
        color_format: COLOR_FORMAT,
        depth_stencil_format: options.depth_stencil.as_ref().map(|state| state.format),
        sample_count: options.multisample.count,
    };
    UnlitVariant {
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
fn vertex_color_variant(device: &wgpu::Device) -> UnlitVariant {
    variant(
        UnlitOptions::standard(device),
        UnlitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: false,
            },
            uv_color: UvColorFlags::COLOR,
        },
        false,
        false,
    )
}

async fn renders_a_cube_over_the_clear_color() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 0.85, 0.4, 1.0])]);

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);

    // The clear color survives in a corner: nothing was drawn there. The
    // target stores sRGB-encoded values, so decode back to linear before
    // comparing against the linear clear color.
    let corner = frame.pixel_u8(0, 0);
    for (channel, want) in corner[..3].iter().zip(CLEAR) {
        let got = srgb_to_linear_u8(*channel) as f64;
        assert!(
            (got - want).abs() < 0.01,
            "corner {corner:?} decodes to {got}, expected the clear color {want}"
        );
    }

    // The cube covers a meaningful part of the frame.
    let covered = count_pixels_off_background(&frame, CLEAR, 12);
    let total = (WIDTH * HEIGHT) as usize;
    assert!(
        covered > total / 8,
        "the cube should cover a meaningful area, got {covered}/{total} pixels"
    );
}

async fn base_color_reaches_the_frame() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), 4);

    let brightest = |base_color: [f32; 4]| {
        let frame = render(&ctx, &fixture, &[placed_cube(base_color)]);
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0] as u16 + p[1] as u16 + p[2] as u16)
            .max()
            .expect("a pixel")
    };

    let bright = brightest([1.0, 1.0, 1.0, 1.0]);
    let dim = brightest([0.1, 0.1, 0.1, 1.0]);
    assert!(
        bright > dim + 60,
        "a white base color should be much brighter than a dark one: {bright} vs {dim}"
    );
}

async fn depth_ordering_hides_the_far_instance() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), 4);

    // A far red cube and a near green one, drawn far-first so a missing depth
    // test would let the far cube show through.
    let far = placed(
        1.0,
        0.0,
        glam::Vec3::new(0.0, 0.2, -0.6),
        [1.0, 0.0, 0.0, 1.0],
    );
    let near = placed(
        1.0,
        0.0,
        glam::Vec3::new(0.0, 0.2, 0.6),
        [0.0, 1.0, 0.0, 1.0],
    );
    let frame = render(&ctx, &fixture, &[far, near]);

    let centre = frame.pixel_u8(WIDTH / 2, HEIGHT / 2);
    assert!(
        centre[1] > centre[0],
        "the near green cube should win the depth test, got {centre:?}"
    );
}

async fn msaa_produces_more_partial_coverage_than_no_msaa() {
    let ctx = Ctx::headless().await;
    let instance = placed(0.9, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);

    // Pixels that are neither fully clear nor fully covered: the
    // antialiased silhouette, which a single-sample render cannot produce.
    let partial = |sample_count: u32| {
        let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), sample_count);
        let frame = render(&ctx, &fixture, std::slice::from_ref(&instance));
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| {
                let luma = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                (30.0..200.0).contains(&luma)
            })
            .count()
    };

    let without = partial(1);
    let with = partial(4);
    assert!(
        with > without,
        "MSAA should produce more partial-coverage pixels: {with} vs {without}"
    );
}

async fn unlit_cube_matches_snapshot() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 0.85, 0.4, 1.0])]);
    assert_image_snapshot(
        snapshot!("unlit_cube.webp"),
        &frame,
        frame.width,
        frame.height,
    );
}

/// One draw call over several instances: the per-instance transform and base
/// color must both reach the shader, so every cube lands in its own place with
/// its own color.
async fn instanced_cubes_match_snapshot() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx.device), 4);

    // A row of cubes at different depths, each with its own base color, all
    // drawn by one instanced draw call.
    let instances: Vec<MeshInstance> = [
        (-0.9, -0.4, [1.0, 0.3, 0.3, 1.0]),
        (0.0, 0.0, [0.3, 1.0, 0.4, 1.0]),
        (0.9, 0.4, [0.4, 0.5, 1.0, 1.0]),
    ]
    .into_iter()
    .map(|(x, z, color)| placed(0.55, 0.6, glam::Vec3::new(x, 0.0, z), color))
    .collect();

    let frame = render(&ctx, &fixture, &instances);

    // The per-instance transform must separate the cubes horizontally, in the
    // order the instances were declared, and each must be tinted by its own
    // base color. Checking this before the snapshot keeps the stored reference
    // from freezing a wrong frame.
    let dominant_x = |channel: usize| {
        let mut weighted = 0.0;
        let mut weight = 0.0;
        for y in 0..frame.height {
            for x in 0..frame.width {
                let p = frame.pixel_u8(x, y);
                let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                let (max, other) = match channel {
                    0 => (r, g.max(b)),
                    1 => (g, r.max(b)),
                    _ => (b, r.max(g)),
                };
                // Weight by how far this pixel leans toward the channel, so
                // the background and the other cubes contribute little.
                let w = (max - other).max(0.0);
                if w > 12.0 {
                    weighted += w * x as f32;
                    weight += w;
                }
            }
        }
        (weight > 0.0).then(|| weighted / weight)
    };

    let red = dominant_x(0).expect("the red cube is drawn");
    let green = dominant_x(1).expect("the green cube is drawn");
    let blue = dominant_x(2).expect("the blue cube is drawn");
    assert!(
        red < green && green < blue,
        "each instance must land where its transform puts it: \
         red x={red:.1}, green x={green:.1}, blue x={blue:.1}"
    );

    assert_image_snapshot_with_tolerance(
        snapshot!("unlit_cubes_instanced.webp"),
        &frame,
        frame.width,
        frame.height,
        CUBE_TOLERANCE,
    );
}

/// Without a position stream the geometry is a single point at each instance
/// origin, so a position-less draw needs no position buffer and still lands
/// where the per-instance transform puts it.
async fn position_less_variant_draws_points_at_instance_origins() {
    let ctx = Ctx::headless().await;
    let variant = variant(
        UnlitOptions::standard(&ctx.device),
        UnlitVertexChannels {
            position: PositionStreamChannels {
                position: None,
                joints: false,
            },
            uv_color: UvColorFlags::empty(),
        },
        false,
        false,
    );
    let fixture = fixture(&ctx, &variant, 1);
    assert!(
        fixture.mesh.positions.is_none(),
        "a position-less variant must not upload a position buffer"
    );

    // Two instances at different places. A position-less variant draws the
    // unindexed point range from the fixture's vertex count.
    let origin = placed(1.0, 0.0, glam::Vec3::ZERO, [1.0, 0.0, 0.0, 1.0]);
    let right = placed(
        1.0,
        0.0,
        glam::Vec3::new(1.0, 0.0, 0.0),
        [0.0, 1.0, 0.0, 1.0],
    );
    let frame = render(&ctx, &fixture, &[origin, right]);

    // Count lit pixels on each side of the centre column: the points must
    // land apart, which only holds if the position came from the instance
    // transform rather than a vertex buffer.
    let mut left = 0;
    let mut right_count = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let p = frame.pixel_u8(x, y);
            let lit = p[..3]
                .iter()
                .zip(CLEAR)
                .any(|(&c, bg)| (c as f64 / 255.0 - bg).abs() * 255.0 > 12.0);
            if lit {
                if x < WIDTH / 2 {
                    left += 1;
                } else {
                    right_count += 1;
                }
            }
        }
    }
    assert!(
        left > 0 && right_count > 0,
        "the two instances should land on opposite sides: {left} left, {right_count} right"
    );
}

async fn textured_cube_matches_snapshot() {
    let ctx = Ctx::headless().await;
    let variant = variant(
        UnlitOptions::standard(&ctx.device),
        UnlitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: false,
            },
            uv_color: UvColorFlags::UV | UvColorFlags::COLOR,
        },
        true,
        false,
    );
    let fixture = fixture(&ctx, &variant, 4);
    let instance = placed(0.8, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);
    let frame = render(&ctx, &fixture, &[instance]);
    assert_image_snapshot_with_tolerance(
        snapshot!("unlit_cube_textured.webp"),
        &frame,
        frame.width,
        frame.height,
        CUBE_TOLERANCE,
    );
}

/// A loaded color attachment keeps its previous contents: the second pass
/// draws nothing, so the frame the first pass left is still there.
///
/// This pins the `LoadOp::Load` path of `begin_pass`, which the other tests
/// never exercise — they all clear.
async fn a_loaded_color_attachment_keeps_its_contents() {
    let ctx = Ctx::headless().await;
    // No MSAA: the pass draws straight into the color texture, so a stored
    // frame survives into a second pass that loads.
    let (context, target) = render_target(&ctx, 1);

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
    // Pass one: clear to the test clear color. Draws nothing.
    {
        let mut pass = context.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass);
    }
    // Pass two: load the color (the depth attachment is transient, so it
    // still clears) and draw nothing. If the color load were a clear to the
    // wgpu default (transparent black), the frame would come back empty.
    {
        let mut pass = context.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Load,
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass);
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
        width: WIDTH,
        height: HEIGHT,
    };
    // The clear color of pass one survived pass two. The readback is the
    // texture's own encoding (sRGB here), not the linear clear value.
    let expected = rgb(CLEAR[0], CLEAR[1], CLEAR[2]);
    let first = frame.pixel_u8(0, 0);
    let encode = |linear: f64| {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0) as u8
    };
    // Off-by-one rounding tolerance on each channel.
    let within = |a: u8, b: u8| a.abs_diff(b) <= 1;
    assert!(
        within(first[0], encode(expected.r))
            && within(first[1], encode(expected.g))
            && within(first[2], encode(expected.b))
            && first[3] == 255,
        "a loaded attachment must keep its contents"
    );
}

/// A uniform buffer the graph tests can stand in for any resource kind.
fn uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    })
}

async fn resource_graph_rebuilds_a_dependent_after_a_resource_change() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use unlit_wgpu::resources::{Rebuild, Resource, ResourceGraph};

    let ctx = Ctx::headless().await;
    let mut graph = ResourceGraph::new();

    let base = graph.insert(uniform(&ctx.device, "test::base"), None);

    // A dependent whose recipe records that it ran: rebuilding is driven
    // purely by the graph.
    let rebuilt = Rc::new(RefCell::new(false));
    let seen = Rc::clone(&rebuilt);
    let device = ctx.device.clone();
    let dependent = graph.insert(
        uniform(&ctx.device, "test::dependent"),
        Some(Rebuild::new({
            let base = base.clone();
            move |graph| {
                assert!(
                    graph.get(&base).is_some(),
                    "the dependency is rebuilt before its dependent"
                );
                *seen.borrow_mut() = true;
                Resource::Buffer(uniform(&device, "test::rebuilt"))
            }
        })),
    );
    graph.add_dependency(&dependent, &base);

    // A no-op pass leaves a clean graph alone, recipes and all.
    graph.maintain();
    assert!(!*rebuilt.borrow());

    // Swapping the base must dirty the dependent, which the next pass then
    // rebuilds in dependency order.
    graph.replace(&base, uniform(&ctx.device, "test::base2"));
    graph.maintain();

    assert!(*rebuilt.borrow());
}

/// A target with no depth attachment must work.
///
/// The pass declares no depth attachment, so every pipeline bound into it must
/// declare no depth state either — `wgpu` rejects the mismatch when the
/// pipeline is bound. A pipeline that kept the base options' depth format
/// therefore could not be drawn into a color-only target at all, which is what
/// a UI-only or overlay-only pass needs.
async fn a_color_only_target_draws_a_cube() {
    let ctx = Ctx::headless().await;
    // Specialize the policy for the depth-less target the same way a caller
    // would, then build the pipeline from the result.
    let mut variant = vertex_color_variant(&ctx.device);
    variant.set_surface(SurfaceKey {
        color_format: COLOR_FORMAT,
        depth_stencil_format: None,
        sample_count: 4,
    });
    assert!(
        variant.options.depth_stencil.is_none(),
        "a depth-less target yields a depth-less pipeline"
    );
    assert!(
        !fixture(&ctx, &variant, 4).has_depth,
        "so the fixture's pass declares no depth attachment"
    );

    let fixture = fixture(&ctx, &variant, 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0; 4])]);

    // The cube reached the frame: some pixel is brighter than the clear color.
    let clear_sum = ((CLEAR[0] + CLEAR[1] + CLEAR[2]) * 255.0) as u16;
    let covered = frame
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] as u16 + p[1] as u16 + p[2] as u16 > clear_sum + 30)
        .count();
    assert!(
        covered > 0,
        "the cube should reach a target with no depth attachment"
    );
}

/// A readback of a texture whose row is not a multiple of the copy alignment
/// comes back tight and correct.
///
/// A texture-to-buffer copy aligns every row to `COPY_BYTES_PER_ROW_ALIGNMENT`,
/// which is 256 bytes and much coarser than the buffer alignment. A width that
/// is not a multiple of 64 RGBA pixels — 256 bytes — therefore needs padding
/// *and* the padding stripped. Every other readback in the suite is 256 wide,
/// where the two alignments coincide, so an unpadded copy validates there by
/// luck and fails on any other width.
async fn a_readback_handles_a_row_that_is_not_copy_aligned() {
    // 60 RGBA pixels is 240 bytes: a multiple of the buffer alignment (4) but
    // not of the copy row alignment (256), which is exactly the case the
    // harness used to reject.
    const WIDTH: u32 = 60;
    const HEIGHT: u32 = 8;
    let ctx = Ctx::headless().await;

    let ft = create_render_target(&ctx.device, COLOR_FORMAT, WIDTH, HEIGHT, 1);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_unlit::unaligned_clear"),
        });
    {
        let mut pass = ft.attachments.begin_pass(
            &mut encoder,
            wgpu::LoadOp::Clear(rgb(CLEAR[0], CLEAR[1], CLEAR[2])),
            depth_clear(),
            stencil_clear(),
        );
        Scene::new().record(&mut pass);
    }
    ctx.queue.submit([encoder.finish()]);

    let frame = Frame {
        rgba: read_texture_bytes(&ctx, &ft.color, WIDTH, HEIGHT, texel_bytes(&ft.color)),
        width: WIDTH,
        height: HEIGHT,
    };

    // Tight bytes, not padded ones: the buffer rows are stripped back off.
    assert_eq!(
        frame.rgba.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "the readback must return exactly the frame's pixels"
    );
    // Every row is the clear colour, so a row that lost its stride would show
    // up as a mismatch somewhere in the image. The readback is the texture's
    // own sRGB encoding, so the clear value is encoded the same way before
    // comparing.
    let encode = |linear: f64| {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let expect = [encode(CLEAR[0]), encode(CLEAR[1]), encode(CLEAR[2])];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let px = frame.pixel_u8(x, y);
            assert!(
                px[..3]
                    .iter()
                    .zip(expect)
                    .all(|(&got, want)| got.abs_diff(want) <= 2),
                "pixel ({x}, {y}) should be the clear colour, got {px:?}"
            );
        }
    }
}

// The registry both runners drive: `cargo nextest` natively, and a
// browser through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    renders_a_cube_over_the_clear_color,
    base_color_reaches_the_frame,
    depth_ordering_hides_the_far_instance,
    msaa_produces_more_partial_coverage_than_no_msaa,
    unlit_cube_matches_snapshot,
    instanced_cubes_match_snapshot,
    position_less_variant_draws_points_at_instance_origins,
    textured_cube_matches_snapshot,
    a_loaded_color_attachment_keeps_its_contents,
    resource_graph_rebuilds_a_dependent_after_a_resource_change,
    a_color_only_target_draws_a_cube,
    a_readback_handles_a_row_that_is_not_copy_aligned,
}

gpu_test_main!(all_tests());
