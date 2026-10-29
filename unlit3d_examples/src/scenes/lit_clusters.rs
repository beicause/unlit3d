//! The cluster debug scene: the grid a lit frame looks its lights up in.
//!
//! Nothing of the lit scene is reused. What this scene has to show is where the
//! light counts differ from cell to cell, and the lit scene's lights reach nearly
//! every cell, which leaves that map flat: here one row of each light kind
//! crosses a long floor, so each kind's footprint is a band of its own and a
//! light written into the wrong cells moves a patch rather than shifting a level
//! everywhere.

use super::{
    LitSurface, PlacedLight, SceneControl, SceneDef, SceneOptions, allocate_surface,
    camera_looking_at, grid,
};
use unlit_ecs::bundle;
use unlit_wgpu::pipeline::LitShader;
use unlit3d::prelude::*;

/// The frame's cluster grid, composited over the lighting.
///
/// The grid is what a fragment uses to find its lights, and it is built on the
/// CPU from the camera and the target's size, so nothing about it is visible: a
/// cell given the wrong lights, a grid built for another aspect, or a depth
/// slice that does not match the fragment's own lookup all show up as a change
/// of brightness and nothing else. This scene draws the grid itself, so a wrong
/// assignment is a shape in the wrong place rather than a subtle change of
/// level. It is a scene of its own rather than an overlay on the lit scene: the
/// lit scene's lights all reach nearly every cell, which leaves the count map
/// flat, and what a debug view has to show is where the counts differ.
pub static CLUSTER_SCENE: SceneDef = SceneDef {
    id: "lit_clusters",
    title: "Cluster grid",
    description: "a floor receding into the distance with the cluster grid drawn over it",
    size: super::BASELINE_SIZE,
    baseline: Some(super::BASELINE_SIZE),
    clear: None,
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build: build_clusters,
};

/// The depth range the debug scene's grid spans.
///
/// The grid's slices are spaced evenly in `ln(depth)`, so a grid spanning the
/// library default's kilometre spends most of its slices on depths this scene
/// never draws. Spanning the floor instead puts every slice on screen, which is
/// what the view has to show.
const CLUSTER_NEAR: f32 = 1.0;
const CLUSTER_FAR: f32 = 120.0;

/// The debug floor: how wide and how deep, and where its front edge sits.
///
/// It is far larger than the camera can see, so the frame is floor from edge to
/// edge: the camera looks down at more than half the vertical field of view, and
/// a floor that ended inside the frame would put a horizon across it and hide the
/// slices furthest away -- exactly the ones a log-spaced grid packs together.
const CLUSTER_FLOOR: (f32, f32, f32) = (700.0, 600.0, 10.0);

/// Where the debug camera sits and what it looks at.
///
/// High, and tilted down by about 32 degrees: past half the vertical field of
/// view, so the horizon is off the top of the frame, but no steeper, so the far
/// slices still spread across the floor rather than piling onto the last one.
const CLUSTER_EYE: glam::Vec3 = glam::Vec3::new(0.0, 9.5, 9.0);
const CLUSTER_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.0, -6.5);

/// The columns the debug floor's lights stand in.
///
/// A regular lattice, so a cell's count is predictable from where it is: a light
/// reaches a disc of the floor whose radius follows from its height and its
/// range, the discs of one row merge into a band, and the rows are far enough
/// apart in depth that a cell holds the lights of one row and not its
/// neighbour's.
const CLUSTER_COLUMNS: [f32; 5] = [-8.0, -4.0, 0.0, 4.0, 8.0];

/// Where each row of lights sits, nearest first: its depth, the height its lights
/// hover at, and how far each one reaches.
///
/// One row per light kind -- point, spot, then quad -- so every kind's footprint
/// is a band of its own and a kind written into the wrong cells shows up as a
/// patch in the wrong band rather than as a change of level everywhere.
const CLUSTER_ROWS: [(f32, f32, f32); 3] =
    [(-1.0, 1.0, 5.5), (-9.0, 5.0, 12.0), (-18.0, 1.5, 10.0)];

