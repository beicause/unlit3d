//! GPU coverage for the inverted-hull outline.
//!
//! An outline drawn this way is one mesh drawn twice: the body, and a second
//! copy spread along its own normals and drawn with its front faces culled, so
//! only the band where the spread copy reaches past the body shows. These
//! tests draw that pair and check what the picture has to have — the band
//! exists, it sits outside the body, its width follows the instance's hull
//! width, and the body still wins where the two overlap.
//!
//! What the band looks like is the example's snapshot to pin; what belongs
//! here is the behaviour behind it, which is why every assertion is a count or
//! a containment over classified pixels rather than a stored frame.

use crate::common::*;
use unlit_ecs::bundle;
use unlit_wgpu_test_util::gpu_tests;
use unlit3d::prelude::*;

/// A mesh as the lit pipeline takes it: positions, normals and indices.
type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>);

/// The camera looks at the body from here.
///
/// It sits on the body's axis, so the silhouette it sees is the body's own
/// great circle and a spread along the normals widens it by the spread itself.
const EYE: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 4.0);

/// The body's colour, in the linear space instance colours are in.
const BODY: glam::Vec4 = glam::Vec4::new(0.32, 0.70, 0.58, 1.0);

/// The outline's colour: far darker than the body, so the nearer of the two
/// names any pixel the frame holds.
const INK: glam::Vec4 = glam::Vec4::new(0.01, 0.02, 0.06, 1.0);

/// The body's radius, in world units.
const RADIUS: f32 = 0.5;

/// How many quads the body is split into around its axis and pole to pole.
const SLICES: u32 = 32;
const STACKS: u32 = 16;

/// A hull width wide enough that the band is tens of pixels across.
const SPREAD: f32 = 0.1;

/// A wider hull, so a band that ignored the instance can be told from one
/// that follows it.
const WIDE_SPREAD: f32 = 0.2;

/// A body: a sphere whose vertex normals point away from its centre.
///
/// Spreading along those normals makes a larger sphere, which is what puts the
/// band outside the silhouette. A mesh without normals could not be spread at
/// all — the pipeline rejects an expanding variant without them.
fn sphere(radius: f32, slices: u32, stacks: u32) -> RawMesh {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();
    let row = slices + 1;
    for stack in 0..=stacks {
        let polar = core::f32::consts::PI * stack as f32 / stacks as f32;
        for slice in 0..=slices {
            let azimuth = core::f32::consts::TAU * slice as f32 / slices as f32;
            let normal = glam::Vec3::new(
                polar.sin() * azimuth.cos(),
                polar.cos(),
                polar.sin() * azimuth.sin(),
            );
            normals.push(normal.to_array());
            positions.push((normal * radius).to_array());
        }
    }
    for stack in 0..stacks {
        for slice in 0..slices {
            let corner = stack * row + slice;
            // Wound counter-clockwise seen from outside, so the faces the body
            // draws are the ones the shell culls.
            indices.extend_from_slice(&[
                corner,
                corner + row,
                corner + row + 1,
                corner,
                corner + row + 1,
                corner + 1,
            ]);
        }
    }
    (positions, normals, indices)
}

/// What a pixel of the frame is, as far as these tests care.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mark {
    /// Clear colour: nothing was drawn here.
    Bare,
    /// The outline's colour.
    Ink,
    /// The body's colour.
    Body,
}

/// The sRGB encode the target applies to the linear colour a shader writes.
fn encode(linear: f32) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// A colour as the frame stores it.
fn stored(color: glam::Vec4) -> [u8; 3] {
    [encode(color.x), encode(color.y), encode(color.z)]
}

