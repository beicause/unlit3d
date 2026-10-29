//! GPU coverage for the lit pipeline itself: a frame's lights reaching the
//! surface, the ambient term, and the kinds whose records used to be shaded
//! wrongly.
//!
//! The covered scene in `unlit3d_examples` (the `lit` scene) verifies how the
//! four kinds look; the cases here pin the arithmetic a picture can hide — a
//! light that stops shining, a surface that stops being lit at all — by
//! comparing one frame against the frame the same scene draws with the ambient
//! term alone.

use crate::common::*;
use unlit_ecs::bundle;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

/// How far above the centre the floor reaches, along x and z.
const FLOOR_EXTENT: f32 = 4.0;

/// How many quads the floor is split into along each axis. A lit plane needs no
/// detail of its own, but a split surface keeps the fragment's own interpolated
/// values from hiding a wrong one.
const FLOOR_SEGMENTS: u32 = 8;

/// The camera looks down on the floor from here.
const EYE: glam::Vec3 = glam::Vec3::new(0.0, 5.5, 5.5);

/// The ambient term a frame with a light gets, so the parts no light reaches are
/// still readable rather than black.
const AMBIENT: glam::Vec4 = glam::Vec4::new(0.05, 0.05, 0.06, 0.0);

/// A luma change this large counts as the light having reached a pixel. The
/// ambient colour quantizes to a few levels in sRGB, so a smaller threshold
/// would count rounding as light.
const LIFT: u8 = 8;

/// One lit floor: the world it lives in, the source that draws it, and the
/// target the frames are read back from.
struct Floor {
    world: World,
    gpu: TestGpu,
    source: Entity,
    ambient: Entity,
    target: wgpu::Texture,
}

/// A floor under a camera, drawn by a source of its own.
///
/// The harness's own source registers the unlit family only; a lit scene needs
/// `register_lit_family`, which is what makes the source cluster the frame's
/// lights at all.
fn floor(ctx: &Ctx) -> Floor {
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, ctx);
    let mut source = MeshSource::new(&world.root_scope(), gpu.context);
    source.register_lit_family(&world.root_scope());
    let source = gpu.spawn_source(&mut world, source);
    let key = LitPipelineKey::new(default_options(&ctx.device));

    let mesh = with_mesh_source(&world, source, |source, scope| {
        source.allocate_lit_mesh(
            scope,
            &key.options,
            LitMeshDesc {
                positions: &floor_positions(),
                normals: Some(&floor_normals()),
                indices: Some(&floor_indices()),
                ..Default::default()
            },
        )
    });
    let camera = camera_looking_at(EYE, glam::Vec3::ZERO, WIDTH as f32 / HEIGHT as f32);
    let mut scope = world.scope_mut(source).expect("the source is a scope");
    scope.spawn(bundle![camera]);
    scope.spawn(bundle![mesh, GpuRenderPipeline::new(key.clone())]);
    let ambient = scope.spawn(bundle![AmbientLight::new(AMBIENT)]);

    let target = gpu.bind_offscreen_target(&world, "test::lit_floor");
    Floor {
        world,
        gpu,
        source,
        ambient,
        target,
    }
}

impl Floor {
    /// Draw one frame and read it back.
    fn frame(&self, ctx: &Ctx) -> Frame {
        self.gpu.render(&self.world);
        Frame {
            rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &self.target),
            width: WIDTH,
            height: HEIGHT,
        }
    }

    /// Give the scene a light, through a transform of its own.
    fn place(&mut self, light: impl Light, translation: glam::Vec3, scale: glam::Vec3) {
        let rotation = glam::Quat::from_rotation_arc(glam::Vec3::NEG_Z, glam::Vec3::NEG_Y);
        let mut scope = self
            .world
            .scope_mut(self.source)
            .expect("the source is a scope");
        scope.spawn(bundle![
            Transform {
                translation,
                rotation,
                scale,
            },
            LightSource::new(light),
        ]);
    }

    /// Replace the ambient light this floor draws with, which is a component
    /// rather than a source field: the scene owns the entity, so changing it
    /// changes what every later frame sums.
    fn set_ambient(&mut self, ambient: glam::Vec4) {
        let mut light = self
            .world
            .root_scope()
            .query_one::<&mut AmbientLight>(self.ambient)
            .expect("the floor's ambient light is alive");
        light.color = ambient;
    }

    /// The pixels the floor covers: everything the ambient term alone lifts off
    /// the background.
    fn lit_pixels(&self, baseline: &Frame) -> usize {
        (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| off_background(baseline.pixel_u8(x, y)))
            .count()
    }
}

