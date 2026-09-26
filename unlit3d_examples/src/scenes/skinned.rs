//! The skinned-cube scene: a mesh bent by two joints across six frames.
//!
//! Ported from the `ecs_skinned_cube_matches_snapshots` test. A skinned cube
//! is drawn over several frames while its pose animates: the joint matrices
//! live in a [`SkinPose`] component the CPU updates, and each frame is stored
//! as its own snapshot.

use super::{
    BendSkin, SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_SIZE, deformation_options,
};
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// Where the camera sits, close enough that a deformation moves many pixels.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.9, 1.1, 2.6);

/// The scene: a cube bending frame by frame, each stored as its own snapshot.
pub static SCENE: SceneDef = SceneDef {
    id: "ecs_skinned",
    title: "Skinned cube",
    description: "a cube bent by a two-joint skin over six frames",
    size: TEST_SIZE,
    frames: FRAMES as u32,
    step_seconds: Some(SEQUENCE_STEP),
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// Build the skinned-cube scene's world content.
fn build(
    world: &mut LocalWorld,
    context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    // The variant that reads the joint stream: its position buffer is wider by
    // the joint pair, so a stream packed for the plain cube would misread.
    let key = UnlitPipelineKey::new(deformation_options(&source.device(world), true, false));
    let source_entity = spawn_source(world, source);

    let (positions, uvs, colors, indices) = super::cube();
    let skin = BendSkin::new(&positions, bend_angle(0));
    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_unlit_mesh(
            world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                joints: Some(&skin.joints),
                weights: Some(&skin.weights),
                morph_targets: &[],
            },
        )
    });

    world.spawn((camera(),));
    // The pose is an entity of its own: the mesh names it rather than owning
    // it, so animating the skeleton is one component write and nothing on the
    // mesh changes.
    let pose_entity = world.spawn((skin.pose(),));
    world.spawn((
        Transform {
            scale: glam::Vec3::splat(0.9),
            ..Default::default()
        },
        InstanceColor::new(glam::Vec4::new(1.0, 0.85, 0.6, 1.0)),
        mesh,
        UnlitPipeline::new(key),
        SkinBinding::new(pose_entity),
    ));

    let mut skin = skin;
    SceneControl {
        advance: Box::new(move |world, frame, _delta| {
            let frame = frame as usize % FRAMES;
            // The pose is a component write, not a mesh re-upload and not a
            // GPU call: the source packs and uploads it once per frame.
            skin.set_angle(bend_angle(frame));
            *world
                .get_mut::<SkinPose>(pose_entity)
                .expect("the pose entity carries a `SkinPose`") = skin.pose();
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES as u32).then(|| format!("ecs_skinned/frame_{frame:02}.webp"))
        }),
    }
}

/// The camera the whole sequence shares, so a frame's difference is the pose's.
fn camera() -> Camera {
    super::camera_looking_at(
        CAMERA_EYE,
        glam::Vec3::new(0.0, 0.2, 0.0),
        TEST_SIZE.0 as f32 / TEST_SIZE.1 as f32,
    )
}

/// The angle the bending joint holds in `frame`.
///
/// Frame zero is the rest pose, which gives the sequence a reference every
/// later frame departs from; the sign flips halfway so the cube bends both
/// ways instead of swinging through one arc.
fn bend_angle(frame: usize) -> f32 {
    let half = FRAMES / 2;
    let step = if frame < half {
        frame as isize
    } else {
        half as isize - (frame - half) as isize
    };
    step as f32 * 0.42
}