/// Classify one pixel by which of the two colours it is nearer to.
///
/// Both colours are flat and far apart, so the nearer one names the pixel even
/// where a rasterizer's rounding moved it a little. The clear colour is black,
/// which is nearer the ink than the body, so bare pixels are recognised by
/// being dark outright rather than by distance.
fn mark(pixel: [u8; 4]) -> Mark {
    if pixel[..3].iter().all(|&channel| channel <= 8) {
        return Mark::Bare;
    }
    let distance_to = |reference: [u8; 3]| {
        pixel[..3]
            .iter()
            .zip(reference)
            .map(|(&drawn, reference)| i32::from(drawn).abs_diff(i32::from(reference)))
            .sum::<u32>()
    };
    if distance_to(stored(INK)) <= distance_to(stored(BODY)) {
        Mark::Ink
    } else {
        Mark::Body
    }
}

/// A scene drawing one body outlined by an inverted hull.
struct Hull {
    world: World,
    gpu: TestGpu,
    target: wgpu::Texture,
}

/// Build the scene, outlining the body with a hull `width` wide.
///
/// The two draws share one mesh buffer: the shell is the same mesh with the
/// expanding variant, so the only difference between them is the pipeline and
/// the per-instance values.
fn hull(ctx: &Ctx, width: f32) -> Hull {
    let mut world = World::new();
    let gpu = TestGpu::new(&mut world, ctx);
    let key = gpu.key.clone();
    let shell_key =
        LitPipelineKey::new(unlit_wgpu::pipeline::LitOptions::inverted_hull(&ctx.device));

    let (positions, normals, indices) = sphere(RADIUS, SLICES, STACKS);
    let mesh = gpu.with_mesh_source(&world, |source, scope| {
        source.allocate_lit_mesh(
            scope,
            &key.options,
            LitMeshDesc {
                positions: &positions,
                normals: Some(&normals),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    let camera = camera_looking_at(EYE, glam::Vec3::ZERO, WIDTH as f32 / HEIGHT as f32);
    gpu.spawn(&mut world, bundle![camera]);
    // The shell is spawned first, so a frame that lost depth would still end
    // on the body's draw.
    gpu.spawn(
        &mut world,
        bundle![
            mesh.clone(),
            GpuRenderPipeline::new(shell_key),
            InstanceColor::new(INK),
            InstanceHullWidth::new(width),
        ],
    );
    gpu.spawn(
        &mut world,
        bundle![mesh, GpuRenderPipeline::new(key), InstanceColor::new(BODY),],
    );

    let target = gpu.bind_offscreen_target(&world, "test::inverted_hull");
    Hull { world, gpu, target }
}

impl Hull {
    /// Draw the scene once and read the frame back.
    fn frame(&self, ctx: &Ctx) -> Frame {
        self.gpu.render(&self.world);
        Frame {
            rgba: unlit_wgpu::readback::readback_texture(&ctx.device, &ctx.queue, &self.target),
            width: WIDTH,
            height: HEIGHT,
        }
    }
}

/// How many pixels of `frame` are `want`.
fn count(frame: &Frame, want: Mark) -> usize {
    (0..frame.height)
        .flat_map(|y| (0..frame.width).map(move |x| (x, y)))
        .filter(|&(x, y)| mark(frame.pixel_u8(x, y)) == want)
        .count()
}

/// The first and last column of `frame` that holds `want`.
fn columns(frame: &Frame, want: Mark) -> Option<(u32, u32)> {
    let mut first = None;
    let mut last = None;
    for x in 0..frame.width {
        if (0..frame.height).any(|y| mark(frame.pixel_u8(x, y)) == want) {
            first.get_or_insert(x);
            last = Some(x);
        }
    }
    first.zip(last)
}

/// The marks along one row of the frame, left to right.
fn row(frame: &Frame, y: u32) -> Vec<Mark> {
    (0..frame.width)
        .map(|x| mark(frame.pixel_u8(x, y)))
        .collect()
}

/// The band sits outside the body, and the body is still there under it.
///
/// The shell is the same sphere drawn larger with its front faces culled, so
/// what it adds is a ring: the body's disc with a band around it. A shell that
/// culled the wrong faces would paint the body's own footprint instead, and one
/// whose spread did nothing would leave the body's silhouette untouched.
async fn the_shell_shows_as_a_band_outside_the_body() {
    let ctx = Ctx::headless().await;
    let scene = hull(&ctx, SPREAD);
    let frame = scene.frame(&ctx);

    let body = count(&frame, Mark::Body);
    let ink = count(&frame, Mark::Ink);
    assert!(body > 0, "the body itself should be drawn");
    assert!(
        ink > 0,
        "the shell should show: a body drawn alone leaves no band at all"
    );

    let (body_first, body_last) = columns(&frame, Mark::Body).expect("the body is drawn");
    let (ink_first, ink_last) = columns(&frame, Mark::Ink).expect("the band is drawn");
    assert!(
        ink_first < body_first,
        "the band should reach left of the body: band starts at {ink_first}, body at {body_first}"
    );
    assert!(
        ink_last > body_last,
        "the band should reach right of the body: band ends at {ink_last}, body at {body_last}"
    );
}

/// The band's width is the instance's hull width, not a constant of the mesh.
///
/// Two scenes draw the same mesh with the same pipeline and differ only in the
/// per-instance width. A width baked into the shader or the pipeline would draw
/// the same band twice, so the two frames are told apart by how far the drawn
/// disc reaches.
async fn a_wider_hull_draws_a_wider_band() {
    let ctx = Ctx::headless().await;
    let narrow = hull(&ctx, SPREAD);
    let narrow = narrow.frame(&ctx);
    let wide = hull(&ctx, WIDE_SPREAD);
    let wide = wide.frame(&ctx);

    let span = |frame: &Frame| {
        let (first, last) = columns(frame, Mark::Ink).expect("the band is drawn");
        (last - first + 1) as i64
    };
    let (narrow, wide) = (span(&narrow), span(&wide));

    // The extra width is twice the extra hull width, in pixels: the shell
    // grows on both sides. The camera turns a world unit into about 42 pixels
    // here, and the two widths differ by 0.1, so the band should widen by
    // roughly eight pixels. The bounds below leave room for the silhouette a
    // rasterizer resolves a pixel either way.
    let growth = wide - narrow;
    assert!(
        (4..=14).contains(&growth),
        "doubling the hull width should widen the band by about eight pixels, got {growth} ({narrow} then {wide})"
    );
}

/// The body keeps its own footprint whatever the shell is doing.
///
/// The shell is drawn first and does not write depth, so the body has to win
/// over it wherever the two overlap. If it did not, the spread copy's own
/// interior would be the picture and the body would be invisible.
async fn the_body_covers_the_shell_where_they_overlap() {
    let ctx = Ctx::headless().await;
    let scene = hull(&ctx, SPREAD);
    let frame = scene.frame(&ctx);

    let middle = row(&frame, HEIGHT / 2);
    let centre = middle[WIDTH as usize / 2];
    assert_eq!(
        centre,
        Mark::Body,
        "the body should own the middle of the frame, not the shell"
    );

    // No clear-colour gap inside what was drawn: the ring closes around the
    // body, so scanning across the middle row leaves the band, the body, and
    // the band again.
    let drawn: Vec<usize> = middle
        .iter()
        .enumerate()
        .filter(|(_, mark)| **mark != Mark::Bare)
        .map(|(x, _)| x)
        .collect();
    let (first, last) = (drawn[0], drawn[drawn.len() - 1]);
    let gap = (first..=last).find(|&x| middle[x] == Mark::Bare);
    assert!(
        gap.is_none(),
        "the band and the body should meet without a gap, but column {gap:?} is clear"
    );
    assert!(
        middle[first] == Mark::Ink && middle[last] == Mark::Ink,
        "the row should be banded at both ends, not body on one side"
    );
}

gpu_tests! {
    the_shell_shows_as_a_band_outside_the_body,
    a_wider_hull_draws_a_wider_band,
    the_body_covers_the_shell_where_they_overlap,
}