/// The floor's vertices: a square centred on the origin, in the y = 0 plane.
fn floor_positions() -> Vec<[f32; 3]> {
    let mut positions = Vec::new();
    for row in 0..=FLOOR_SEGMENTS {
        for column in 0..=FLOOR_SEGMENTS {
            let x = -FLOOR_EXTENT + 2.0 * FLOOR_EXTENT * column as f32 / FLOOR_SEGMENTS as f32;
            let z = -FLOOR_EXTENT + 2.0 * FLOOR_EXTENT * row as f32 / FLOOR_SEGMENTS as f32;
            positions.push([x, 0.0, z]);
        }
    }
    positions
}

/// One normal per floor vertex: the plane faces up.
fn floor_normals() -> Vec<[f32; 3]> {
    vec![[0.0, 1.0, 0.0]; floor_positions().len()]
}

/// The floor's triangles, wound so the up face is the front one.
fn floor_indices() -> Vec<u32> {
    let stride = FLOOR_SEGMENTS + 1;
    let mut indices = Vec::new();
    for row in 0..FLOOR_SEGMENTS {
        for column in 0..FLOOR_SEGMENTS {
            let corner = row * stride + column;
            indices.extend([
                corner,
                corner + stride,
                corner + stride + 1,
                corner,
                corner + stride + 1,
                corner + 1,
            ]);
        }
    }
    indices
}

/// Whether a pixel is brighter than what a pass that clears to black leaves
/// behind, by more than the rounding an sRGB target introduces.
fn off_background(pixel: [u8; 4]) -> bool {
    pixel[..3].iter().any(|&channel| channel > 4)
}

/// The pixels `lit` lifts over `baseline`, and the pixels it does not.
fn lift(baseline: &Frame, lit: &Frame) -> (usize, usize) {
    let mut lifted = 0;
    let mut kept = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let before = baseline.pixel_u8(x, y);
            let after = lit.pixel_u8(x, y);
            if off_background(before) {
                let (b, a) = (luma(before), luma(after));
                if a > b + i32::from(LIFT) {
                    lifted += 1;
                } else {
                    kept += 1;
                }
            }
        }
    }
    (lifted, kept)
}

/// A pixel's brightness, in the units the comparison works in.
fn luma(pixel: [u8; 4]) -> i32 {
    i32::from(pixel[0]) * 3 + i32::from(pixel[1]) * 4 + i32::from(pixel[2]) * 2
}

/// A light at infinity lifts every lit pixel at once: it reaches the whole
/// surface, so nothing the ambient term alone drew may stay behind.
async fn a_directional_light_lifts_every_lit_pixel() {
    let ctx = Ctx::headless().await;
    let mut scene = floor(&ctx);
    let baseline = scene.frame(&ctx);
    let lit = scene.lit_pixels(&baseline);

    scene.place(
        DirectionalLight {
            color: glam::Vec4::new(1.0, 1.0, 1.0, 0.0),
            intensity: 0.6,
        },
        glam::Vec3::ZERO,
        glam::Vec3::ONE,
    );
    let after = scene.frame(&ctx);
    let (lifted, kept) = lift(&baseline, &after);

    assert!(
        kept < lit / 20,
        "a directional light should reach nearly every lit pixel: {lifted} lifted, {kept} kept of {lit}"
    );
}

