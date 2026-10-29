//! The lit scene: Lambert shading under every kind of built-in light.
//!
//! One large surface faces the camera, and each of the four light types lays a
//! shape on it, so a frame that drops a light, packs one wrong or clusters one
//! out of its cells changes the pixels rather than only the brightness:
//!
//! * the two **directional** lights are constant over the whole scene and own no
//!   shape of their own: one rakes the floor from above and one faces the wall,
//!   so together they give every surface a gradient without competing with the
//!   bounded lights' patches;
//! * the **point** lights are the three small, saturated sources floating where
//!   their distance to the wall equals their height above the floor, so each
//!   pool runs up the wall and across the floor at the same reach, and the three
//!   pools overlap just enough to read as one band of mixed colour;
//! * the **spot** light stands at the far left, level with the floor, pointing
//!   at the wall: its cone cuts a half disc on the wall with the straight edge on
//!   the floor, and a cone packed with the wrong outer angle would light far more
//!   of the wall than a half disc;
//! * the **quad** lights are the two area sources: a violet rectangle standing
//!   upright right where it meets the floor, and a teal triangle standing on its
//!   base just above the floor to its left. They are the lights whose shading is
//!   not a point term, so a form factor that is zero, negative or far too bright
//!   shows up as a missing or blown-out patch -- and the pane's outline lands on
//!   the wall as a triangle, which a form factor that integrates the wrong
//!   corners cannot match.
//!
//! Each light owns its own corner of the frame, so the patches stay apart and
//! every shape reads on its own.
//!
//! Every light has a colour no other has -- gold and blue suns, red/green/blue
//! points, an amber spot, a violet quad -- so a light that is packed, clustered
//! or shaded wrongly changes a hue rather than only a brightness.
//!
//! The wall is a relief rather than a plane, displaced out of its own plane by a
//! vertex snippet the scene splices into the built-in lit stage — the same hook a
//! caller uses to shape its own geometry. Being displaced, it is drawn from a
//! mesh with no normal channel, so its fragments take the normal the displaced
//! surface actually has from the world positions across the primitive; the floor
//! in front of it is a plain plane and keeps its own flat normals, so one frame
//! draws both normal sources.

use super::{
    LitSurface, PlacedLight, SceneControl, SceneDef, SceneOptions, allocate_surface,
    camera_looking_at, grid,
};
use unlit_ecs::bundle;
use unlit_wgpu::pipeline::LitShader;
use unlit3d::prelude::*;

/// How far above the floor, and how far back, the camera sits.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.0, 2.05, 1.85);

/// The point the camera looks at: the middle of the floor, where the quad
/// light hangs.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.9, -3.5);

/// The light every fragment gets whether any light reaches it, tinted cold so
/// the parts no light reaches read as shaded rather than as black.
const AMBIENT: glam::Vec4 = glam::Vec4::new(0.012, 0.014, 0.02, 0.0);

/// The floor: its depth along z, its width along x, and how far it reaches in
/// front of the camera. The back edge is placed at the wall, so shortening the
/// floor crops empty foreground rather than opening a gap at the wall.
const FLOOR: (f32, f32, f32) = (5.2, 11.5, 1.0);

/// How many quads the wall is split into along x and along y. The relief is a
/// vertex displacement, so this is the resolution it is sampled at.
const WALL_SEGMENTS: (u32, u32) = (1280, 400);

/// The wall's relief: how far the noise stands out of it, and how large one cell
/// of its lattice is along x and along y. A noise octave has no direction of its
/// own, so unlike a wave train it reads the same however the frame is cropped.
const RELIEF: (f32, f32, f32) = (0.012, 0.055, 0.045);

/// The back wall: how wide, how tall, and how far behind the origin. It stands
/// behind the floor so the key light has a second surface at a right angle to
/// the floor, which is what shows a normal stream that is right for one face
/// and wrong for the other.
const WALL: (f32, f32, f32) = (11.5, 3.6, -4.2);

/// The three point lights, as x position and colour.
const POINT_LIGHTS: [(f32, glam::Vec4); 3] = [
    (0.15, glam::Vec4::new(1.0, 0.08, 0.05, 0.0)),
    (0.8, glam::Vec4::new(0.1, 1.0, 0.1, 0.0)),
    (1.45, glam::Vec4::new(0.1, 0.2, 1.0, 0.0)),
];

