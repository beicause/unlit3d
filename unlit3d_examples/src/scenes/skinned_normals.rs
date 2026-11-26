//! The lit-skinned-cube scene: a cube whose normals follow its rotating and
//! mirrored pose joints.
//!
//! A cube carried by two joints is drawn under a side light across six frames.
//! Each face carries a flat, distinct colour, so the face looking at the
//! camera is obvious at a glance instead of being an abstract gradient.
//! Unlike the unlit skinned scene, the mesh carries per-vertex normals and
//! the material is lit, so each frame is a check that the shader transforms a
//! skinned normal by the same pose joints that deform its position. The mesh
//! entity stays at rest; only the joints place the geometry, so a shader that
//! read the normal from the entity would leave it pointing the wrong way and
//! shade a different picture.
//!
//! The pose turns a sixth of a revolution each frame and its x scale sweeps
//! from +1 through -1 back to +1, so the six frames form one full loop:
//! each face takes its turn towards the camera and the side light, making the
//! shading change easy to see. The determinant changes sign partway through,
//! and the frames with a negative scale check that winding follows the pose:
//! a mirrored skin whose front faces were still chosen from the entity's
//! identity transform would cull every front face and draw nothing.

use super::{
    BendSkin, PlacedLight, SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_SIZE,
    aim_camera, camera_looking_at, cube_with_normals, default_options, with_mesh_source,
};
use unlit_ecs::bundle;
use unlit3d::prelude::*;

const FRAMES: usize = 6;

/// The scene: a lit cube bent and mirrored by its skin, one frame each.
pub static SCENE: SceneDef = SceneDef {
    id: "skinned_normals",
    title: "Lit skinned normals",
    description: "a lit cube bent and mirrored by its skin across six frames",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: None,
    frames: FRAMES as u32,
    step_seconds: Some(SEQUENCE_STEP),
    samples: 1,
    depth: true,
    egui: false,
    reproducible_egui: false,
    build,
};

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
    let mut options = default_options();
    options.unlit = false;
    let key = LitPipelineKey::new(options);
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(SourceOrder::MESH, source)]);

    let ((positions, uvs, _gradient, indices), normals) = cube_with_normals();
    // A distinct flat colour per face: the face turned toward the camera is
    // then obvious at a glance. With the winding bug present, a mirrored pose
    // culls every front face and the frame goes empty rather than shifting to
    // a subtly different shade.
    const FACE_COLORS: [[u8; 4]; 6] = [
        [235, 55, 55, 255],  // -X
        [55, 205, 75, 255],  // +X
        [55, 110, 235, 255], // -Y
        [235, 210, 55, 255], // +Y
        [220, 55, 205, 255], // -Z, the back face
        [55, 220, 225, 255], // +Z, the face that looks at the camera
    ];
    let colors: Vec<[u8; 4]> = (0..positions.len()).map(|i| FACE_COLORS[i / 4]).collect();
    let skin = BendSkin::new(&positions, 0.0);
    let mesh = with_mesh_source(world, source_entity, |source, world| {
        source.allocate_lit_mesh(
            world,
            LitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                normals: Some(&normals),
                indices: Some(&indices),
                joints: Some(&skin.joints),
                weights: Some(&skin.weights),
                morph_deltas: None,
            },
        )
    });

    let camera_entity = world.scope_mut(source_entity).spawn(bundle![camera(size)]);
    let pose_entity = world.scope_mut(source_entity).spawn(bundle![skin.pose()]);
    world.scope_mut(source_entity).spawn(LitMeshBundle {
        transform: Transform::default(),
        primitive: LitMeshPrimitive::new(mesh, GpuRenderPipeline::new(key)),
        skin: Some(SkinBinding { pose: pose_entity }),
        morph: None,
    });

    world
        .scope_mut(source_entity)
        .spawn(bundle![AmbientLight::new(glam::Vec4::new(
            0.02, 0.02, 0.03, 0.0
        ))]);
    world.scope_mut(source_entity).spawn(PlacedLight::aiming(
        LightSource::new(DirectionalLight {
            color: glam::Vec4::new(1.0, 0.85, 0.6, 0.0),
            intensity: 1.6,
        }),
        glam::Vec3::new(2.0, 2.5, 2.0),
        glam::Vec3::ZERO,
    ));

    let mut skin = skin;
    SceneControl {
        advance: Box::new(move |world, frame, _delta, frame_size| {
            let frame = frame as usize % FRAMES;
            let mesh_world = frame_state(frame);
            skin.set_angle(0.0);
            *world
                .scope(source_entity)
                .query_one::<&mut SkinPose>(pose_entity)
                .get() = skin.pose_at(mesh_world);
            aim_camera(
                world,
                source_entity,
                camera_entity,
                CAMERA_EYE,
                glam::Vec3::ZERO,
                frame_size,
            );
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES as u32).then(|| format!("skinned_normals/frame_{frame:02}.webp"))
        }),
    }
}

const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 3.4);

fn camera(size: (u32, u32)) -> Camera {
    camera_looking_at(CAMERA_EYE, glam::Vec3::ZERO, size)
}

fn frame_state(frame: usize) -> glam::Mat4 {
    // One full turn over the six frames, so a seventh matches the first.
    let phase = std::f32::consts::TAU * frame as f32 / FRAMES as f32;
    // A sixth of a revolution per frame lets each coloured face take its turn
    // towards the camera and the fixed side light, so the shading visibly
    // changes from frame to frame.
    let turn = glam::Mat4::from_rotation_y(phase);
    // Scaling x alone sweeps +1 -> -1 -> +1 at a constant angular speed, so
    // the skin mirrors partway through and its determinant changes sign. An
    // x mirror, unlike a uniform one, swaps left and right faces instead of
    // reproducing the same picture a half turn would. Cosine never lands on a
    // sampled frame, so no frame collapses to a line.
    let scale = phase.cos();
    glam::Mat4::from_scale(glam::Vec3::new(scale, 1.0, 1.0)) * turn
}
