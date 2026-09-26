//! The instanced skin-and-morph scene: three entities sharing one deformed
//! mesh.
//!
//! Ported from the `ecs_instanced_skinned_morphed_cubes_match_snapshots`
//! test. Three entities share one skinned-and-morphed mesh. They draw the
//! same pipeline, bind the same mesh and material bind groups and read the
//! same vertex and index buffers, so the source folds them into a single
//! instanced draw; what keeps each instance distinct is the pose state the
//! instance stream carries — every instance names its own [`SkinPose`] and
//! [`MorphWeights`] entity. The frames animate those per instance, so the
//! sequence is what proves a merged draw still reads each instance's own pose.

use super::{
    BendSkin, SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_SIZE, deformation_options,
    morph_targets, rest_pose,
};
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// How many entities share the mesh.
const INSTANCES: usize = 3;

/// The scene: a crowd of three instanced cubes, each skinned and morphed on
/// its own, stored as one snapshot per frame.
pub static SCENE: SceneDef = SceneDef {
    id: "instanced_skinned_morph",
    title: "Instanced skin + morph",
    description: "three cubes sharing one mesh, deformed per instance",
    size: TEST_SIZE,
    frames: FRAMES as u32,
    step_seconds: Some(SEQUENCE_STEP),
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// Build the instanced scene's world content.
fn build(
    world: &mut LocalWorld,
    context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    // The variant that reads both the joint stream and the morph bindings.
    let key = UnlitPipelineKey::new(deformation_options(&source.device(world), true, true));
    let source_entity = spawn_source(world, source);

    let (positions, uvs, colors, indices) = super::cube();
    let skin = BendSkin::new(&positions, 0.0);
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
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
                morph_targets: &targets,
            },
        )
    });

    world.spawn((camera(),));

    // One pose entity and one weights entity per instance, all naming the
    // same shared mesh: the instances merge into a single draw whose instance
    // records carry each entity's pose.
    let mut pose_entities = Vec::new();
    let mut weight_entities = Vec::new();
    for instance in 0..INSTANCES {
        let pose_entity = world.spawn((SkinPose::new(rest_pose(2)),));
        let weight_entity = world.spawn((MorphWeights::new(instance_weights(0, instance)),));
        let color = match instance {
            0 => glam::Vec4::new(1.0, 0.75, 0.55, 1.0),
            1 => glam::Vec4::new(0.55, 0.85, 1.0, 1.0),
            _ => glam::Vec4::new(0.7, 1.0, 0.6, 1.0),
        };
        world.spawn((
            Transform {
                translation: glam::Vec3::new((instance as f32 - 1.0) * 1.3, 0.0, 0.0),
                scale: glam::Vec3::splat(0.8),
                ..Default::default()
            },
            InstanceColor::new(color),
            mesh.clone(),
            UnlitPipeline::new(key.clone()),
            SkinBinding::new(pose_entity),
            MorphBinding::new(weight_entity),
        ));
        pose_entities.push(pose_entity);
        weight_entities.push(weight_entity);
    }

    // One skin per instance, reused every frame: the pose is a component
    // write, and only the bend angle changes.
    let mut skins: Vec<BendSkin> = (0..INSTANCES)
        .map(|_| BendSkin::new(&positions, 0.0))
        .collect();

    SceneControl {
        advance: Box::new(move |world, frame, _delta| {
            let frame = frame as usize % FRAMES;
            // Update every instance's own pose: one component write per
            // instance, and the source packs and uploads them once per frame.
            for instance in 0..INSTANCES {
                skins[instance].set_angle(instance_angle(frame, instance));
                *world
                    .get_mut::<SkinPose>(pose_entities[instance])
                    .expect("the pose entity carries a `SkinPose`") = skins[instance].pose();
                world
                    .get_mut::<MorphWeights>(weight_entities[instance])
                    .expect("the weight entity carries a `MorphWeights`")
                    .weights = instance_weights(frame, instance).to_vec();
            }
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES as u32)
                .then(|| format!("instanced_skinned_morph/frame_{frame:02}.webp"))
        }),
    }
}

/// The camera the whole sequence shares, wide enough to see all instances.
fn camera() -> Camera {
    super::camera_looking_at(
        glam::Vec3::new(0.0, 1.3, 3.6),
        glam::Vec3::new(0.0, 0.2, 0.0),
        TEST_SIZE.0 as f32 / TEST_SIZE.1 as f32,
    )
}

/// The bending angle `instance` holds in `frame`.
///
/// Every instance bends on its own schedule, so the frames cannot be told
/// apart unless each instance's pose record reaches the shader.
fn instance_angle(frame: usize, instance: usize) -> f32 {
    let phase = frame as f32 * 0.9 + instance as f32 * 0.7;
    phase.sin() * 0.55
}

/// The morph weights `instance` holds in `frame`, in target order.
fn instance_weights(frame: usize, instance: usize) -> [f32; 2] {
    let phase = frame as f32 * 0.5 + instance as f32 * 1.1;
    [phase.cos().abs(), phase.sin().abs()]
}