/// Build the cluster debug scene's world content.
///
/// One long floor, a handful of short-ranged lights and a fragment that overlays
/// the grid on the lighting: nothing of the lit scene is reused, because
/// what this scene has to show is where the counts differ, and the lit scene's
/// lights reach nearly every cell.
fn build_clusters(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    source.clustering = ClusterConfig {
        first_slice_depth: CLUSTER_NEAR,
        far: CLUSTER_FAR,
        ..ClusterConfig::DEFAULT
    };
    let device = source.device(&world.root_scope());
    let mut options = super::default_options(&device);
    options.shader = Some(LitShader::new("lit::cluster_grid", "", "", cluster_grid()));
    let key = LitPipelineKey::new(options);

    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);

    // The floor is one plane: the grid is a screen-space structure, so its cells
    // are not aligned with anything the geometry could carry.
    // Wound right along x and up along -z, so the cross product of the two -- the
    // side the front face is on -- is the world's up axis, which is the side the
    // camera looks down at.
    let mut floor = grid(
        glam::Vec3::new(0.0, 0.0, CLUSTER_FLOOR.2 - CLUSTER_FLOOR.1 * 0.5),
        glam::Vec3::X,
        glam::Vec3::NEG_Z,
        (CLUSTER_FLOOR.0, CLUSTER_FLOOR.1),
        (1, 1),
    );
    floor.normals = Some(vec![glam::Vec3::Y.to_array(); floor.positions.len()]);
    let mesh = allocate_surface(world, source_entity, &key, &floor);
    let pipeline = GpuRenderPipeline::new(key);

    let camera_entity = super::spawn_in(
        world,
        source_entity,
        bundle![camera_looking_at(CLUSTER_EYE, CLUSTER_TARGET, size)],
    );
    // A dim ambient keeps the unoccupied cells readable beneath the grid, while
    // the local lights remain visible through their cells.
    super::spawn_in(
        world,
        source_entity,
        bundle![AmbientLight::new(glam::Vec4::new(0.08, 0.08, 0.08, 0.0))],
    );
    super::spawn_in(world, source_entity, LitSurface { mesh, pipeline });

    // One row per light kind, each light aimed straight down at the floor. The
    // color follows the row, so a lit frame of this scene still tells the rows
    // apart beneath the translucent count overlay.
    let hue_color = |hue: f32| {
        glam::Vec4::new(
            0.5 + 0.5 * (hue * std::f32::consts::TAU).cos(),
            0.5 + 0.5 * (hue * std::f32::consts::TAU + 2.094).cos(),
            0.5 + 0.5 * (hue * std::f32::consts::TAU + 4.189).cos(),
            0.0,
        )
    };
    let [
        (point_depth, point_height, point_range),
        (spot_depth, spot_height, spot_range),
        (quad_depth, quad_height, quad_range),
    ] = CLUSTER_ROWS;

    for (column, x) in CLUSTER_COLUMNS.into_iter().enumerate() {
        let color = hue_color(column as f32 / CLUSTER_COLUMNS.len() as f32);
        super::spawn_in(
            world,
            source_entity,
            PlacedLight {
                light: LightSource::new(PointLight {
                    color,
                    intensity: 0.5,
                    range: point_range,
                }),
                translation: glam::Vec3::new(x, point_height, point_depth),
                rotation: glam::Quat::IDENTITY,
            },
        );
    }

    for (column, x) in CLUSTER_COLUMNS.into_iter().enumerate() {
        let color = hue_color(column as f32 / CLUSTER_COLUMNS.len() as f32 + 0.33);
        let position = glam::Vec3::new(x, spot_height, spot_depth);
        super::spawn_in(
            world,
            source_entity,
            PlacedLight::aiming(
                LightSource::new(SpotLight {
                    color,
                    intensity: 1.0,
                    range: spot_range,
                    inner_angle: 18f32.to_radians(),
                    outer_angle: 32f32.to_radians(),
                }),
                position,
                position + glam::Vec3::NEG_Y,
            ),
        );
    }

    for (column, x) in CLUSTER_COLUMNS.into_iter().enumerate() {
        let color = hue_color(column as f32 / CLUSTER_COLUMNS.len() as f32 + 0.66);
        let position = glam::Vec3::new(x, quad_height, quad_depth);
        super::spawn_in(
            world,
            source_entity,
            PlacedLight::aiming(
                LightSource::new(QuadLight::rect(color, 0.6, quad_range, 1.6, 1.2)),
                position,
                position + glam::Vec3::NEG_Y,
            ),
        );
    }

    // A single directional light, which reaches every cell of the grid whatever
    // its depth: the row it lights is the whole frame, so a grid that drops it
    // from any cell shows up as that cell going dark.
    super::spawn_in(
        world,
        source_entity,
        PlacedLight {
            light: LightSource::new(DirectionalLight {
                color: glam::Vec4::new(0.9, 0.85, 0.75, 0.0),
                intensity: 0.2,
            }),
            translation: glam::Vec3::ZERO,
            rotation: glam::Quat::from_rotation_x(-1.0),
        },
    );

    SceneControl {
        advance: Box::new(move |world, _frame, _delta, frame_size| {
            super::aim_camera(
                world,
                source_entity,
                camera_entity,
                CLUSTER_EYE,
                CLUSTER_TARGET,
                frame_size,
            );
        }),
        snapshot: Box::new(move |frame| (frame == 0).then(|| "lit_clusters.webp".to_owned())),
    }
}

