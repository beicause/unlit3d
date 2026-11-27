//! The CSG scene: one block, bored, merged and clipped by the same sphere.
//!
//! Procedural geometry rarely needs a boolean for its own sake — a crate, a
//! pillar or a pipe is a function from a parameter to vertices, and writing
//! that function is cheaper than building a solid and cutting it. A boolean
//! earns its place when a *cut* has to close: a bore through a block, a notch
//! out of a bracket, the usable volume where two shapes overlap. Those surfaces
//! are what this scene draws.
//!
//! Three copies of the same pair of solids stand side by side, each combined
//! with one of the operations: difference, union and intersection. Reading left
//! to right, the block is drilled through, the drill is merged into it, and
//! only the part of the drill inside the block is kept.
//!
//! The drill is a cylinder longer than the block is deep, standing along Z, so
//! the difference leaves a hole that opens on the face the camera looks at.
//! That is what makes the cut visible at all — a bore entirely inside the block
//! would leave an enclosed cavity, and a closed solid shows nothing of its
//! inside. The union's drill sticks out toward the camera for the same reason,
//! and the intersection is the short plug the block keeps of it.
//!
//! The scene is lit rather than drawn in flat tints. A boolean's evidence is the
//! shading of the surface it created: the inside of a bore is a cylinder the
//! light falls across, and it reads as a hole only because its walls are darker
//! than the face around it.
//!
//! No normals are uploaded, so the lit pipeline derives one geometric normal per
//! triangle from screen-space derivatives. That is a hard edge at every face
//! without a single extra vertex, and it keeps the block's corners and the
//! bore's rim as crisp as the boolean states them; smoothed normals would round
//! off exactly the creases worth seeing.

use super::{PlacedLight, SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_looking_at};
use unlit_ecs::bundle;
use unlit_wgpu::pipeline::LitOptions;
use unlit3d::manifold::manifold_rust::linalg::Vec3;
use unlit3d::manifold::manifold_rust::manifold::Manifold;
use unlit3d::manifold::{BooleanOp, boolean, streams};
use unlit3d::prelude::*;

/// The block's extent, in world units: wide enough that the bore's wall is a
/// band rather than a line, and shallow enough that the drill shows at both
/// ends.
const BLOCK: glam::Vec3 = glam::Vec3::new(2.4, 1.6, 2.0);

/// The drill's radius and length, in world units. It is longer than the block
/// is deep, so it opens the block's front and back faces rather than leaving a
/// cavity inside it.
const DRILL_RADIUS: f64 = 0.5;
const DRILL_LENGTH: f64 = 2.9;

/// How far apart the three results stand, in world units.
const COLUMN_SPACING: f32 = 3.4;

/// Where the camera sits, in world units.
///
/// Above the row and in front of it, so the top faces and the bores through the
/// block both show; a camera level with the blocks would see the first two
/// results as silhouettes only.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(1.4, 2.2, 8.4);

/// Where the row is aimed: slightly below the blocks, so the camera looks down
/// on their top faces and into the bores without flattening them.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, -0.15, 0.0);

/// The three results, left to right, with the tint each draws in.
///
/// The tints separate the three at a glance without a material, since the
/// unlit pipeline draws an instance's base colour unchanged.
const COLUMNS: [(BooleanOp, glam::Vec4); 3] = [
    (
        BooleanOp::Difference,
        glam::Vec4::new(0.86, 0.36, 0.30, 1.0),
    ),
    (BooleanOp::Union, glam::Vec4::new(0.34, 0.66, 0.42, 1.0)),
    (
        BooleanOp::Intersection,
        glam::Vec4::new(0.36, 0.54, 0.88, 1.0),
    ),
];

/// The scene: the same pair of solids under each of the three operations.
pub static SCENE: SceneDef = SceneDef {
    id: "csg_boolean",
    title: "CSG booleans",
    description: "boolean difference, union and intersection of procedural solids",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: None,
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    egui: false,
    reproducible_egui: false,
    build,
};

/// Build the CSG scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(SourceOrder::MESH, source)]);

    // The lit pipeline's own defaults, rather than the unlit options the other
    // ECS scenes state: a boolean's cut surface is only legible when the light
    // falls across it.
    let key = LitPipelineKey::new(LitOptions::standard());
    let camera_entity = world
        .scope_mut(source_entity)
        .spawn(bundle![camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size)]);

    // Ambient light lifts the faces the key light does not reach, so the bore's
    // far wall is dark rather than black.
    world
        .scope_mut(source_entity)
        .spawn(bundle![AmbientLight::new(glam::Vec4::new(
            0.28, 0.30, 0.36, 0.0
        ))]);
    // One key light from above and to the left, which is the direction that
    // separates the block's top face from its front one.
    world.scope_mut(source_entity).spawn(PlacedLight {
        light: LightSource::new(DirectionalLight {
            color: glam::Vec4::new(1.0, 0.96, 0.88, 0.0),
            intensity: 1.0,
        }),
        translation: glam::Vec3::ZERO,
        rotation: glam::Quat::from_rotation_y(-0.7) * glam::Quat::from_rotation_x(-0.85),
    });

    let block = Manifold::cube(
        Vec3::new(f64::from(BLOCK.x), f64::from(BLOCK.y), f64::from(BLOCK.z)),
        true,
    );
    // A cylinder stands along Z, which is the axis the camera looks down: the
    // bore therefore opens on the face the scene shows.
    let drill = Manifold::cylinder(DRILL_LENGTH, DRILL_RADIUS, DRILL_RADIUS, 64);

    for (column, &(op, tint)) in COLUMNS.iter().enumerate() {
        let solid = boolean(&block, &drill, op).expect("the two solids meet for every operation");
        let mut mesh = streams(&solid).expect("a boolean of two solids is a solid");
        let gpu_mesh = super::with_mesh_source(world, source_entity, |source, world| {
            source.allocate_lit_mesh(world, mesh.borrowed())
        });
        let offset = (column as f32 - 1.0) * COLUMN_SPACING;
        world.scope_mut(source_entity).spawn(LitMeshBundle {
            transform: Transform {
                translation: glam::Vec3::new(offset, 0.0, 0.0),
                ..Transform::default()
            },
            primitive: LitMeshPrimitive {
                color: InstanceColor::new(tint),
                ..LitMeshPrimitive::new(gpu_mesh, GpuRenderPipeline::new(key.clone()))
            },
            skin: None,
            morph: None,
        });
    }

    SceneControl {
        // Static: nothing animates, but the camera keeps the frame's aspect so
        // a resized window does not stretch the row.
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
        snapshot: Box::new(|frame| (frame == 0).then(|| "csg_boolean.webp".to_owned())),
    }
}
