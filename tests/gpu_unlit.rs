//! End-to-end GPU tests for the built-in unlit pipeline.
//!
//! Every test builds real resources through the library's public API — the
//! vertex compressors, the built-in [`UnlitPipeline`] and the single-pass
//! [`Renderer`] — renders offscreen, and inspects the pixels that come back.
//! The snapshot tests additionally compare frames against stored references
//! with the SSIMULACRA2 perceptual metric.

mod common;

use common::*;
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::mesh::{
    MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use wgpu_unlit_render::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP, MESH_INFO_BINDING,
    MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT, UnlitFlags, UnlitOptions, UnlitPipeline,
};
use wgpu_unlit_render::render_context::{RenderContext, RendererOptions};
use wgpu_unlit_render::scene::{DrawRange, MaterialGroup, MeshDraw, PipelineGroup, Scene};
use zerocopy::IntoBytes;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
/// Background the tests clear to, as normalized sRGB components.
const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

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
    /// variant's [`MeshUvColorStream`]; `None` when the variant declares no
    /// channel.
    uv_color: Option<wgpu::Buffer>,
    /// Index buffer and its index count, when the mesh is drawn indexed.
    indices: Option<(wgpu::Buffer, u32)>,
    /// Number of vertices.
    vertex_count: u32,
    /// Per-mesh decode parameters.
    metadata: MeshMetadata,
}

