//! GPU coverage for procedural solids: a boolean's cut surface reaching the
//! frame, and the mesh it was read out of being the one that was drawn.
//!
//! The covered `csg_boolean` scene shows the three operations side by side; the
//! cases here pin what a picture of it cannot: that the cut reshaped the inside
//! of a solid, that a union added what it claims to, and that the exported
//! triangles are wound the way the lit pipeline culls.
//!
//! Every case compares frames against each other rather than against a stored
//! image, so they hold on any device that renders the scene at all; the
//! scene's own picture is the snapshot.

use crate::common::{Ctx, Frame, HEIGHT, TestGpu, WIDTH, camera_looking_at};
use unlit_ecs::bundle;
use unlit_wgpu::pipeline::LitOptions;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::manifold::manifold_rust::linalg::Vec3;
use unlit3d::manifold::manifold_rust::manifold::Manifold;
use unlit3d::manifold::{BooleanOp, boolean, streams};
use unlit3d::prelude::*;

/// How far the camera sits from the origin, along `Z`.
const CAMERA_DISTANCE: f32 = 6.0;

/// What every case cuts, merges or draws: the shape the scene draws.
fn block() -> Manifold {
    Manifold::cube(Vec3::new(2.0, 1.6, 2.0), true)
}

/// The same block bored through the face the camera looks at and the one
/// behind it.
///
/// The drill is longer than the block is deep, so the hole opens on both faces;
/// a shorter one would leave a cavity a closed solid never shows, and a frame
/// that dropped the cut would be indistinguishable from the block's.
fn bored_cut() -> Manifold {
    let drill = Manifold::cylinder(3.0, 0.5, 0.5, 48);
    boolean(&block(), &drill, BooleanOp::Difference).expect("a bore is a solid")
}

/// A scene holding one source and one camera, ready for a solid.
struct Scene {
    world: World,
    gpu: TestGpu,
    source: Entity,
    target: wgpu::Texture,
}

impl Scene {
    /// A scene whose source draws lit meshes.
    fn new(ctx: &Ctx) -> Self {
        let mut world = World::new();
        let gpu = TestGpu::new(&mut world, ctx);
        let source = gpu.mesh_source();
        let target = gpu.bind_offscreen_target(&world, "test::manifold_scene");
        // The camera and the lights belong to the scene, so they are placed
        // once at spawn: a second camera would draw into the same target twice
        // and a second ambient light would double the term.
        let camera = camera_looking_at(
            glam::Vec3::new(0.0, 0.0, CAMERA_DISTANCE),
            glam::Vec3::ZERO,
            WIDTH as f32 / HEIGHT as f32,
        );
        let mut scope = world.scope_mut(source);
        scope.spawn(bundle![camera]);
        // Ambient light keeps every surface visible, and one directional light
        // from the side is what makes two faces of the same solid differ: a
        // bore's wall then shades unlike the face the camera looks at, which is
        // the only way a bore shows in a picture at all.
        scope.spawn(bundle![AmbientLight::new(glam::Vec4::new(
            0.25, 0.25, 0.3, 0.0
        ))]);
        // A light shines along its transform's negative z, so a turn of -90
        // degrees about x points it down; the turn about y then rakes it across
        // the block from one side, which is what separates a vertical bore wall
        // from the horizontal faces around it.
        let shine = glam::Quat::from_rotation_y(0.5) * glam::Quat::from_rotation_x(-0.9);
        scope.spawn(bundle![
            Transform {
                translation: glam::Vec3::ZERO,
                rotation: shine,
                scale: glam::Vec3::ONE,
            },
            LightSource::new(DirectionalLight {
                color: glam::Vec4::new(1.0, 1.0, 1.0, 0.0),
                intensity: 1.0,
            }),
        ]);
        Self {
            world,
            gpu,
            source,
            target,
        }
    }

    /// Upload a solid and spawn the entity that draws it.
    fn spawn_solid(&mut self, solid: &Manifold) {
        let mut mesh = streams(solid).expect("a boolean of two solids is a solid");
        let gpu = &self.gpu;
        let mesh = gpu.with_mesh_source(&self.world, |source, scope| {
            source.allocate_lit_mesh(scope, mesh.borrowed())
        });
        let key = LitPipelineKey::new(LitOptions::standard());
        self.world.scope_mut(self.source).spawn(LitMeshBundle {
            transform: Transform::default(),
            primitive: LitMeshPrimitive::new(mesh, GpuRenderPipeline::new(key)),
            skin: None,
            morph: None,
        });
    }

