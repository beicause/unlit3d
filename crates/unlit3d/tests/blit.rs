//! End-to-end GPU tests for [`BlitSource`], a frame source that copies one
//! world texture onto the frame's target.
//!
//! The blit reads its view and sampler out of the frame's resource graph, so
//! these tests exercise the property the graph wiring exists for: that a
//! texture registered in the graph reaches the rasterizer, and that swapping
//! the component for another texture swaps what the frame shows.

pub mod common;

use common::*;
use unlit_wgpu::resources::TextureExt;
use unlit_wgpu_test_util::{gpu_test_main, gpu_tests};
use unlit3d::prelude::*;

/// The side of the solid source texture, in texels.
///
/// A row of `SIZE` RGBA texels is 256 bytes, which is
/// [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`], so `write_texture` needs no
/// padding.
const SIZE: u32 = 64;

/// Allocate a `SIZE`-square texture of one colour, ready to be sampled.
fn solid_texture(device: &wgpu::Device, queue: &wgpu::Queue, rgba: [u8; 4]) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test::blit::source"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let row = rgba.repeat(SIZE as usize);
    let data = row.repeat(SIZE as usize);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
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
    texture
}

/// The RGBA of the target's centre pixel.
fn centre_pixel(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> [u8; 4] {
    let rgba = unlit_wgpu::readback::readback_texture(device, queue, target);
    let width = WIDTH as usize;
    let offset = ((HEIGHT as usize / 2) * width + width / 2) * 4;
    rgba[offset..offset + 4].try_into().expect("four channels")
}

/// A world drawing [`BlitSource`] from the texture `rgba`, with the frame's
/// target bound.
///
/// The blit declares no depth state, so the target it draws into carries no
/// depth attachment: wgpu requires the two to match.
fn blit_world(ctx: &Ctx, rgba: [u8; 4]) -> (World, TestGpu, wgpu::Texture) {
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, ctx);
    let texture = solid_texture(&ctx.device, &ctx.queue, rgba);
    let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
    let blit = BlitTexture::new(&world, gpu.context, view);
    world.spawn((blit,));
    world.spawn_source(BlitSource::new());
    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    (world, gpu, target)
}

/// The blit copies the texture the graph holds onto the whole target.
async fn blit_copies_the_source_texture() {
    let ctx = Ctx::headless().await;
    const COLOR: [u8; 4] = [200, 100, 50, 255];
    let (world, gpu, target) = blit_world(&ctx, COLOR);

    gpu.render(&world);

    // The colour survives the sRGB round trip unchanged: the source and the
    // target share a format, so the fragment shader's linear sample is
    // re-encoded exactly back to the byte that was written.
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target),
        COLOR,
        "the blit should copy the source texture to the target"
    );
}

/// A texture swapped into the world through its component re-samples the frame,
/// which is what makes the graph ids the component carries the blit's input.
async fn a_new_texture_component_changes_what_the_frame_shows() {
    let ctx = Ctx::headless().await;
    const FIRST: [u8; 4] = [220, 40, 40, 255];
    const SECOND: [u8; 4] = [40, 220, 40, 255];
    let (mut world, gpu, target) = blit_world(&ctx, FIRST);

    gpu.render(&world);
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target),
        FIRST,
        "the first texture should reach the frame"
    );

    // Replace the component with a blit of the second texture. The source
    // keeps its pipeline and layout; the new graph ids get their own group.
    let texture = solid_texture(&ctx.device, &ctx.queue, SECOND);
    let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
    let blit = BlitTexture::new(&world, gpu.context, view);
    let entity = world
        .query::<&BlitTexture>()
        .next()
        .map(|(entity, _)| entity)
        .expect("the world has a blit texture");
    world.despawn(entity);
    world.spawn((blit,));
    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target),
        SECOND,
        "the second texture should reach the frame"
    );
}

// The registry both runners drive: `cargo nextest` natively, and a
// browser through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    blit_copies_the_source_texture,
    a_new_texture_component_changes_what_the_frame_shows,
}

gpu_test_main!(all_tests());
