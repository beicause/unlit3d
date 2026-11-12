//! The snapshot comparisons: each stored baseline of the lit cube.

use super::fixture::*;
use super::*;

pub(super) async fn lit_cube_matches_snapshot() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx), 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 0.85, 0.4, 1.0])]);
    assert_image_snapshot(
        snapshot!("lit_cube.webp"),
        &frame,
        frame.width,
        frame.height,
    );
}

/// One draw call over several instances: the per-instance transform and base
/// color must both reach the shader, so every cube lands in its own place with
/// its own color.
pub(super) async fn instanced_cubes_match_snapshot() {
    let ctx = Ctx::headless().await;
    let fixture = fixture(&ctx, &vertex_color_variant(&ctx), 4);

    // A row of cubes at different depths, each with its own base color, all
    // drawn by one instanced draw call.
    let instances: Vec<MeshInstance> = [
        (-0.9, -0.4, [1.0, 0.3, 0.3, 1.0]),
        (0.0, 0.0, [0.3, 1.0, 0.4, 1.0]),
        (0.9, 0.4, [0.4, 0.5, 1.0, 1.0]),
    ]
    .into_iter()
    .map(|(x, z, color)| placed(0.55, 0.6, glam::Vec3::new(x, 0.0, z), color))
    .collect();

    let frame = render(&ctx, &fixture, &instances);

    // The per-instance transform must separate the cubes horizontally, in the
    // order the instances were declared, and each must be tinted by its own
    // base color. Checking this before the snapshot keeps the stored reference
    // from freezing a wrong frame.
    let dominant_x = |channel: usize| {
        let mut weighted = 0.0;
        let mut weight = 0.0;
        for y in 0..frame.height {
            for x in 0..frame.width {
                let p = frame.pixel_u8(x, y);
                let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                let (max, other) = match channel {
                    0 => (r, g.max(b)),
                    1 => (g, r.max(b)),
                    _ => (b, r.max(g)),
                };
                // Weight by how far this pixel leans toward the channel, so
                // the background and the other cubes contribute little.
                let w = (max - other).max(0.0);
                if w > 12.0 {
                    weighted += w * x as f32;
                    weight += w;
                }
            }
        }
        (weight > 0.0).then(|| weighted / weight)
    };

    let red = dominant_x(0).expect("the red cube is drawn");
    let green = dominant_x(1).expect("the green cube is drawn");
    let blue = dominant_x(2).expect("the blue cube is drawn");
    assert!(
        red < green && green < blue,
        "each instance must land where its transform puts it: \
         red x={red:.1}, green x={green:.1}, blue x={blue:.1}"
    );

    assert_image_snapshot_with_tolerance(
        snapshot!("lit_cubes_instanced.webp"),
        &frame,
        frame.width,
        frame.height,
        CUBE_TOLERANCE,
    );
}

/// Without a position stream the geometry is a single point at each instance
/// origin, so a position-less draw needs no position buffer and still lands
/// where the per-instance transform puts it.
pub(super) async fn position_less_variant_draws_points_at_instance_origins() {
    let ctx = Ctx::headless().await;
    let variant = variant(
        &ctx,
        LitOptions::standard(),
        LitVertexChannels {
            position: PositionStreamChannels {
                position: None,
                joints: false,
            },
            secondary: SecondaryFlags::empty(),
        },
        false,
        false,
    );
    let fixture = fixture(&ctx, &variant, 1);
    assert!(
        fixture.mesh.positions.is_none(),
        "a position-less variant must not upload a position buffer"
    );

    // Two instances at different places. A position-less variant draws the
    // unindexed point range from the fixture's vertex count.
    let origin = placed(1.0, 0.0, glam::Vec3::ZERO, [1.0, 0.0, 0.0, 1.0]);
    let right = placed(
        1.0,
        0.0,
        glam::Vec3::new(1.0, 0.0, 0.0),
        [0.0, 1.0, 0.0, 1.0],
    );
    let frame = render(&ctx, &fixture, &[origin, right]);

    // Count lit pixels on each side of the centre column: the points must
    // land apart, which only holds if the position came from the instance
    // transform rather than a vertex buffer.
    let mut left = 0;
    let mut right_count = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let p = frame.pixel_u8(x, y);
            let lit = p[..3]
                .iter()
                .zip(CLEAR)
                .any(|(&c, bg)| (c as f64 / 255.0 - bg).abs() * 255.0 > 12.0);
            if lit {
                if x < WIDTH / 2 {
                    left += 1;
                } else {
                    right_count += 1;
                }
            }
        }
    }
    assert!(
        left > 0 && right_count > 0,
        "the two instances should land on opposite sides: {left} left, {right_count} right"
    );
}

/// A caller's module replaces the built-in entry shader, while the variant's
/// layouts and streams stay exactly what the built-in shader would have used:
/// the same bind group, the same vertex buffers, one draw.
pub(super) async fn a_caller_module_replaces_the_built_in_stages() {
    let ctx = Ctx::headless().await;
    // The module keeps the built-in vertex stage and replaces only the fragment:
    // the geometry arrives through the variant's own attributes, leaves the
    // vertex stage exactly where the built-in shader puts it, and is painted
    // over by a constant colour.
    let shader = Shader::new(
        "test::flat",
        r#"
        import package::lit_fragment;
        import package::lit_vertex;

        @vertex
        public fn vs_main(in: lit_vertex::VertexInput, @builtin(vertex_index) vertex_index: u32) -> lit_vertex::VertexOut {
            return lit_vertex::vs_stage(in, vertex_index);
        }

        @fragment
        public fn fs_main(in: lit_vertex::VertexOut) -> @location(0) vec4<f32> {
            return vec4<f32>(1.0, 0.0, 1.0, 1.0);
        }
        "#,
    );
    let mut options = LitOptions::standard();
    options.shader = Some(shader);
    let variant = variant(
        &ctx,
        options,
        LitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::UncompressedPosition),
                joints: false,
            },
            secondary: SecondaryFlags::empty(),
        },
        false,
        false,
    );
    let fixture = fixture(&ctx, &variant, 4);
    let frame = render(&ctx, &fixture, &[placed_cube([1.0, 1.0, 1.0, 1.0])]);

    // The module paints its own color, and it covers the same pixels the
    // built-in shader would have: the geometry reaches it through the very
    // attributes the built-in variant declares.
    let painted = frame
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] > 200)
        .count();
    let total = (WIDTH * HEIGHT) as usize;
    assert!(
        painted > total / 8,
        "the caller's fragment should paint the cube, got {painted}/{total} pixels"
    );
}

pub(super) async fn textured_cube_matches_snapshot() {
    let ctx = Ctx::headless().await;
    let variant = variant(
        &ctx,
        LitOptions::standard(),
        LitVertexChannels {
            position: PositionStreamChannels {
                position: Some(ChannelEncoding::CompressedPosition),
                joints: false,
            },
            secondary: SecondaryFlags::UV | SecondaryFlags::COLOR,
        },
        true,
        false,
    );
    let fixture = fixture(&ctx, &variant, 4);
    let instance = placed(0.8, 0.6, glam::Vec3::ZERO, [1.0, 1.0, 1.0, 1.0]);
    let frame = render(&ctx, &fixture, &[instance]);
    assert_image_snapshot_with_tolerance(
        snapshot!("lit_cube_textured.webp"),
        &frame,
        frame.width,
        frame.height,
        CUBE_TOLERANCE,
    );
}
