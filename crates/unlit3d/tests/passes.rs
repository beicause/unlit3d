//! End-to-end GPU tests for a frame made of several passes in one world.
//!
//! A pass records the scenes of the sources in its own scope, so a source's
//! place in the entity tree is what chooses the pass — and a source under a
//! nested pass is reached by both that pass and the one around it, one scene
//! each. These tests exercise that from the outside: what ends up in each
//! target, which sources are built at all, and what a shared source draws into.

use core::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::common::{COLOR_FORMAT, Ctx, HEIGHT, TestGpu, WIDTH};
use unlit_ecs::Scope;
use unlit_ecs::bundle;
use unlit_wgpu::resources::{ResHandle, TextureView};
use unlit_wgpu::scene::Scene;
use unlit_wgpu_test_util::gpu_tests;
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

/// Bind one fresh offscreen target to every pass in `passes`, and return the
/// colour texture.
///
/// [`TestGpu`] binds its own pass; a frame with several passes has to say which
/// one it means. Binding one target to more than one pass is how a test of
/// pass identity keeps the target out of the difference.
fn bind_shared_target(
    gpu: &TestGpu,
    world: &World,
    passes: &[Entity],
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context's device");
    let color = create_color_target(&device, COLOR_FORMAT, WIDTH, HEIGHT);
    let (_, color_view) = gpu.register_texture(world, color.clone());
    for &pass in passes {
        bind_over(world, gpu, pass, color_view.clone(), samples, with_depth);
    }
    color
}

/// State `pass`'s attachments over an existing colour view.
///
/// Two passes may draw into one colour view with different depth or sample
/// counts; this is what a test of target identity uses to keep the view out of
/// the difference.
fn bind_over(
    world: &World,
    gpu: &TestGpu,
    pass: Entity,
    color_view: ResHandle<TextureView>,
    samples: u32,
    with_depth: bool,
) {
    let attachments = FrameAttachments::new(
        world,
        gpu.context,
        AttachmentDesc {
            format: COLOR_FORMAT,
            size: (WIDTH, HEIGHT),
            sample_count: samples,
            depth: with_depth.then_some(DepthAttachment::Transient),
        },
    );
    let mut row = world.root_scope().query_one::<&mut FramePass>(pass);
    row.get().set_attachments(&attachments, color_view);
}

/// Bind a fresh offscreen target to `pass`, and return the colour texture.
fn bind_target(
    gpu: &TestGpu,
    world: &World,
    pass: Entity,
    samples: u32,
    with_depth: bool,
) -> wgpu::Texture {
    bind_shared_target(gpu, world, &[pass], samples, with_depth)
}

/// The RGBA of `target`'s centre pixel.
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

/// Spawn a pass under `parent`, ordered after it.
fn spawn_pass(world: &mut World, parent: Entity, order: i32) -> Entity {
    world
        .scope_mut(parent)
        .spawn(Pass::new(FramePassOrder(order)))
}

/// A source that draws nothing and counts how many times it was built.
///
/// Which passes reach a source, and how often, is not visible in the pixels; a
/// source's build is where a pass declares that it records the source.
struct CountingSource {
    builds: Rc<Cell<u32>>,
}

impl DrawSource for CountingSource {
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
    gpu.spawn_source(&mut world, SourceOrder::MESH, BlitSource::new(view));

