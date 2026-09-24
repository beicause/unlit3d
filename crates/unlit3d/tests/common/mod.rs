//! Shared harness for unlit3d GPU integration tests.
//!
//! Re-exports general GPU test helpers and adds crate-specific
//! helpers tailored to the `unlit3d` ECS-based rendering API.

pub use wgpu_unlit_test_util::{
    Ctx, Frame, assert_image_snapshot, count_pixels_off_background, read_texture_bytes, texel_bytes,
};

use unlit_ecs::LocalWorld;
use unlit3d::prelude::*;

/// Test constants matching what `wgpu_unlit_render`'s own tests use.
pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 192;
pub const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Create a `(Renderer, LocalWorld, UnlitPipelineKey)` triplet ready for
/// testing.
///
/// The returned [UnlitPipelineKey] is the built-in unlit family's key; every
/// renderable entity must carry an [UnlitPipeline] built from it.
pub fn test_world(ctx: &Ctx) -> (Renderer, LocalWorld, UnlitPipelineKey) {
    let (renderer, key) = create_renderer(ctx);
    let world = LocalWorld::new();
    (renderer, world, key)
}

/// Build a `Renderer` on `ctx`'s device with the built-in unlit family
/// registered, plus a standard key to draw with.
pub fn create_renderer(ctx: &Ctx) -> (Renderer, UnlitPipelineKey) {
    let mut renderer = Renderer::new(ctx.device.clone(), ctx.queue.clone());
    renderer.register_unlit_family();
    let key = UnlitPipelineKey::new(unlit_options(&ctx.device));
    (renderer, key)
}

/// Unlit options for the ECS tests: vertex colour + instance, no texture,
/// no MSAA, with reverse-z depth.
fn unlit_options(device: &wgpu::Device) -> wgpu_unlit_render::pipeline::UnlitOptions {
    use wgpu_unlit_render::pipeline::UnlitFlags;
    use wgpu_unlit_render::render_attachments::default_depth_stencil_format;
    wgpu_unlit_render::pipeline::UnlitOptions {
        flags: UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_COLOR | UnlitFlags::VERTEX_INSTANCE,
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: wgpu::DepthStencilState {
            format: default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        },
        color_target: wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        },
        multisample: wgpu::MultisampleState {
            count: 1,
            ..Default::default()
        },
    }
}

/// The raw channels of one mesh: `(positions, uvs, colors, indices)`.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A unit cube centred at the origin, returned as
/// `(positions, uvs, colors, indices)`.
pub fn cube() -> RawMesh {
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
            let color = (position + 1.0) * 0.5;
            colors.push([color.x, color.y, color.z, 1.0]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // The stream stores colors as `Unorm8x4`, so quantize once here rather
    // than carrying a float copy through the fixture.
    let colors = wgpu_unlit_render::mesh::quantize_colors(&colors).collect();
    (positions, uvs, colors, indices)
}

/// Camera looking at the origin from (0, 1.2, 3.2).
///
/// Uses reverse-z infinite perspective matching the built-in pipeline's
/// `CompareFunction::Greater` and `depth_clear = 0.0`.
pub fn camera_view(aspect: f32) -> Camera {
    camera_looking_at(
        glam::Vec3::new(0.0, 1.2, 3.2),
        glam::Vec3::new(0.0, 0.2, 0.0),
        aspect,
    )
}

/// A camera at `eye` looking at `target`, with the same reverse-z infinite
/// perspective as [`camera_view`].
pub fn camera_looking_at(eye: glam::Vec3, target: glam::Vec3, aspect: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let view = glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y);
    Camera {
        clip_from_world: projection * view,
        position: eye,
    }
}

/// A dense grid of cubes as one mesh, returned as
/// `(positions, uvs, colors, indices)`.
///
/// One cube is a couple of hundred bytes, too small to push a pool past its
/// starting capacity; a grid of many of them is what makes a pool grow inside
/// a test.
pub fn grid_cube(steps: u32) -> RawMesh {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();

    for i in 0..steps {
        for j in 0..steps {
            for k in 0..steps {
                let offset = glam::Vec3::new(i as f32 * 3.0, j as f32 * 3.0, k as f32 * 3.0);
                let (mut cube_positions, mut cube_uvs, mut cube_colors, cube_indices) = cube();
                for position in &mut cube_positions {
                    *position = [
                        position[0] * 0.5 + offset.x,
                        position[1] * 0.5 + offset.y,
                        position[2] * 0.5 + offset.z,
                    ];
                }
                let base = positions.len() as u32;
                positions.append(&mut cube_positions);
                uvs.append(&mut cube_uvs);
                colors.append(&mut cube_colors);
                indices.extend(cube_indices.into_iter().map(|index| index + base));
            }
        }
    }

    (positions, uvs, colors, indices)
}

/// Allocate the cube mesh through the renderer for `key` and return a
/// `GpuMesh` handle.
pub fn allocate_cube_mesh(r: &mut Renderer, key: &UnlitPipelineKey) -> GpuMesh {
    allocate_offset_cube_mesh(r, key, glam::Vec3::ZERO)
}

/// Allocate a cube mesh whose vertices are offset by `offset` in mesh space,
/// returning the `GpuMesh` handle.
///
/// The offset is baked into the vertices, so two cubes allocated from
/// different offsets draw differently even at the same transform — which is
/// what tells a mesh apart from the one whose pool range it sits next to.
pub fn allocate_offset_cube_mesh(
    r: &mut Renderer,
    key: &UnlitPipelineKey,
    offset: glam::Vec3,
) -> GpuMesh {
    let (positions, uvs, colors, indices) = cube();
    let positions = positions
        .into_iter()
        .map(|position| {
            [
                position[0] + offset.x,
                position[1] + offset.y,
                position[2] + offset.z,
            ]
        })
        .collect::<Vec<_>>();
    r.allocate_unlit_mesh(key, &positions, Some(&uvs), Some(&colors), Some(&indices))
}

/// Allocate a dense grid of cubes through the renderer for `key`, returning
/// the `GpuMesh` handle.
///
/// `steps` cubes per axis means `steps³` cubes, which is what makes a pool
/// grow inside a test.
pub fn allocate_grid_cube_mesh(r: &mut Renderer, key: &UnlitPipelineKey, steps: u32) -> GpuMesh {
    let (positions, uvs, colors, indices) = grid_cube(steps);
    r.allocate_unlit_mesh(key, &positions, Some(&uvs), Some(&colors), Some(&indices))
}

/// Allocate an offscreen colour target and a matching depth-stencil target,
/// register their views in `renderer`'s resource graph, and bind them as the
/// renderer's render target. Returns the colour texture (for readback).
pub fn bind_offscreen_target(renderer: &mut Renderer, _label: &str) -> wgpu::Texture {
    use wgpu_unlit_render::render_attachments::create_render_target;
    let ft = create_render_target(&renderer.device, COLOR_FORMAT, WIDTH, HEIGHT, 1);
    let color_view = renderer
        .register_texture_and_default_view(ft.color.clone())
        .1;
    let depth_view = renderer
        .graph
        .insert_strong(
            ft.depth
                .create_view(&wgpu::TextureViewDescriptor::default()),
            &[],
        )
        .expect("depth view has no dependencies");
    renderer.set_render_target(Some(color_view), Some(depth_view), None);
    ft.color
}