impl GpuMesh {
    /// Compress and upload the channels `options` enables, narrowing
    /// `indices` to `u16`. Channels the variant does not declare are neither
    /// compressed nor uploaded, so a position-less variant has no position
    /// buffer at all. An empty `indices` slice uploads no index buffer.
    fn upload(
        ctx: &Ctx,
        label: &str,
        options: &UnlitOptions,
        positions: &[[f32; 3]],
        uvs: &[[f32; 2]],
        colors: &[[f32; 4]],
        indices: &[u32],
    ) -> Self {
        let stream = options.uv_color_stream();
        let mut metadata = MeshMetadata::default();
        let compressed_positions = options.flags.contains(UnlitFlags::VERTEX_POSITION)
            && !options.flags.contains(UnlitFlags::UNCOMPRESSED_POSITION);
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
        let positions_buffer = options
            .flags
            .contains(UnlitFlags::VERTEX_POSITION)
            .then(|| {
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
        let indices = (options.flags.contains(UnlitFlags::VERTEX_POSITION) && !indices.is_empty())
            .then(|| {
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
    View::new(projection * view, eye)
}

/// A CPU-side mesh: positions, UVs, vertex colors and `u32` indices.
type MeshData = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[f32; 4]>, Vec<u32>);

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

    (positions, uvs, colors, indices)
}

/// A procedurally generated base-color texture plus its sampler.
struct BaseColorTexture {
    view: wgpu::TextureView,
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
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
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
    pipeline: UnlitPipeline,
    mesh: GpuMesh,
    material: Option<wgpu::BindGroup>,
    /// Sample count the pipeline was compiled for; the render pass must match.
    sample_count: u32,
}

/// Build the built-in pipeline for `options`, upload the cube and — when the
/// variant samples a base-color texture — create its material bind group.
fn fixture(ctx: &Ctx, options: &UnlitOptions, sample_count: u32) -> SceneFixture {
    let pipeline = UnlitPipeline::new(
        &ctx.device,
        options,
        COLOR_FORMAT,
        Some(DEPTH_FORMAT),
        sample_count,
    );

    let (positions, uvs, colors, indices) = cube();
    let mesh = GpuMesh::upload(
        ctx,
        "test::cube",
        options,
        &positions,
        &uvs,
        &colors,
        &indices,
    );

    let material = options
        .flags
        .contains(UnlitFlags::BASE_COLOR_TEXTURE)
        .then(|| {
            let texture = checkerboard_texture(ctx);
            ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test::material"),
                layout: pipeline
                    .material_layout
                    .as_ref()
                    .expect("a textured variant has a material layout"),
                entries: &[
                    bg_entry(
                        BASE_COLOR_TEXTURE_BINDING,
                        wgpu::BindingResource::TextureView(&texture.view),
                    ),
                    bg_entry(
                        BASE_COLOR_SAMPLER_BINDING,
                        wgpu::BindingResource::Sampler(&texture.sampler),
                    ),
                ],
            })
        });

    SceneFixture {
        pipeline,
        mesh,
        material,
        sample_count,
    }
}

/// Render `instances` of `fixture`'s mesh and read the frame back.
fn render(ctx: &Ctx, fixture: &SceneFixture, instances: &[MeshInstance]) -> Frame {
    let target = ColorTarget::new(&ctx.device, "test::target", WIDTH, HEIGHT);
    let context = RenderContext::new(
        &ctx.device,
        Some(target.view.clone()),
        RendererOptions {
            color: Some(COLOR_FORMAT),
            depth: Some(DEPTH_FORMAT),
            width: WIDTH,
            height: HEIGHT,
            sample_count: fixture.sample_count,
        },
    );
    let renderer = context.renderer();

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
    let metadata = fixture.pipeline.options.needs_metadata();
    let metadata_buffer = upload_buffer(
        ctx,
        "test::mesh_meta",
        fixture.mesh.metadata.as_bytes(),
        wgpu::BufferUsages::STORAGE,
    );
    let mut global_entries = vec![
        bg_entry(CAMERA_BINDING, camera_buffer.as_entire_binding()),
        bg_entry(FRAME_BINDING, globals_buffer.as_entire_binding()),
    ];
    if metadata {
        global_entries.push(bg_entry(
            MESH_METADATA_BINDING,
            metadata_buffer.as_entire_binding(),
        ));
    }
    let global_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test::globals"),
        layout: &fixture.pipeline.global_layout,
        entries: &global_entries,
    });

    // Mesh group: which metadata entry decodes this draw.
    let info_buffer = upload_buffer(
        ctx,
        "test::mesh_info",
        MeshInfo::new(0).as_bytes(),
        wgpu::BufferUsages::UNIFORM,
    );
    let mesh_group = metadata.then(|| {
        ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test::mesh"),
            layout: fixture
                .pipeline
                .mesh_layout
                .as_ref()
                .expect("a variant that needs metadata has a mesh layout"),
            entries: &[bg_entry(MESH_INFO_BINDING, info_buffer.as_entire_binding())],
        })
    });

    let instance_data = upload_buffer(
        ctx,
        "test::instances",
        instances.as_bytes(),
        wgpu::BufferUsages::VERTEX,
    );
    let instance_count = instances.len() as u32;
    let instanced = fixture
        .pipeline
        .options
        .flags
        .contains(UnlitFlags::VERTEX_INSTANCE);
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

    let mut mesh_draw = MeshDraw::new(range);
    if let Some(mesh_group) = &mesh_group {
        mesh_draw = mesh_draw.with_bind_group(MESH_GROUP, mesh_group);
    }
    // Per-instance data places the geometry, so the slot is bound only when
    // the variant reads it. A variant without it draws one instance.
    if instanced {
        mesh_draw = mesh_draw.with_vertex_buffer(INSTANCE_SLOT, instance_data.slice(..));
    }
    if let Some(positions) = &fixture.mesh.positions {
        mesh_draw = mesh_draw.with_vertex_buffer(POSITION_SLOT, positions.slice(..));
    }
    if let Some(uv_color) = &fixture.mesh.uv_color {
        mesh_draw = mesh_draw.with_vertex_buffer(UV_COLOR_SLOT, uv_color.slice(..));
    }
    if let Some((buffer, _)) = &fixture.mesh.indices {
        mesh_draw = mesh_draw.with_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint16);
    }

    let mut material = MaterialGroup::new();
    if let Some(bind_group) = &fixture.material {
        material = material.with_bind_group(MATERIAL_GROUP, bind_group);
    }
    let scene = Scene::new().with_pipeline(
        PipelineGroup::new(&fixture.pipeline.pipeline)
            .with_bind_group(GLOBAL_GROUP, &global_group)
            .with_material(material.with_mesh(mesh_draw)),
    );

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test::encoder"),
        });
    renderer.render(&mut encoder, rgb(CLEAR[0], CLEAR[1], CLEAR[2]), &scene);
    ctx.queue.submit([encoder.finish()]);
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");

    Frame {
        rgba: read_texture_bytes(
            ctx,
            &target.texture,
            WIDTH,
            HEIGHT,
            texel_bytes(&target.texture),
        ),
        width: WIDTH,
        height: HEIGHT,
    }
}

/// The cube's default placement in every test: scaled, Y-rotated, raised.
fn placed_cube(base_color: [f32; 4]) -> MeshInstance {
    placed(0.7, 0.6, glam::Vec3::new(0.0, 0.2, 0.0), base_color)
}

/// The vertex-color variant the pixel tests use.
fn vertex_color_options() -> UnlitOptions {
    UnlitOptions {
        flags: UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_COLOR | UnlitFlags::VERTEX_INSTANCE,
        ..UnlitOptions::standard()
    }
}