/// How far above the floor the point lights float, and how far in front of the
/// origin. They float near the back wall so each pool runs up it as well.
const POINT_HEIGHT: f32 = 0.5;
const POINT_DEPTH: f32 = WALL.2 + 0.55;

/// The reach of each point light: far enough to pool on the floor beneath it
/// and to climb the wall behind it, short enough that the pools stay apart.
const POINT_RANGE: f32 = 1.9;

/// The intensity of each point light, in the units the shader's falloff uses.
const POINT_INTENSITY: f32 = 0.24;

/// The spot light that hangs in front of the wall: where it hangs, and its
/// colour.
const OVERHEAD_SPOT: (glam::Vec3, glam::Vec4) = (
    glam::Vec3::new(-3.05, 0.12, -1.5),
    glam::Vec4::new(1.0, 0.55, 0.12, 0.0),
);

/// What that spot aims at: high on the wall, so the cone splashes across the
/// wall and reaches down onto the floor in front of it. It hangs off to the
/// right, clear of the standing pane's patch.
const OVERHEAD_TARGET: glam::Vec3 = glam::Vec3::new(-3.05, 0.12, -4.2);

/// The cone's inner and outer half-angles in degrees, and how bright the spot
/// is.
const SPOT_INNER: f32 = 26.0;
const SPOT_OUTER: f32 = 38.0;
const SPOT_INTENSITY: f32 = 5.0;

/// The flat pane over the front of the floor: its size, how far up it hangs,
/// how far in front of the origin, and how bright it is.
///
/// It hangs just above the crests of the relief, because a polygon light is an
/// area source: the closer it sits to what it lights, the sharper the edge of
/// its patch, and lifted high it would only wash the whole floor.
const QUAD_SIZE: (f32, f32) = (1.25, 1.0);
const QUAD_HEIGHT: f32 = 0.5;
const QUAD_X: f32 = 3.0;
const QUAD_DEPTH: f32 = -3.85;
const QUAD_INTENSITY: f32 = 0.4;
const QUAD_COLOR: glam::Vec4 = glam::Vec4::new(0.75, 0.35, 1.0, 0.0);

/// A second polygon light, standing upright instead of lying flat, and shaped
/// as a triangle: a polygon light is any convex outline, and three corners are
/// enough to show it. The frame merges the fourth corner onto the third.
const TRIANGLE_COLOR: glam::Vec4 = glam::Vec4::new(0.2, 1.0, 0.75, 0.0);
const TRIANGLE_INTENSITY: f32 = 0.3;
/// Where the pane's base edge floats: a little in front of the wall and a
/// little above the floor, so the outline it throws stays compact the way the
/// flat pane's does.
const TRIANGLE_FOOT: glam::Vec3 = glam::Vec3::new(-1.45, 0.2, -3.85);
/// How far the standing pane tips back from upright.
///
/// A pane square to the wall shines across it and nothing else, because the
/// floor lies behind its radiating face. Tipping it back turns the ray down, so
/// the same polygon throws its outline on the wall and rakes the floor in front
/// of it.
const TRIANGLE_TILT_DEGREES: f32 = 12.0;
/// The triangle's corners, in its own plane: a wide base and one apex, so the
/// patch it throws reads as a triangle and not as a band.
const TRIANGLE_VERTICES: [glam::Vec3; 4] = [
    glam::Vec3::new(-0.85, -0.65, 0.0),
    glam::Vec3::new(0.85, -0.65, 0.0),
    glam::Vec3::new(0.0, 0.95, 0.0),
    glam::Vec3::new(0.0, 0.95, 0.0),
];

