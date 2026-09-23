//! Shared harness for unlit3d GPU integration tests.
//!
//! Re-exports general GPU test helpers and adds crate-specific
//! helpers tailored to the `unlit3d` ECS-based rendering API.

pub use wgpu_unlit_test_util::{
    Ctx, Frame, assert_image_snapshot, busy_wait_block_on, count_pixels_off_background,
    read_texture_bytes, texel_bytes,
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

/// Allocate the cube mesh through the renderer for `key` and return a
/// `GpuMesh` handle.
pub fn allocate_cube_mesh(r: &mut Renderer, key: &UnlitPipelineKey) -> GpuMesh {
    let (positions, uvs, colors, indices) = cube();
    r.allocate_unlit_mesh(key, &positions, Some(&uvs), Some(&colors), Some(&indices))
}

/// Allocate an offscreen colour target and a matching depth-stencil target,
/// register their views in `renderer`'s resource graph, and bind them as the
/// renderer's render target. Returns the colour texture (for readback).
pub fn bind_offscreen_target(renderer: &mut Renderer, label: &str) -> wgpu::Texture {
    use wgpu_unlit_render::render_attachments::default_depth_stencil_format;
    let color = renderer.device.create_texture(&wgpu::TextureDescriptor {
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
    let color_view = renderer.register_texture_and_default_view(color.clone()).1;
    let depth = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&format!("{label}::depth")),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: default_depth_stencil_format(&renderer.device),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = renderer
        .graph
        .insert_strong(
            wgpu_unlit_render::resources::Resource::TextureView(
                depth.create_view(&wgpu::TextureViewDescriptor::default()),
            ),
            &[],
        )
        .expect("depth view has no dependencies");
    renderer.set_render_target(Some(color_view), Some(depth_view), None);
    color
}
