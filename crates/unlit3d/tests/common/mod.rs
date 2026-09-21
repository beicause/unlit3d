//! Shared harness for unlit3d GPU integration tests.
//!
//! Re-exports general GPU test helpers and adds crate-specific
//! helpers tailored to the `unlit3d` ECS-based rendering API.

#![expect(unused_imports, reason = "different test files use different subsets")]

pub use wgpu_unlit_test_util::{
    Ctx, Frame, assert_image_snapshot, assert_image_snapshot_with_threshold, bg_entry,
    busy_wait_block_on, count_pixels_off_background, read_texture_bytes, readback_buffer, rgb,
    srgb_to_linear_u8, texel_bytes,
};

use unlit_ecs::LocalWorld;
use unlit3d::prelude::*;

/// Test constants matching what `wgpu_unlit_render`'s own tests use.
pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 192;
pub const CLEAR: [f64; 3] = [0.05, 0.05, 0.08];
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Create a `(Ctx, Renderer, LocalWorld)` triplet ready for testing.
pub fn test_world(ctx: &Ctx) -> (Renderer, LocalWorld) {
    let renderer = create_renderer(ctx);
    let world = LocalWorld::new();
    (renderer, world)
}

/// Build a `Renderer` on `ctx`'s device with standard unlit options.
pub fn create_renderer(ctx: &Ctx) -> Renderer {
    Renderer::new(
        ctx.device.clone(),
        ctx.queue.clone(),
        unlit_options(&ctx.device),
        WIDTH,
        HEIGHT,
    )
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

/// A unit cube centred at the origin, returned as
/// `(positions, uvs, colors, indices)`.
pub fn cube() -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[f32; 4]>, Vec<u32>) {
    use wgpu_unlit_render::pipeline::POSITION_SLOT;
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

    (positions, uvs, colors, indices)
}

/// Camera looking at the origin from (0, 1.2, 3.2).
///
/// Uses reverse-z infinite perspective matching the built-in pipeline's
/// `CompareFunction::Greater` and `depth_clear = 0.0`.
pub fn camera_view(aspect: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = glam::Vec3::new(0.0, 1.2, 3.2);
    let view =
        glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::new(0.0, 0.2, 0.0), glam::Vec3::Y);
    Camera {
        clip_from_world: projection * view,
        position: eye,
    }
}

/// Upload the cube mesh through the renderer and return a `GpuMesh` handle.
pub fn upload_cube_mesh(r: &mut Renderer) -> GpuMesh {
    let (positions, uvs, colors, indices) = cube();
    r.upload_mesh(&positions, &uvs, &colors, &indices)
}

/// A simple offscreen colour target on which to render, returning
/// `(texture, view)`.
pub fn offscreen_target(device: &wgpu::Device, label: &str) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
