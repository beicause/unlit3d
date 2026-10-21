//! End-to-end GPU tests for a frame made of several passes in one world.
//!
//! A pass records the scenes of the sources in its own scope, so a source's
//! place in the entity tree is what chooses the pass — and a source under a
//! nested pass is reached by both that pass and the one around it, one scene
//! each. These tests exercise that from the outside: what ends up in each
//! target, which sources are built at all, and what a shared source draws into.

pub mod common;

use core::cell::Cell;
use std::rc::Rc;

use common::*;
use unlit_ecs::Scope;
use unlit_ecs::bundle;
use unlit_wgpu::scene::Scene;
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
        label: Some("test::passes::source"),
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

/// Bind a fresh offscreen target to `pass`, and return the colour texture.
///
/// [`TestGpu`] binds its own pass; a frame with several passes has to say which
/// one it means.
fn bind_target(
    gpu: &TestGpu,
    world: &World,
    pass: Entity,
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    let device = world
        .root_scope()
        .query_one::<&wgpu::Device>(gpu.context.device)
        .expect("the context's device")
        .clone();
    let color = create_color_target(&device, COLOR_FORMAT, WIDTH, HEIGHT);
    let (_, color_view) = gpu.register_texture(world, color.clone());
    let attachments = FrameAttachments::new(
        world,
        gpu.context,
        COLOR_FORMAT,
        (WIDTH, HEIGHT),
        samples,
        with_depth,
    );
    world
        .root_scope()
        .query_one::<&mut FramePass>(pass)
        .map(|mut pass| {
            pass.set_attachments(&attachments, color_view);
        })
        .expect("the pass entity exists");
    color
}

/// The RGBA of `target`'s centre pixel.
fn centre_pixel(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> [u8; 4] {
    let rgba = unlit_wgpu::readback::readback_texture(device, queue, target);
    let width = WIDTH as usize;
    let offset = ((HEIGHT as usize / 2) * width + width / 2) * 4;
    rgba[offset..offset + 4].try_into().expect("four channels")
}

/// Spawn a pass under `parent`, ordered after it.
fn spawn_pass(world: &mut World, parent: Entity, order: i32) -> Entity {
    world
        .scope_mut(parent)
        .expect("the parent entity is its own scope")
        .spawn(bundle!(
            FramePass::new(FramePassOrder(order)),
            RenderLoadOps::default()
        ))
}

/// A source that draws nothing and counts how many times it was built.
///
/// Which passes reach a source, and how often, is not visible in the pixels; a
/// source's build is where a pass declares that it records the source.
struct CountingSource {
    builds: Rc<Cell<u32>>,
}

impl FrameSource for CountingSource {
    fn build_scene(
        &mut self,
        _scope: &Scope<'_>,
        _ctx: RenderContext,
        _info: PassInfo,
        _scene: &mut Scene,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        self.builds.set(self.builds.get() + 1);
    }

    fn order(&self) -> FrameSourceOrder {
        FrameSourceOrder::MESH
    }
}

/// Two passes of one world each draw their own sources, in one frame.
async fn two_passes_draw_their_own_sources() {
    let ctx = Ctx::headless().await;
    const FIRST: [u8; 4] = [200, 100, 50, 255];
    const SECOND: [u8; 4] = [40, 80, 220, 255];
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let first_pass = gpu.pass();
    let root = world.root_scope().entity();

    // A blit declares no depth state, so neither target carries a depth
    // attachment: wgpu requires the two to match.
    let first_target = gpu.bind_offscreen_target_with(&world, 1, false);
    let second_pass = spawn_pass(&mut world, root, 1);
    let second_target = bind_target(&gpu, &world, second_pass, 1, false);

    let texture = solid_texture(&ctx.device, &ctx.queue, FIRST);
    let (_, view) = gpu.register_texture(&world, texture);
    gpu.spawn_source(&mut world, BlitSource::new(view));

    let texture = solid_texture(&ctx.device, &ctx.queue, SECOND);
    let (_, view) = gpu.register_texture(&world, texture);
    world
        .scope_mut(second_pass)
        .expect("the pass entity is its own scope")
        .spawn(bundle!(Source::new(BlitSource::new(view)),));

    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &first_target),
        FIRST,
        "the first pass should draw its own source"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &second_target),
        SECOND,
        "the second pass should draw its own source, not the first pass's"
    );
    assert_ne!(first_pass, second_pass);
}