    let texture = solid_texture(&ctx.device, &ctx.queue, SECOND);
    let (_, view) = gpu.register_texture(&world, texture);
    world.scope_mut(second_pass).spawn(bundle![Source::new(
        SourceOrder::MESH,
        BlitSource::new(view)
    )]);

    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &first_target).await,
        FIRST,
        "the first pass should draw its own source"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &second_target).await,
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
    world.scope_mut(nested_pass).spawn(bundle![Source::new(
        SourceOrder::MESH,
        CountingSource {
            builds: nested_builds.clone(),
        }
    ),]);

    let sibling_builds = Rc::new(Cell::new(0));
    world.scope_mut(outer_pass).spawn(bundle![Source::new(
        SourceOrder::MESH,
        CountingSource {
            builds: sibling_builds.clone(),
        }
    ),]);

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
    world.root_scope_mut().spawn(bundle![Source::new(
        SourceOrder::MESH,
        CountingSource {
            builds: builds.clone(),
        }
    ),]);

    gpu.render(&world);

    assert_eq!(
        builds.get(),
        0,
        "a source beside the passes is reached by none of them"
    );
    let rgba = unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target).await;
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
    world
        .root_scope_mut()
        .despawn(pass)
        .expect("the pass entity should be despawning");
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
        .get()
        .color = wgpu::LoadOp::Clear(wgpu::Color::RED);

    let texture = solid_texture(&ctx.device, &ctx.queue, COLOR);
    let (_, view) = gpu.register_texture(&world, texture);
    world.scope_mut(pass_b).spawn(bundle![Source::new(
        SourceOrder::MESH,
        BlitSource::new(view)
    )]);

    world
        .root_scope()
        .query_one::<&mut FramePass>(pass_b)
        .get()
        .park();
    gpu.render(&world);

    let untouched =
        unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &target_b).await;
    assert!(
        untouched.iter().all(|&channel| channel == 0),
        "a parked pass should not be opened, so it neither draws nor clears"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &target_a).await,
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
    world.scope_mut(shared).spawn(bundle![Source::new(
        SourceOrder::MESH,
        BlitSource::new(view)
    )]);

    gpu.render(&world);

    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &first_target).await,
        COLOR,
        "the first pass should draw the shared source"
    );
    assert_eq!(
        centre_pixel(&ctx.device, &ctx.queue, &second_target).await,
        COLOR,
        "the second pass should draw the shared source too"
    );
}

/// What one build of a source was told about where it draws.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    pass: PassId,
    target: RenderTargetId,
    frame: FrameId,
    size: (u32, u32),
    camera: Option<Entity>,
    viewport: Option<ViewportRect>,
}

/// A source that records what each build was told, and draws nothing.
///
/// A build's context is not visible in the pixels, so a source is the only
/// place a test can read it.
struct RecordingSource {
    seen: Rc<RefCell<Vec<Seen>>>,
}

impl SceneSource for RecordingSource {
    fn build(&mut self, build: SceneBuild<'_>, _scene: &mut Scene) {
        let SceneBuild { info, .. } = build;
        self.seen.borrow_mut().push(Seen {
            pass: info.pass,
            target: info.target,
            frame: info.frame,
            size: (info.width, info.height),
            camera: info.camera,
            viewport: info.viewport,
        });
    }
}

async fn a_pass_uses_its_own_camera_and_content_size() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let root = world.root_scope().entity();
    let passes = [gpu.pass(), spawn_pass(&mut world, root, 1)];
    let shared = world.shared();
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context's device");
    let seen = Rc::new(RefCell::new(Vec::new()));
    world.scope_mut(shared).spawn(bundle![Source::new(
        SourceOrder::MESH,
        RecordingSource { seen: seen.clone() }
    )]);

    let mut cases = Vec::new();
    for (pass, size, viewport) in [
        (passes[0], (128, 64), None),
        (
            passes[1],
            (64, 128),
            Some(ViewportRect::new(64.0, 96.0).at(0.0, 16.0)),
        ),
    ] {
        let target = PersistentTarget::bind(
            &world,
            gpu.context,
            pass,
            &device,
            size,
            TargetConfig {
                samples: 1,
                depth: None,
                ..TargetConfig::default()
            },
        )
        .expect("the pass entity exists");
        let scope = world.root_scope();
        let mut row = scope.query_one::<&FramePass>(pass);
        let (width, height) = row
            .get()
            .target_size(&world)
            .expect("the pass target is bound");
        drop(row);
        let aspect = viewport.map_or(width as f32 / height as f32, |rect| {
            rect.width / rect.height
        });
        let camera = world.scope_mut(shared).spawn(bundle![Camera {
            view_from_world: glam::Mat4::IDENTITY,
            clip_from_view: glam::camera::rh::proj::directx::perspective_infinite_reverse(
                1.0, aspect, 0.1,
            ),
            active: false,
        }]);
        let scope = world.root_scope();
        let mut row = scope.query_one::<&mut FramePass>(pass);
        let state = row.get();
        state.camera = Some(camera);
        state.viewport = viewport;
        cases.push((target, camera, size, viewport));
    }

    gpu.render(&world);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 2);
    for ((_, camera, size, viewport), build) in cases.iter().zip(seen.iter()) {
        assert_eq!(build.camera, Some(*camera));
        assert_eq!(build.size, *size);
        assert_eq!(build.viewport, *viewport);
    }
    assert_ne!(seen[0].pass, seen[1].pass);
    assert_ne!(seen[0].target, seen[1].target);
}