/// The WESL the debug scene's fragment stage runs, spliced at the built-in
/// stage's fragment hook.
///
/// The grid is composited over the lit color in the fragment hook: cell fills
/// stay translucent so the original light pools remain visible, while borders
/// and the directional-light mark are nearly opaque. Two things are drawn, each
/// from the arithmetic the lookup itself runs, so a border is exactly where the
/// lookup switches:
///
/// * the **cell fill**: a fixed heat ramp from an empty cell's dark blue to a
///   full cell's red. Every record in a cell is a bounded light now -- the
///   frame's directional lights live in the lighting uniform, not the grid -- so
///   the whole count is what the ramp shows.
/// * the **borders**, black for the cell's xy bounds and subdued white for its
///   depth slice. Both are measured in pixels, so they neither thicken into
///   bands nearby nor disappear in the distance. A magenta line along the left
///   edge marks a frame that carries a directional light, which every fragment
///   sees whatever its cell.
///
/// The cell borders cross the slice borders rather than one hiding the other, so
/// where a cell ends and where a slice ends stay separate readings.
fn cluster_grid() -> String {
    r#"
let grid_depth = view_depth(in.world);
let grid_ndc = fragment_ndc(in.world);
let grid_cell = fragment_cluster_index(grid_ndc, grid_depth);
let grid_local_count = array_access::load_cell_word(grid_cell, 0u);
// Fixed bands make equal colors mean equal counts. Red means the record is
// full and further lights are being dropped.
var grid = vec3<f32>(0.025, 0.035, 0.10);
if grid_local_count > 0u { grid = vec3<f32>(0.04, 0.16, 0.48); }
if grid_local_count > 2u { grid = vec3<f32>(0.02, 0.52, 0.56); }
if grid_local_count > 4u { grid = vec3<f32>(0.12, 0.62, 0.20); }
if grid_local_count > 6u { grid = vec3<f32>(0.82, 0.72, 0.08); }
if grid_local_count > 8u { grid = vec3<f32>(0.95, 0.38, 0.035); }
if grid_local_count >= cluster.dims.w { grid = vec3<f32>(0.92, 0.025, 0.025); }
// The cell borders: the product the lookup floors, so a border is where its
// floor changes. A border is measured in pixels, not in cells -- an edge's
// distance to the nearest border divided by how much the lattice moves per
// pixel -- so it stays the same width wherever the floor's depth changes fastest
// and never thickens into a band.
let grid_lattice = (grid_ndc * 0.5 + 0.5) * vec2<f32>(cluster.dims.xy);
let grid_fract = fract(grid_lattice);
let grid_edge = min(grid_fract, 1.0 - grid_fract);
let grid_pixels = grid_edge / max(fwidth(grid_lattice), vec2<f32>(1e-6));
let grid_border = 1.0 - smoothstep(0.5, 1.5, min(grid_pixels.x, grid_pixels.y));
// The slice borders use the same continuous mapping as the actual lookup.
let grid_slice = cluster_slice_coordinate(grid_depth);
let grid_slice_fract = fract(grid_slice);
let grid_slice_edge = min(grid_slice_fract, 1.0 - grid_slice_fract);
let grid_slice_pixels = grid_slice_edge / max(fwidth(grid_slice), 1e-6);
let grid_slice_border = 1.0 - smoothstep(0.5, 1.5, grid_slice_pixels);
grid = mix(grid, vec3<f32>(0.0), grid_border * 0.9);
grid = mix(grid, vec3<f32>(0.55), grid_slice_border * 0.7);
var grid_opacity = 0.48;
grid_opacity = max(grid_opacity, grid_border * 0.85);
grid_opacity = max(grid_opacity, grid_slice_border * 0.7);
// The frame's directional lights are outside the grid, so this edge mark counts
// them from the uniform instead: one magenta line says the frame carries at
// least one, and no line says it carries none.
if lighting.directionals[0].color_intensity.w > 0.0 && grid_ndc.x < -0.985 {
    grid = vec3<f32>(0.8, 0.08, 0.65);
    grid_opacity = 0.85;
}
color = vec4<f32>(mix(color.rgb, grid, grid_opacity), color.a);
    "#
    .to_owned()
}