#[test]
fn renders_a_cube_over_the_clear_color() {
    let ctx = Ctx::headless();
    let fixture = fixture(&ctx, &vertex_color_options(), 4);
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

#[test]
fn base_color_reaches_the_frame() {
    let ctx = Ctx::headless();
    let fixture = fixture(&ctx, &vertex_color_options(), 4);

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

#[test]
fn depth_ordering_hides_the_far_instance() {
    let ctx = Ctx::headless();
    let fixture = fixture(&ctx, &vertex_color_options(), 4);

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

#[test]
fn msaa_produces_more_partial_coverage_than_no_msaa() {
    let ctx = Ctx::headless();
    let instance = placed(0.9, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);

    // Pixels that are neither fully clear nor fully covered: the
    // antialiased silhouette, which a single-sample render cannot produce.
    let partial = |sample_count: u32| {
        let fixture = fixture(&ctx, &vertex_color_options(), sample_count);
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

#[test]
fn unlit_cube_matches_snapshot() {
    let ctx = Ctx::headless();
    let fixture = fixture(&ctx, &vertex_color_options(), 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 0.85, 0.4, 1.0])]);
    assert_image_snapshot("unlit_cube.webp", &frame, frame.width, frame.height);
}

/// One draw call over several instances: the per-instance transform and base
/// color must both reach the shader, so every cube lands in its own place with
/// its own color.
#[test]
fn instanced_cubes_match_snapshot() {
    let ctx = Ctx::headless();
    let fixture = fixture(&ctx, &vertex_color_options(), 4);

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

    assert_image_snapshot(
        "unlit_cubes_instanced.webp",
        &frame,
        frame.width,
        frame.height,
    );
}

/// Without a position stream the geometry is a single point at each instance
/// origin, so a position-less draw needs no position buffer and still lands
/// where the per-instance transform puts it.
#[test]
fn position_less_variant_draws_points_at_instance_origins() {
    let ctx = Ctx::headless();
    let options = UnlitOptions {
        flags: UnlitFlags::VERTEX_INSTANCE,
        ..UnlitOptions::standard()
    };
    let fixture = fixture(&ctx, &options, 1);
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

#[test]
fn textured_cube_matches_snapshot() {
    let ctx = Ctx::headless();
    let options = UnlitOptions::standard();
    let fixture = fixture(&ctx, &options, 4);
    let instance = placed(0.8, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);
    let frame = render(&ctx, &fixture, &[instance]);
    assert_image_snapshot(
        "unlit_cube_textured.webp",
        &frame,
        frame.width,
        frame.height,
    );
}

/// Two minimal WGSL modules: the resource graph tracks identity, so the
/// shader bodies only need to differ.
const TRIVIAL_WGSL: &str =
    "@vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }";
const TRIVIAL_WGSL_2: &str =
    "@vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(1.0); }";

#[test]
fn resource_graph_rebuilds_a_pipeline_after_a_target_change() {
    use wgpu_unlit_render::resources::{Resource, ResourceGraph};

    let ctx = Ctx::headless();
    let mut graph = ResourceGraph::new();

    let shader = graph
        .insert(
            ctx.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("test::shader"),
                    // A trivial module: the graph tracks the resource, not
                    // what the shader does.
                    source: wgpu::ShaderSource::Wgsl(TRIVIAL_WGSL.into()),
                }),
            &[],
        )
        .expect("insert shader");

    // A stand-in dependent: rebuilding is driven purely by the graph.
    let dependent = graph
        .insert(
            ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("test::dependent"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            }),
            &[shader],
        )
        .expect("insert dependent");

    assert!(!graph.is_dirty(dependent));

    // Swapping the shader must dirty the dependent, which the rebuild pass
    // then refreshes in dependency order.
    graph.replace(
        shader,
        ctx.device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("test::shader2"),
                // A genuinely different module, so the dependent rebuilds
                // over something new rather than an identical copy.
                source: wgpu::ShaderSource::Wgsl(TRIVIAL_WGSL_2.into()),
            }),
    );
    assert!(graph.is_dirty(shader));
    assert!(graph.is_dirty(dependent));

    let mut rebuilt = Vec::new();
    graph.rebuild_dirty(|id, _current, _dependencies| {
        rebuilt.push(id);
        Some(Resource::Buffer(ctx.device.create_buffer(
            &wgpu::BufferDescriptor {
                label: Some("test::rebuilt"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            },
        )))
    });
    assert_eq!(rebuilt, vec![shader, dependent]);
    assert!(!graph.any_dirty());
}
