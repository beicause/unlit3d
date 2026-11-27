//! The instanced skin-and-morph scene: three entities sharing one deformed
//! mesh.
//!
//! Three entities share one skinned-and-morphed mesh. They draw the
//! same pipeline, bind the same mesh and material bind groups and read the
//! same vertex and index buffers, so the source folds them into a single
//! instanced draw; what keeps each instance distinct is the pose state the
//! instance stream carries — every instance names its own [`SkinPose`] and
//! [`MorphWeights`] entity. The frames animate those per instance, so the
//! sequence is what proves a merged draw still reads each instance's own pose.

use super::{
    BendSkin, SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_SIZE, morph_deltas,
};
use unlit_ecs::bundle;
use unlit_wgpu::pipeline::LitOptions;
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// How many entities share the mesh.
const INSTANCES: usize = 3;

/// Where an instance sits. Skinned vertices ignore the mesh entity transform,
/// so this placement is baked into the instance joint matrices; the entity
/// keeps it for culling.
fn instance_transform(index: usize) -> Transform {
    Transform {
        translation: glam::Vec3::new((index as f32 - 1.0) * 1.3, 0.0, 0.0),
        scale: glam::Vec3::splat(0.8),
        ..Default::default()
    }
}

/// The scene: a crowd of three instanced cubes, each skinned and morphed on
/// its own, stored as one snapshot per frame.
pub static SCENE: SceneDef = SceneDef {
    id: "instanced_skinned_morph",
    title: "Instanced skin + morph",
    description: "three cubes sharing one mesh, deformed per instance",
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

/// Build the instanced scene's world content.
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
    // The variant that reads both the joint stream and the morph bindings.
    let key = LitPipelineKey::new(LitOptions::unlit_tinted());
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(SourceOrder::MESH, source)]);

    let (positions, uvs, colors, indices) = super::cube();
    let skin = BendSkin::new(&positions, 0.0);
    let deltas = morph_deltas(&positions);
    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_lit_mesh(
            world,
            LitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                normals: None,
                indices: Some(&indices),
                joints: Some(&skin.joints),
                weights: Some(&skin.weights),
                morph_deltas: Some(deltas),
            },
        )
    });

    let camera_entity = world.scope_mut(source_entity).spawn(bundle![camera(size)]);

    // One pose entity and one weights entity per instance, all naming the
    // same shared mesh: the instances merge into a single draw whose instance
    // records carry each entity's pose.
    let mut pose_entities = Vec::new();
    let mut weight_entities = Vec::new();
    for instance in 0..INSTANCES {
        let transform = instance_transform(instance);
        let pose_entity = world.scope_mut(source_entity).spawn(bundle![
            skin.pose_at(glam::Mat4::from(transform.compute_matrix()))
        ]);
        let weight_entity = world.scope_mut(source_entity).spawn(bundle![MorphWeights {
            weights: instance_weights(0, instance).to_vec()
        }]);
        let color = match instance {
            0 => glam::Vec4::new(1.0, 0.75, 0.55, 1.0),
            1 => glam::Vec4::new(0.55, 0.85, 1.0, 1.0),
            _ => glam::Vec4::new(0.7, 1.0, 0.6, 1.0),
        };
        world.scope_mut(source_entity).spawn(LitMeshBundle {
            transform,
            primitive: LitMeshPrimitive {
                color: InstanceColor::new(color),
                ..LitMeshPrimitive::new(mesh.clone(), GpuRenderPipeline::new(key.clone()))
            },
            skin: Some(SkinBinding { pose: pose_entity }),
            morph: Some(MorphBinding {
                weights: weight_entity,
            }),
        });
        pose_entities.push(pose_entity);
        weight_entities.push(weight_entity);
    }

    // One skin per instance, reused every frame: the pose is a component
    // write, and only the bend angle changes.
    let mut skins: Vec<BendSkin> = (0..INSTANCES)
        .map(|_| BendSkin::new(&positions, 0.0))
        .collect();

    SceneControl {
        advance: Box::new(move |world, frame, _delta, frame_size| {
            let frame = frame as usize % FRAMES;
            // Update every instance's own pose: one component write per
            // instance, and the source packs and uploads them once per frame.
            {
                let scope = world.scope(source_entity);
                for instance in 0..INSTANCES {
                    skins[instance].set_angle(instance_angle(frame, instance));
                    let world = glam::Mat4::from(instance_transform(instance).compute_matrix());
                    *scope
                        .query_one::<&mut SkinPose>(pose_entities[instance])
                        .get() = skins[instance].pose_at(world);
                    scope
                        .query_one::<&mut MorphWeights>(weight_entities[instance])
                        .get()
                        .weights = instance_weights(frame, instance).to_vec();
                }
            }
            // The camera the whole sequence shares, re-aimed at the frame's
            // aspect so a resized window does not stretch the instances.
            super::aim_camera(
                world,
                source_entity,
                camera_entity,
                CAMERA_EYE,
                CAMERA_TARGET,
                frame_size,
            );
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES as u32)
                .then(|| format!("instanced_skinned_morph/frame_{frame:02}.webp"))
        }),
    }
}

/// Where the camera sits, far enough back to see all instances, and what it
/// looks at.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.0, 1.3, 3.6);
/// The point the camera looks at.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.2, 0.0);

/// The camera the whole sequence shares, wide enough to see all instances.
fn camera(size: (u32, u32)) -> Camera {
    super::camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size)
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