/// A source in the shared subtree is built once per pass, and each build names
/// its own pass, the target it draws into, and the frame it belongs to.
async fn a_build_is_told_the_pass_target_and_frame_it_belongs_to() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let first_pass = gpu.pass();
    let root = world.root_scope().entity();
    let second_pass = spawn_pass(&mut world, root, 1);
    // One target for both passes: the two builds then differ in the pass they
    // name and nothing else.
    let shared_target = bind_shared_target(&gpu, &world, &[first_pass, second_pass], 1, false);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let shared = world.shared();
    world.scope_mut(shared).spawn(bundle![Source::new(
        SourceOrder::MESH,
        RecordingSource { seen: seen.clone() }
    )]);

    gpu.render(&world);
    let first_frame = seen.borrow().clone();
    assert_eq!(
        first_frame.len(),
        2,
        "a source the shared subtree places under both passes is built once per pass"
    );
    assert_ne!(
        first_frame[0].pass, first_frame[1].pass,
        "each build names the pass that reached it"
    );
    assert_eq!(
        first_frame[0].target, first_frame[1].target,
        "both passes draw into the one bound target, so both builds name it"
    );
    assert_eq!(
        first_frame[0].frame, first_frame[1].frame,
        "one frame builds every source, so the builds share a frame id"
    );

    // The same passes, one frame later: the frame id moves and the identities
    // of the pass and the target do not. The clock belongs to the frame loop,
    // and a test drives it by hand the way `dispatch_frame` does.
    world
        .root_scope()
        .query::<&mut FrameClock>()
        .iter()
        .for_each(|clock| clock.advance(unlit3d::time::FrameDelta::from_secs(1.0 / 60.0), 1));
    gpu.render(&world);
    let second_frame = seen.borrow()[2..].to_vec();
    assert_eq!(second_frame.len(), 2);
    assert_ne!(
        second_frame[0].frame, first_frame[0].frame,
        "a later frame is a different frame"
    );
    assert_eq!(
        second_frame[0].pass, first_frame[0].pass,
        "the pass keeps its identity across frames"
    );
    assert_eq!(
        second_frame[0].target, first_frame[0].target,
        "the target keeps its identity across frames"
    );
    assert!(
        second_frame.iter().all(|seen| seen.size == (WIDTH, HEIGHT)),
        "a build learns the size of the target it draws into, got {second_frame:?}"
    );

    // A pass bound to another target names that one, so a source keying
    // per-target state tells the two targets apart.
    let other_target = bind_target(&gpu, &world, second_pass, 1, false);
    let _ = other_target;
    seen.borrow_mut().clear();
    gpu.render(&world);
    let moved = seen.borrow().clone();
    assert_eq!(moved.len(), 2);
    assert_ne!(
        moved[0].target, moved[1].target,
        "the rebound pass draws into its own target, which is not the shared one"
    );
    assert_eq!(
        moved
            .iter()
            .filter(|seen| seen.target == first_frame[0].target)
            .count(),
        1,
        "only the pass still bound to the shared target names it, got {moved:?}"
    );
    assert_ne!(shared_target, other_target);
}

/// A target applies its own size policy, so every caller states the size it
/// knows rather than having to resolve it first.
async fn a_target_applies_its_size_policy_to_the_size_it_is_told() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context's device");
    let target = PersistentTarget::bind(
        &world,
        gpu.context,
        gpu.pass(),
        &device,
        (WIDTH, HEIGHT),
        TargetConfig {
            samples: 1,
            depth: None,
            size: IntermediateSize::Scale(0.5),
            ..TargetConfig::default()
        },
    )
    .expect("the pass entity exists");

    assert_eq!(
        target.size(&world),
        Some((WIDTH / 2, HEIGHT / 2)),
        "the scale policy halves what the caller reports"
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&FramePass>(gpu.pass())
            .get()
            .target_size(&world),
        Some((WIDTH / 2, HEIGHT / 2)),
        "the pass reads its own colour attachment, so it cannot disagree with the target"
    );
}