/// A point light lifts the pixels its range reaches and leaves the rest alone.
/// A frame where the patch covers the whole floor, or none of it, is what a
/// light that is clustered into the wrong cells looks like.
async fn a_point_light_lifts_only_the_patch_it_reaches() {
    let ctx = Ctx::headless().await;
    let mut scene = floor(&ctx);
    let baseline = scene.frame(&ctx);
    let lit = scene.lit_pixels(&baseline);

    scene.place(
        PointLight {
            color: glam::Vec4::ONE,
            intensity: 2.0,
            range: 2.0,
        },
        glam::Vec3::new(0.0, 1.2, 0.0),
        glam::Vec3::ONE,
    );
    let after = scene.frame(&ctx);
    let (lifted, kept) = lift(&baseline, &after);

    assert!(
        lifted > 0,
        "the point light should light the floor under it"
    );
    assert!(
        kept > lit / 2,
        "a point light with a short range should leave most of the floor in the ambient term: \
         {lifted} lifted, {kept} kept of {lit}"
    );
}

/// A cone whose inner and outer angles are the same has no gap to divide by,
/// which the record states as a zero reciprocal. The shader must read that as a
/// hard edge and keep shining, not as a zero multiplier.
async fn a_hard_edged_spot_light_still_shines() {
    let ctx = Ctx::headless().await;
    let mut scene = floor(&ctx);
    let baseline = scene.frame(&ctx);

    scene.place(
        SpotLight {
            color: glam::Vec4::ONE,
            intensity: 4.0,
            range: 6.0,
            inner_angle: 0.5,
            outer_angle: 0.5,
        },
        glam::Vec3::new(0.0, 2.0, 0.0),
        glam::Vec3::ONE,
    );
    let after = scene.frame(&ctx);
    let (lifted, _) = lift(&baseline, &after);

    assert!(
        lifted > 0,
        "a spot light whose cone has no gap should still light the floor"
    );
}

/// A quad light under a transform with a negative determinant is mirrored: the
/// corner order that faced the floor now faces away from it. The record has to
/// be wound in the world it lands in, or the whole light draws nothing.
async fn a_mirrored_quad_light_still_shines() {
    let ctx = Ctx::headless().await;
    let mut scene = floor(&ctx);
    let baseline = scene.frame(&ctx);

    scene.place(
        QuadLight::rect(glam::Vec4::ONE, 1.0, 6.0, 2.0, 2.0),
        glam::Vec3::new(0.0, 1.5, 0.0),
        // Mirroring x turns the polygon over without moving it, which is the
        // transform a caller mirroring a whole object gets for free.
        glam::Vec3::new(-1.0, 1.0, 1.0),
    );
    let after = scene.frame(&ctx);
    let (lifted, _) = lift(&baseline, &after);

    assert!(
        lifted > 0,
        "a quad light under a mirroring transform should still light the floor"
    );
}

/// The ambient term is the frame's, not a cell's: every lit pixel receives it,
/// and raising it raises them all.
async fn the_ambient_term_reaches_every_lit_pixel() {
    let ctx = Ctx::headless().await;
    let mut scene = floor(&ctx);
    let dim = scene.frame(&ctx);
    let lit = scene.lit_pixels(&dim);

    scene.set_ambient(glam::Vec4::new(0.3, 0.3, 0.3, 0.0));
    let bright = scene.frame(&ctx);
    let (lifted, kept) = lift(&dim, &bright);

    assert!(
        kept < lit / 20,
        "the ambient term belongs to the frame, so raising it lifts every lit pixel: \
         {lifted} lifted, {kept} kept of {lit}"
    );
}

/// Allocate a mesh through `source` rather than through the harness's own
/// source, which is what a test with a source of its own needs.
fn with_mesh_source<R>(
    world: &World,
    source: Entity,
    f: impl FnOnce(&mut MeshSource, &unlit_ecs::Scope<'_>) -> R,
) -> R {
    let mut handle = world
        .root_scope()
        .query_one::<&mut Source>(source)
        .expect("the source entity exists");
    let mesh = handle
        .as_mut::<MeshSource>()
        .expect("the source is a MeshSource");
    let scope = world.scope(source).expect("the source entity is alive");
    f(mesh, &scope)
}

gpu_tests! {
    a_directional_light_lifts_every_lit_pixel,
    a_point_light_lifts_only_the_patch_it_reaches,
    a_hard_edged_spot_light_still_shines,
    a_mirrored_quad_light_still_shines,
    the_ambient_term_reaches_every_lit_pixel,
}
