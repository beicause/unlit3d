//! End-to-end GPU tests for [`BlitSource`], a frame source that copies one
//! world texture onto the frame's target.
//!
//! The blit reads its view and sampler out of the frame's resource graph, so
//! these tests exercise the property the graph wiring exists for: that a
//! texture registered in the graph reaches the rasterizer, and that pointing
//! the source at another view swaps what the frame shows.

use crate::common::*;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

/// The side of the solid source texture, in texels.
///
/// A row of `SIZE` RGBA texels is 256 bytes, which is
/// [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`], so `write_texture` needs no
/// padding.
const SIZE: u32 = 64;

/// Allocate a `SIZE`-square texture of one colour in `format`, ready to be
/// sampled.
fn solid_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    rgba: [u8; 4],
) -> wgpu::Texture {
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
        format,
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
async fn centre_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
) -> [u8; 4] {
    let rgba = unlit_wgpu::readback::readback_texture(device, queue, target).await;
    let width = WIDTH as usize;
    let offset = ((HEIGHT as usize / 2) * width + width / 2) * 4;
    rgba[offset..offset + 4].try_into().expect("four channels")
}

/// A world drawing [`BlitSource`] from an sRGB texture of `rgba`, with the
/// frame's target bound.
///
/// The blit declares no depth state, so the target it draws into carries no
/// depth attachment: wgpu requires the two to match.
fn blit_world(ctx: &Ctx, rgba: [u8; 4]) -> (World, TestGpu, wgpu::Texture, Entity) {
    blit_world_from(ctx, wgpu::TextureFormat::Rgba8UnormSrgb, rgba)
}

/// A world drawing [`BlitSource`] from a texture of `rgba` in `format`.
fn blit_world_from(
    ctx: &Ctx,
    format: wgpu::TextureFormat,
    rgba: [u8; 4],
) -> (World, TestGpu, wgpu::Texture, Entity) {
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, ctx);
    let texture = solid_texture(&ctx.device, &ctx.queue, format, rgba);
    let (_, view) = gpu.register_texture(&world, texture);
    let source = gpu.spawn_source(&mut world, FrameSourceOrder::MESH, BlitSource::new(view));
    let target = gpu.bind_offscreen_target_with(&world, 1, false);
    (world, gpu, target, source)
}

/// The blit copies the texture the graph holds onto the whole target.
async fn blit_copies_the_source_texture() {
    let ctx = Ctx::headless().await;
    const COLOR: [u8; 4] = [200, 100, 50, 255];
    let (world, gpu, target, _source) = blit_world(&ctx, COLOR);

    gpu.render(&world);

    // The colour survives the sRGB round trip unchanged: the source and the
    // target share a format, so the fragment shader's linear sample is
    // re-encoded exactly back to the byte that was written.
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target).await,
        COLOR,
        "the blit should copy the source texture to the target"
    );
}

/// Pointing the source at another view re-samples the frame, which is what
/// makes the graph id the source carries its input.
async fn a_new_view_changes_what_the_frame_shows() {
    let ctx = Ctx::headless().await;
    const FIRST: [u8; 4] = [220, 40, 40, 255];
    const SECOND: [u8; 4] = [40, 220, 40, 255];
    let (world, gpu, target, source) = blit_world(&ctx, FIRST);

    gpu.render(&world);
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target).await,
        FIRST,
        "the first texture should reach the frame"
    );

    // Point the source at the second texture. It keeps its pipeline and layout;
    // only the view the bind group samples changes.
    let texture = solid_texture(
        &ctx.device,
        &ctx.queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SECOND,
    );
    let (_, view) = gpu.register_texture(&world, texture);
    {
        let mut row = world.root_scope().query_one::<&mut Source>(source);
        row.get()
            .as_mut::<BlitSource>()
            .expect("the source is a BlitSource")
            .view = view;
    }
    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target).await,
        SECOND,
        "the second texture should reach the frame"
    );
}

/// The centre of a frame whose only source blits `rgba` with premultiplied
/// source-over, which is the blend a document composited over the meshes
/// states.
async fn blended_centre(ctx: &Ctx, rgba: [u8; 4]) -> [u8; 4] {
    let (world, gpu, target, source) = blit_world(ctx, rgba);
    {
        let mut row = world.root_scope().query_one::<&mut Source>(source);
        row.get()
            .as_mut::<BlitSource>()
            .expect("the source is a BlitSource")
            .blend = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
    }
    gpu.render(&world);
    centre_pixel(&ctx.device, &ctx.queue, &target).await
}

/// A source that states a blend state combines with what the pass already holds
/// rather than replacing it.
///
/// The three cases bracket the blend from both ends: a transparent texture
/// leaves the cleared target untouched, an opaque one still reproduces its own
/// colour, and a half-transparent one lands strictly between the two. A
/// composite that ignored the blend state could not produce all three. The
/// texture's RGB is premultiplied by its alpha, which is the convention
/// `wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING` expects and what a painted
/// page's transparent pixels carry.
async fn a_blended_copy_layers_over_the_pass() {
    let ctx = Ctx::headless().await;
    // Straight `(128, 200, 50)` at half coverage.
    const HALF: [u8; 4] = [64, 100, 25, 128];
    const OPAQUE: [u8; 4] = [200, 100, 50, 255];
    const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

    let cleared = blended_centre(&ctx, TRANSPARENT).await;
    let laid_over = blended_centre(&ctx, HALF).await;
    let covered = blended_centre(&ctx, OPAQUE).await;

    assert_eq!(
        covered[..3],
        OPAQUE[..3],
        "an opaque layer hides the target it is drawn over"
    );
    assert_eq!(cleared[3], 255, "the pass's own alpha stays where it was");
    for channel in 0..3 {
        let (low, high) = if cleared[channel] <= HALF[channel] {
            (cleared[channel], HALF[channel])
        } else {
            (HALF[channel], cleared[channel])
        };
        assert!(
            (low..=high).contains(&laid_over[channel]) && laid_over[channel] != cleared[channel],
            "channel {channel}: {} should lie between the target {low} and the texture's              premultiplied {high}",
            laid_over[channel]
        );
    }
}

/// A composite reads the format of the texture it samples, not only the
/// target's.
///
/// A plain `Rgba8Unorm` source hands the fragment the bytes it stores; an sRGB
/// target then encodes whatever it is given, so the picture would brighten
/// unless the fragment decodes first. The two cases are the same bytes into the
/// same target, which differs only in the flag the source's format picks, so
/// the plain one reaching the frame unchanged is what proves the decode ran —
/// and the opaque copy above is the sRGB case of the same table.
async fn a_plain_source_is_decoded_for_an_srgb_target() {
    let ctx = Ctx::headless().await;
    const COLOR: [u8; 4] = [200, 100, 50, 255];
    let (world, gpu, target, _source) =
        blit_world_from(&ctx, wgpu::TextureFormat::Rgba8Unorm, COLOR);

    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target).await,
        COLOR,
        "a plain source should be decoded before the sRGB target encodes it"
    );
}

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    blit_copies_the_source_texture,
    a_new_view_changes_what_the_frame_shows,
    a_blended_copy_layers_over_the_pass,
    a_plain_source_is_decoded_for_an_srgb_target,
}