/// A resize leaves one size behind: the target, its attachments and the pass
/// all report the new one, and the next build sees it.
async fn a_resize_leaves_one_size_behind() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context's device");
    let pass = gpu.pass();
    let mut target = PersistentTarget::bind(
        &world,
        gpu.context,
        pass,
        &device,
        (WIDTH, HEIGHT),
        TargetConfig {
            samples: 1,
            depth: Some(DepthAttachment::Transient),
            ..TargetConfig::default()
        },
    )
    .expect("the pass entity exists");
    let seen = Rc::new(RefCell::new(Vec::new()));
    world.scope_mut(pass).spawn(bundle![Source::new(
        SourceOrder::MESH,
        RecordingSource { seen: seen.clone() }
    )]);

    gpu.render(&world);
    assert_eq!(
        seen.borrow().last().map(|seen| seen.size),
        Some((WIDTH, HEIGHT)),
        "the first build sees the size the target was bound at"
    );

    let resized = (WIDTH / 2, HEIGHT / 2);
    target
        .resize(&world, &device, resized)
        .expect("the pass entity exists");
    assert_eq!(
        target.size(&world),
        Some(resized),
        "the target reports the size its texture is allocated at"
    );
    assert_eq!(
        world
            .root_scope()
            .query_one::<&FramePass>(pass)
            .get()
            .target_size(&world),
        Some(resized),
        "the pass reads the attachment it draws into, so a resize cannot leave it behind"
    );

    seen.borrow_mut().clear();
    gpu.render(&world);
    let after = seen.borrow().clone();
    assert_eq!(
        after.first().map(|seen| seen.size),
        Some(resized),
        "the next build is told the resized target, got {after:?}"
    );
}

/// Two passes over one colour view draw into different targets when their
/// other attachments differ.
///
/// The colour view is what pixels land in, but depth and multisampling belong
/// to the attachment set: a source keyed per target would otherwise build one
/// pipeline for a pass that depth-tests and one for a pass that does not.
async fn one_colour_view_is_two_targets_when_the_depth_differs() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let first_pass = gpu.pass();
    let root = world.root_scope().entity();
    let second_pass = spawn_pass(&mut world, root, 1);
    let device = gpu
        .context
        .device(&world.root_scope())
        .expect("the context's device");
    let color = create_color_target(&device, COLOR_FORMAT, WIDTH, HEIGHT);
    let (_, color_view) = gpu.register_texture(&world, color);
    bind_over(&world, &gpu, first_pass, color_view.clone(), 1, false);
    bind_over(&world, &gpu, second_pass, color_view, 1, true);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let shared = world.shared();
    world.scope_mut(shared).spawn(bundle![Source::new(
        SourceOrder::MESH,
        RecordingSource { seen: seen.clone() }
    )]);

    gpu.render(&world);
    let seen = seen.borrow().clone();
    assert_eq!(seen.len(), 2, "both passes reach the shared source");
    assert_eq!(
        seen[0].target.color, seen[1].target.color,
        "both passes resolve into the one colour view"
    );
    assert_ne!(
        seen[0].target.depth, seen[1].target.depth,
        "only one of the passes depth-tests, so the targets differ, got {seen:?}"
    );
    assert_ne!(
        seen[0].target, seen[1].target,
        "a target is its whole attachment set, not the view it resolves into"
    );
}

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
gpu_tests! {
    a_pass_uses_its_own_camera_and_content_size,
    a_build_is_told_the_pass_target_and_frame_it_belongs_to,
    a_target_applies_its_size_policy_to_the_size_it_is_told,
    a_resize_leaves_one_size_behind,
    one_colour_view_is_two_targets_when_the_depth_differs,
    two_passes_draw_their_own_sources,
    a_nested_source_is_built_by_every_pass_above_it,
    a_source_no_pass_reaches_draws_nowhere,
    a_parked_pass_draws_nothing,
    a_shared_source_draws_into_every_pass,
}
