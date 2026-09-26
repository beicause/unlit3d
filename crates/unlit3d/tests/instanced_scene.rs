//! Multi-frame snapshot coverage for automatic instancing of deformed meshes.
//!
//! Three entities share one skinned-and-morphed mesh. They draw the same
//! pipeline, bind the same mesh and material bind groups and read the same
//! vertex and index buffers, so the source folds them into a single instanced
//! draw; what keeps each instance distinct is the pose state the instance
//! stream carries — every instance names its own [`SkinPose`] and
//! [`MorphWeights`] entity. The frames animate those per instance, so the
//! sequence is what proves a merged draw still reads each instance's own pose:
//! a draw that reused instance zero's record for all three would freeze the
//! crowd into one shape.

pub mod common;

use common::*;
use unlit3d::prelude::*;
use wgpu_unlit_render::scene::DrawRange;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// How many entities share the mesh.
const INSTANCES: usize = 3;

/// The camera the whole sequence shares, wide enough to see all instances.
fn camera() -> Camera {
    camera_looking_at(
        glam::Vec3::new(0.0, 1.3, 3.6),
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// The per-vertex displacement of each morph target, as in `morphed_scene`.
fn morph_targets(positions: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let taper = positions
        .iter()
        .map(|position| {
            let height = (position[1] + 1.0) * 0.5;
            [
                -position[0] * height * 0.7,
                0.0,
                -position[2] * height * 0.7,
            ]
        })
        .collect();
    let widen = positions
        .iter()
        .map(|position| {
            let height = (1.0 - position[1]) * 0.5;
            [position[0] * height * 0.6, 0.0, position[2] * height * 0.6]
        })
        .collect();
    (taper, widen)
}

/// The bending angle `instance` holds in `frame`.
///
/// Every instance bends on its own schedule, so the frames cannot be told
/// apart unless each instance's pose record reaches the shader — and no two
/// instances share a frame's shape.
fn instance_angle(frame: usize, instance: usize) -> f32 {
    let phase = frame as f32 * 0.9 + instance as f32 * 0.7;
    phase.sin() * 0.55
}

/// The morph weights `instance` holds in `frame`, in target order.
fn instance_weights(frame: usize, instance: usize) -> [f32; 2] {
    let phase = frame as f32 * 0.5 + instance as f32 * 1.1;
    [phase.cos().abs(), phase.sin().abs()]
}

/// How many pixels of `rgba` land within `tolerance` of `rgb` on every channel.
///
/// Used to prove an instance's own tint is on screen, which the per-instance
/// colour only reaches through the instance record the draw read for it.
fn count_close_to(rgba: &[u8], rgb: [f32; 3], tolerance: u8) -> usize {
    let target = [
        (rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
    ];
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|px| (0..3).all(|i| px[i].abs_diff(target[i]) <= tolerance))
        .count()
}

/// A crowd of instanced cubes, each skinned and morphed on its own, matches a
/// multi-frame snapshot sequence — and really is one instanced draw.
#[test]
fn ecs_instanced_skinned_morphed_cubes_match_snapshots() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);

    // The variant that reads both the joint stream and the morph bindings.
    let key = UnlitPipelineKey::new(deformation_options(&ctx.device, true, true));
    let (positions, _uvs, _colors, _indices) = cube();
    let skin = BendSkin::new(&positions, 0.0);
    let (taper, widen) = morph_targets(&positions);
    let targets = [
        UnlitMorphTarget { positions: &taper },
        UnlitMorphTarget { positions: &widen },
    ];
    let mesh = gpu.allocate_deformed_cube_mesh(
        &world,
        &key,
        Some(&skin.joints),
        Some(&skin.weights),
        &targets,
    );

    world.spawn((camera(),));

    // One pose entity and one weights entity per instance, all naming the
    // same shared mesh: the instances merge into a single draw whose instance
    // records carry each entity's pose.
    let mut pose_entities = Vec::new();
    let mut weight_entities = Vec::new();
    let mut colors = Vec::new();
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
        colors.push(color);
    }

    let target = gpu.bind_offscreen_target(&world, "test::ecs_instanced_skinned_morph");

    let mut previous: Option<Vec<u8>> = None;
    for frame in 0..FRAMES {
        // Update every instance's own pose: one component write per instance,
        // and the source packs and uploads them once per frame.
        for instance in 0..INSTANCES {
            let mut instance_skin = BendSkin::new(&positions, 0.0);
            instance_skin.set_angle(instance_angle(frame, instance));
            *world
                .get_mut::<SkinPose>(pose_entities[instance])
                .expect("the pose entity carries a `SkinPose`") = instance_skin.pose();
            world
                .get_mut::<MorphWeights>(weight_entities[instance])
                .expect("the weight entity carries a `MorphWeights`")
                .weights = instance_weights(frame, instance).to_vec();
        }

        gpu.render(&world);

        let pixels = Frame {
            rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
            width: WIDTH,
            height: HEIGHT,
        };

        // What the snapshot may freeze: the crowd is on screen, each instance
        // is lit by its own colour, and the frame moved on from the last one.
        let covered = count_pixels_off_background(&pixels, CLEAR, 12);
        assert!(
            covered > (WIDTH * HEIGHT) as usize / 50,
            "frame {frame} should show the crowd, got {covered} lit pixels"
        );
        for color in &colors {
            assert!(
                count_close_to(&pixels.rgba, (*color).truncate().into(), 30) > 0,
                "frame {frame} should show instance tinted {color:?}"
            );
        }
        if let Some(previous) = &previous {
            assert_ne!(
                &pixels.rgba, previous,
                "frame {frame} should differ from the one before it"
            );
        }
        previous = Some(pixels.rgba.clone());

        assert_image_snapshot(
            &format!("instanced_skinned_morph/frame_{frame:02}.webp"),
            &pixels,
            WIDTH,
            HEIGHT,
        );
    }

    // The three entities share one mesh, one material and one pipeline, so
    // they are drawn as a single instanced draw whose range covers all three
    // instances — not as three draws that each pay for a state re-bind.
    gpu.with_mesh_source(&world, |source, _| {
        let draws = &source.scene().draws;
        assert_eq!(
            draws.len(),
            1,
            "the shared mesh is one instanced draw, got {}",
            draws.len()
        );
        let instances = match &draws[0].range {
            DrawRange::Indexed { instances, .. } | DrawRange::Vertices { instances, .. } => {
                instances.clone()
            }
        };
        assert_eq!(
            instances,
            0..INSTANCES as u32,
            "the draw covers every instance, in order"
        );
    });
}