    /// Draw one frame and read it back.
    async fn frame(&self, ctx: &Ctx) -> Frame {
        self.gpu.render(&self.world);
        Frame {
            rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &self.target)
                .await,
            width: WIDTH,
            height: HEIGHT,
        }
    }
}

/// The pixels that differ between two frames.
///
/// Counting pixels rather than comparing colours is what makes the count
/// independent of the clear colour and of how a given device rounds it: a pixel
/// a solid reached differs from the same pixel before it was spawned.
fn uncovered_by(before: &Frame, after: &Frame) -> usize {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| before.pixel_u8(x, y) != after.pixel_u8(x, y))
        .count()
}

/// A block covers some of the frame, which is what a solid drawn through the
/// back-face cull has to do.
///
/// The standard options cull back faces, so a solid whose triangles came back
/// wound the other way would draw nothing at all — no error, no warning, an
/// empty frame.
async fn a_boolean_result_draws_through_the_back_face_cull() {
    let ctx = Ctx::headless().await;
    let mut scene = Scene::new(&ctx);
    let empty = scene.frame(&ctx).await;
    scene.spawn_solid(&block());
    let drawn = scene.frame(&ctx).await;
    let covered = uncovered_by(&empty, &drawn);
    assert!(covered > 0, "a solid should cover part of the frame");
}

/// A bore through a block changes what the frame shows without changing the
/// silhouette it covers.
///
/// A bore leaves the block's outline alone — the hole opens on the face the
/// camera looks at and on the one behind it, so every pixel the solid covered
/// it still covers. What changes is the surface inside: the bore's wall shades
/// unlike the face around it. A frame that dropped the cut would be exactly the
/// block's frame, which is what makes the pixel difference the evidence, and
/// the unchanged outline is what says the boolean cut rather than resized.
async fn a_bore_changes_the_pixels_without_moving_the_silhouette() {
    let ctx = Ctx::headless().await;

    let mut whole = Scene::new(&ctx);
    let empty = whole.frame(&ctx).await;
    whole.spawn_solid(&block());
    let whole_frame = whole.frame(&ctx).await;
    let whole_covered = uncovered_by(&empty, &whole_frame);

    let mut bored = Scene::new(&ctx);
    bored.spawn_solid(&bored_cut());
    let bored_frame = bored.frame(&ctx).await;
    let bored_covered = uncovered_by(&empty, &bored_frame);

    assert_eq!(
        bored_covered, whole_covered,
        "a bore through two opposite faces should leave the outline alone"
    );
    let changed = uncovered_by(&whole_frame, &bored_frame);
    assert!(
        changed > whole_covered / 50,
        "the bore's wall should shade unlike the face around it: {changed} pixels changed of {whole_covered} covered"
    );
}

/// Merging a cylinder into a block covers more of the frame than the block.
async fn a_union_covers_more_of_the_frame_than_the_block() {
    let ctx = Ctx::headless().await;

    let mut whole = Scene::new(&ctx);
    let empty = whole.frame(&ctx).await;
    whole.spawn_solid(&block());
    let whole_frame = whole.frame(&ctx).await;
    let whole_covered = uncovered_by(&empty, &whole_frame);

    let mut merged = Scene::new(&ctx);
    let empty = merged.frame(&ctx).await;
    // A drill shorter than the block is deep, so the union is the block with a
    // cylinder buried in it: the shape a caller makes when it fuses two parts
    // that overlap rather than when it cuts a channel.
    let plug = Manifold::cylinder(1.0, 0.9, 0.9, 48);
    let fused = boolean(&block(), &plug, BooleanOp::Union).expect("a union is a solid");
    merged.spawn_solid(&fused);
    let merged_frame = merged.frame(&ctx).await;
    let merged_covered = uncovered_by(&empty, &merged_frame);

    assert!(
        merged_covered > whole_covered,
        "the union should cover more pixels: {merged_covered} against {whole_covered} for the block alone"
    );
}

gpu_tests! {
    a_boolean_result_draws_through_the_back_face_cull,
    a_bore_changes_the_pixels_without_moving_the_silhouette,
    a_union_covers_more_of_the_frame_than_the_block,
}