/// A source under a nested pass is built once for each pass that reaches it.
async fn a_nested_source_is_built_by_every_pass_above_it() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let outer_pass = gpu.pass();
    let nested_pass = spawn_pass(&mut world, outer_pass, 1);
    gpu.bind_offscreen_target_with(&world, 1, false);
    bind_target(&gpu, &world, nested_pass, 1, false);

    let nested_builds = Rc::new(Cell::new(0));
    world
        .scope_mut(nested_pass)
        .expect("the pass entity is its own scope")
        .spawn(bundle!(Source::new(CountingSource {
            builds: nested_builds.clone(),
        }),));

    let sibling_builds = Rc::new(Cell::new(0));
    world
        .scope_mut(outer_pass)
        .expect("the pass entity is its own scope")
        .spawn(bundle!(Source::new(CountingSource {
            builds: sibling_builds.clone(),
        }),));

    gpu.render(&world);

    assert_eq!(
        nested_builds.get(),
        2,
        "the outer pass and the nested one both reach a source inside the nested pass"
    );
    assert_eq!(
        sibling_builds.get(),
        1,
        "a source beside the nested pass is reached by the outer pass only"
    );
}

/// A source no pass reaches draws nothing, and so does a world with no pass.
async fn a_source_no_pass_reaches_draws_nowhere() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let target = gpu.bind_offscreen_target_with(&world, 1, false);

    let builds = Rc::new(Cell::new(0));
    world
        .root_scope_mut()
        .spawn(bundle!(Source::new(CountingSource {
            builds: builds.clone(),
        }),));

    gpu.render(&world);

    assert_eq!(
        builds.get(),
        0,
        "a source beside the passes is reached by none of them"
    );
    let rgba = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target);
    assert!(
        rgba.as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [0, 0, 0]),
        "a pass with no source to record should still only apply its load ops"
    );

    // With the only pass gone the frame has nothing to record at all, and
    // still has to render without complaint.
    let pass = gpu.pass();
    assert!(
        world.root_scope_mut().despawn(pass),
        "the pass entity should be despawning"
    );
    gpu.render(&world);
    assert_eq!(builds.get(), 0, "a world with no pass reaches no source");
}

/// A parked pass records nothing, and neither does anything under it.
async fn a_parked_pass_draws_nothing() {
    let ctx = Ctx::headless().await;
    const COLOR: [u8; 4] = [40, 200, 90, 255];
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let target_a = gpu.bind_offscreen_target_with(&world, 1, false);
    let root = world.root_scope().entity();
    let pass_b = spawn_pass(&mut world, root, 1);
    let target_b = bind_target(&gpu, &world, pass_b, 1, false);
    // A load op of its own, so opening the pass at all would be visible: the
    // target is zeroed when it is created, so a parked pass leaves it black
    // while a recorded one would clear it to this red.
    world
        .root_scope()
        .query_one::<&mut RenderLoadOps>(pass_b)
        .map(|mut load| load.color = wgpu::LoadOp::Clear(wgpu::Color::RED))
        .expect("the pass carries its load ops");

    let texture = solid_texture(&ctx.device, &ctx.queue, COLOR);
    let (_, view) = gpu.register_texture(&world, texture);
    world
        .scope_mut(pass_b)
        .expect("the pass entity is its own scope")
        .spawn(bundle!(Source::new(BlitSource::new(view)),));

    world
        .root_scope()
        .query_one::<&mut FramePass>(pass_b)
        .map(|mut pass| pass.park())
        .expect("the pass entity exists");
    gpu.render(&world);

    let untouched = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target_b);
    assert!(
        untouched.iter().all(|&channel| channel == 0),
        "a parked pass should not be opened, so it neither draws nor clears"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target_a),
        [0, 0, 0, 255],
        "the other pass still records, and has no source to draw"
    );
}

/// A source in the shared subtree is drawn into every pass.
async fn a_shared_source_draws_into_every_pass() {
    let ctx = Ctx::headless().await;
    const COLOR: [u8; 4] = [30, 200, 120, 255];
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let root = world.root_scope().entity();
    let first_target = gpu.bind_offscreen_target_with(&world, 1, false);
    let second_pass = spawn_pass(&mut world, root, 1);
    let second_target = bind_target(&gpu, &world, second_pass, 1, false);

    let texture = solid_texture(&ctx.device, &ctx.queue, COLOR);
    let (_, view) = gpu.register_texture(&world, texture);
    let shared = world.shared();
    world
        .scope_mut(shared)
        .expect("the shared subtree is its own scope")
        .spawn(bundle!(Source::new(BlitSource::new(view)),));

    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &first_target),
        COLOR,
        "the first pass should draw the shared source"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &second_target),
        COLOR,
        "the second pass should draw the shared source too"
    );
}

// The registry both runners drive: `cargo nextest` natively, and a
// browser through the wasm export `gpu_test_main!` adds.
gpu_tests! {
    two_passes_draw_their_own_sources,
    a_nested_source_is_built_by_every_pass_above_it,
    a_source_no_pass_reaches_draws_nowhere,
    a_parked_pass_draws_nothing,
    a_shared_source_draws_into_every_pass,
}

gpu_test_main!(all_tests());