/// The scene: one relief floor and one wall, lit by one light of every kind.
pub static SCENE: SceneDef = SceneDef {
    id: "lit",
    title: "Lambert lighting",
    description: "one relief surface under directional, point, spot and quad lights",
    size: super::BASELINE_SIZE,
    baseline: Some(super::BASELINE_SIZE),
    clear: None,
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};
/// Build the lit scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    // The lit family, rather than the unlit one: it is the same pipeline with
    // the variant's lighting flag set, so the mesh and material allocation
    // below is the unlit pipeline's own.
    source.register_lit_family(&world.root_scope());
    let device = source.device(&world.root_scope());
    // The default options draw unlit; this scene is the one that lights.
    let mut options = super::default_options(&device);
    options.unlit = false;
    let key = LitPipelineKey::new(options.clone());
    // The wall's own key: the relief rides in the vertex stage, and a shader
    // is part of the options a variant is compiled from, so the wall draws
    // through a second pipeline of the same family.
    let mut relief = options;
    relief.shader = Some(LitShader::new("lit::wall_relief", "", wall_relief(), ""));
    let relief_key = LitPipelineKey::new(relief);

    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);

    let mut floor = grid(
        glam::Vec3::new(0.0, 0.0, FLOOR.2 - FLOOR.0 * 0.5),
        glam::Vec3::Z,
        glam::Vec3::X,
        (FLOOR.0, FLOOR.1),
        (1, 1),
    );
    let wall = grid(
        glam::Vec3::new(0.0, WALL.1 * 0.5, WALL.2),
        glam::Vec3::X,
        glam::Vec3::Y,
        (WALL.0, WALL.1),
        WALL_SEGMENTS,
    );
    // A surface with no normal channel is shaded by the normal its displaced
    // positions give it, which is what the relief needs; the floor is a plane,
    // so the mesh's own normal is exactly right for it.
    floor.normals = Some(vec![glam::Vec3::Y.to_array(); floor.positions.len()]);
    let floor_mesh = allocate_surface(world, source_entity, &key, &floor);
    let wall_mesh = allocate_surface(world, source_entity, &relief_key, &wall);
    let pipeline = GpuRenderPipeline::new(key);
    let relief_pipeline = GpuRenderPipeline::new(relief_key);

    let camera_entity = super::spawn_in(
        world,
        source_entity,
        bundle![camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size)],
    );
    super::spawn_in(world, source_entity, bundle![AmbientLight::new(AMBIENT)]);

    super::spawn_in(
        world,
        source_entity,
        LitSurface {
            mesh: floor_mesh,
            pipeline,
        },
    );
    super::spawn_in(
        world,
        source_entity,
        LitSurface {
            mesh: wall_mesh,
            pipeline: relief_pipeline,
        },
    );

    // Two directional lights, told apart by hue as well as by direction: the
    // amber key rakes across the floor from above and the blue fill faces the
    // wall head-on. They are constant over the whole scene, so neither hides the
    // bounded lights' shapes.
    for (color, intensity, rotation) in [
        (
            glam::Vec4::new(1.0, 0.62, 0.15, 0.0),
            0.22,
            glam::Quat::from_rotation_y(0.65) * glam::Quat::from_rotation_x(-0.65),
        ),
        (
            glam::Vec4::new(0.2, 0.45, 1.0, 0.0),
            0.11,
            glam::Quat::IDENTITY,
        ),
    ] {
        super::spawn_in(
            world,
            source_entity,
            PlacedLight {
                light: LightSource::new(DirectionalLight { color, intensity }),
                translation: glam::Vec3::ZERO,
                rotation,
            },
        );
    }

    for (x, color) in POINT_LIGHTS {
        super::spawn_in(
            world,
            source_entity,
            PlacedLight {
                light: LightSource::new(PointLight {
                    color,
                    intensity: POINT_INTENSITY,
                    range: POINT_RANGE,
                }),
                translation: glam::Vec3::new(x, POINT_HEIGHT, POINT_DEPTH),
                rotation: glam::Quat::IDENTITY,
            },
        );
    }

    // The overhead spot aims at the wall, so its cone shows on both surfaces at
    // once: the wall takes the middle of it and the floor takes the spill.
    super::spawn_in(
        world,
        source_entity,
        PlacedLight::aiming(
            LightSource::new(SpotLight {
                color: OVERHEAD_SPOT.1,
                intensity: SPOT_INTENSITY,
                range: 14.0,
                inner_angle: SPOT_INNER.to_radians(),
                outer_angle: SPOT_OUTER.to_radians(),
            }),
            OVERHEAD_SPOT.0,
            OVERHEAD_TARGET,
        ),
    );

    // The rectangle stands upright and faces the right of the wall. Its
    // distance gives it a soft edge, contrasting with the closer triangle.
    super::spawn_in(
        world,
        source_entity,
        PlacedLight::aiming(
            LightSource::new(QuadLight::rect(
                QUAD_COLOR,
                QUAD_INTENSITY,
                8.0,
                QUAD_SIZE.0,
                QUAD_SIZE.1,
            )),
            glam::Vec3::new(QUAD_X, QUAD_HEIGHT, QUAD_DEPTH),
            glam::Vec3::new(QUAD_X, QUAD_HEIGHT, WALL.2),
        ),
    );

    // The triangle stands on the left, aiming mostly at the wall but tipped
    // down enough for its lower part to spill onto the floor.
    let tilt = TRIANGLE_TILT_DEGREES.to_radians();
    let upright = glam::Vec3::new(0.0, tilt.cos(), -tilt.sin());
    let triangle_center = TRIANGLE_FOOT + upright * TRIANGLE_VERTICES[0].y.abs();
    super::spawn_in(
        world,
        source_entity,
        PlacedLight::aiming(
            LightSource::new(QuadLight {
                color: TRIANGLE_COLOR,
                intensity: TRIANGLE_INTENSITY,
                range: 16.0,
                vertices: TRIANGLE_VERTICES,
            }),
            triangle_center,
            triangle_center + glam::Vec3::new(0.0, -tilt.sin(), -tilt.cos()),
        ),
    );

    SceneControl {
        // A single frame: the lights do not move, so only the camera's aspect
        // is advanced, keeping a resized window from stretching the floor.
        advance: Box::new(move |world, _frame, _delta, frame_size| {
            super::aim_camera(
                world,
                source_entity,
                camera_entity,
                CAMERA_EYE,
                CAMERA_TARGET,
                frame_size,
            );
        }),
        snapshot: Box::new(move |frame| (frame == 0).then(|| "lit.webp".to_owned())),
    }
}
/// The WESL the wall's vertex stage runs, spliced at the built-in stage's
/// vertex hook.
///
/// `world` is the vertex the instance transform placed, and everything after
/// the hook — the clip position, the world position the lights are addressed
/// from and the cluster cell — follows the vertex this leaves. The relief fades
/// out at the wall's borders, so the surface still meets the floor along its
/// bottom edge and still fills the frame.
///
/// The noise runs out of the wall along its own normal rather than along the
/// world's up axis, which is what makes the wall a surface the lights cross
/// rather than a backdrop they paint on: a fragment's normal comes from the
/// displaced positions, so every rise and hollow shades by its own slope.
///
/// The snippet runs per vertex, so the segments the wall is split into are the
/// resolution the noise is sampled at; one lattice cell spans a few segments,
/// which is what keeps a vertex displacement from turning the noise into flat
/// facets.
fn wall_relief() -> String {
    let (amplitude, cell_x, cell_y) = RELIEF;
    let half_width = WALL.0 * 0.5;
    let half_height = WALL.1 * 0.5;
    format!(
        "\
let lattice = vec2<f32>(world.x / {cell_x:.4}, world.y / {cell_y:.4});
let cell = floor(lattice);
let offset = fract(lattice);
let weighted = (3.0 - 2.0 * offset) * offset * offset;
// An integer hash, not `fract(sin(...))`: `sin` is only specified to a few
// units in the last place, and the usual trick of multiplying it by a large
// constant turns that into an unrelated lattice on another driver. Integer
// arithmetic is exact, so every backend displaces the wall the same way.
var corners = vec4<f32>(0.0);
for (var corner = 0u; corner < 4u; corner += 1u) {{
    let at = vec2<u32>(vec2<i32>(cell + vec2<f32>(f32(corner % 2u), f32(corner / 2u))));
    var hash = at.x * 0x27d4eb2du + at.y * 0x165667b1u;
    hash = (hash ^ (hash >> 15u)) * 0x2545f491u;
    hash = hash ^ (hash >> 13u);
    corners[corner] = f32(hash) * 2.3283064e-10;
}}
let top = mix(corners.x, corners.y, weighted.x);
let bottom = mix(corners.z, corners.w, weighted.x);
let rise = mix(top, bottom, weighted.y) * 2.0 - 1.0;
let fade = (1.0 - smoothstep({x0:.4}, {x1:.4}, abs(world.x)))
         * (1.0 - smoothstep({y0:.4}, {y1:.4}, abs(world.y - {center_height:.4})));
world = world + vec3<f32>(0.0, 0.0, {amplitude:.4} * rise * fade);
",
        x0 = half_width * 0.80,
        x1 = half_width * 0.99,
        y0 = half_height * 0.72,
        y1 = half_height * 0.99,
        center_height = half_height,
    )
}
