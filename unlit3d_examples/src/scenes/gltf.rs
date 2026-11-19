//! The glTF scene: a skinned fox and a morph-target cube, animated from the
//! documents they were loaded from.
//!
//! Both assets under `unlit3d_asset_files/assets` are loaded as glTF documents
//! and drawn through one [`MeshSource`]: the fox's mesh is deformed by its own
//! skeleton under the document's `Walk` animation, and the morph stress test's
//! mesh is deformed by its eight morph targets under the document's `Pulse`
//! animation. In the window each clip plays at its own real duration and loops.
//! A snapshot capture instead walks a fixed six-step sequence and stores each
//! step as its own snapshot, so a wrong joint matrix, a mispacked morph delta
//! or a stale binding shows up as a frame that does not move the way it should.

use super::{PlacedLight, SceneControl, SceneDef, SceneOptions, TEST_SIZE};
use unlit_ecs::bundle;
use unlit3d::gltf::LitGltf;
use unlit3d::prelude::*;

/// The frames a snapshot capture freezes, one snapshot each.
const FRAMES: usize = 6;

/// The animation each document plays, by the name the document gives it.
///
/// The fox walks; the morph cube pulses, which is the animation that drives
/// every one of its eight targets at once.
const FOX_ANIMATION: &str = "Walk";
const MORPH_ANIMATION: &str = "Pulse";

/// Where the fox stands.
///
/// The fox is authored nose along −Z, so a camera that frames the pair along
/// the axis the two are separated on would see it exactly side-on. Turning it
/// towards the camera reads the body's depth instead of flattening it to a
/// silhouette. The rotation is about the fox's own centre, which is why the
/// translation is not zero: turning a model whose origin sits at one end of
/// its length would otherwise swing it out of frame.
const FOX_PLACEMENT: Transform = Transform {
    translation: glam::Vec3::new(-4.54, 0.0, -1.01),
    // A 25° turn about Y towards the camera, written out because
    // `from_rotation_y` is not a constant function.
    rotation: glam::Quat::from_xyzw(0.0, -0.216_439_62, 0.0, 0.976_296),
    scale: glam::Vec3::ONE,
};

/// Where the morph cube stands, so it sits beside the fox rather than inside
/// it.
///
/// The two assets are authored at unrelated scales: the fox is about 155 units
/// long and 79 tall, while the morph cube is 4 units across. Scaling the cube
/// up and moving it clear of the fox is what makes both readable at once: the
/// cube is placed to the fox's side and nearer the camera, which is what lets
/// it be this large without hiding the animal behind it.
const MORPH_PLACEMENT: Transform = Transform {
    translation: glam::Vec3::new(20.0, 0.0, 90.0),
    rotation: glam::Quat::IDENTITY,
    scale: glam::Vec3::splat(20.0),
};

/// Where the camera sits, and the point it looks at.
///
/// The eye is lifted above the pair and set off to the side the fox turns
/// towards, far enough back that the fox and the scaled cube each fill most of
/// the frame without either leaving it.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(160.5, 109.0, 49.5);
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(4.5, 38.0, 26.5);

/// The ambient term the scene draws under: neither document carries
/// `KHR_materials_unlit`, so both draw lit and a frame without lights draws
/// them black.
const AMBIENT: glam::Vec4 = glam::Vec4::new(0.045, 0.045, 0.05, 0.0);

/// The scene: two glTF documents whose clips play in real time, with six frames
/// stored as snapshots.
pub static SCENE: SceneDef = SceneDef {
    id: "gltf",
    title: "glTF documents",
    description: "a skinned fox and a morph-target cube playing their glTF clips",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: None,
    frames: FRAMES as u32,
    // The window plays each clip continuously from the frame delta; only the
    // snapshot capture walks the fixed sequence below.
    step_seconds: None,
    samples: 1,
    depth: true,
    egui: false,
    reproducible_egui: false,
    build,
};

/// Build the glTF scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let fox = LitGltf::from_bytes(unlit3d_asset_bytes::fox())
        .expect("the fox asset is a valid glTF document");
    let morph = LitGltf::from_bytes(unlit3d_asset_bytes::morph_stress_test())
        .expect("the morph asset is a valid glTF document");

    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(FrameSourceOrder::MESH, source)]);

    // Both documents' resources go into the one source the scene draws
    // through: a mesh allocated by one source cannot be drawn by another, so
    // two documents sharing a frame share a source. Each document derives its
    // own pipeline keys, since a skinned primitive and a morphed one read
    // different streams.
    let (fox_resources, morph_resources) =
        super::with_mesh_source(world, source_entity, |source, world| {
            let fox_resources = fox.insert_resources(source, world);
            let morph_resources = morph.insert_resources(source, world);
            (fox_resources, morph_resources)
        });

    // The spawned handles are what `apply_animation` writes through: the fox's
    // handle carries the pose entity its skin reads, and the cube's carries
    // the weights entity its morph targets read.
    let (fox_nodes, morph_nodes) = {
        let mut scope = world.scope_mut(source_entity);
        // The embedded assets are checked in, so their scenes always spawn.
        (
            fox.spawn_default_scene(&mut scope, &fox_resources)
                .expect("the fox asset spawns"),
            morph
                .spawn_default_scene(&mut scope, &morph_resources)
                .expect("the morph asset spawns"),
        )
    };

    // Neither document animates the node its mesh hangs from — the fox's mesh
    // sits on a root beside the skeleton, and the cube's node is animated only
    // in its morph weights — so the placement written here survives every
    // frame instead of being recomposed away.
    {
        let scope = world.scope(source_entity);
        for node in &fox_nodes {
            for &entity in &node.entities {
                let _ = scope
                    .try_query_one::<&mut Transform>(entity)
                    .map(|mut current| *current.get() = FOX_PLACEMENT);
            }
        }
        for node in &morph_nodes {
            for &entity in &node.entities {
                let _ = scope
                    .try_query_one::<&mut Transform>(entity)
                    .map(|mut current| *current.get() = MORPH_PLACEMENT);
            }
        }
    }

    // Neither document carries `KHR_materials_unlit`, so both draw lit: a
    // frame without lights would draw them black. One ambient term keeps
    // every surface off pure black, and a two-hue
    // directional pair — an amber key raking from above, a blue fill facing the camera —
    // gives the fur and the cube's faces a readable shape.
    world
        .scope_mut(source_entity)
        .spawn(bundle![AmbientLight::new(AMBIENT)]);
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
        world.scope_mut(source_entity).spawn(PlacedLight {
            light: LightSource::new(DirectionalLight { color, intensity }),
            translation: glam::Vec3::ZERO,
            rotation,
        });
    }

    let camera_entity = world
        .scope_mut(source_entity)
        .spawn(bundle![super::camera_looking_at(
            CAMERA_EYE,
            CAMERA_TARGET,
            size
        )]);

    let fox_animation = animation_index(&fox, FOX_ANIMATION);
    let morph_animation = animation_index(&morph, MORPH_ANIMATION);
    let fox_duration = fox.animation_duration(unlit3d::gltf::AnimationId::new(fox_animation));
    let morph_duration = morph.animation_duration(unlit3d::gltf::AnimationId::new(morph_animation));

    // In the window each clip plays at its own real duration and loops. A
    // snapshot capture instead walks a fixed six-step sequence, so a frame does
    // not depend on when it was drawn.
    let reproducible = options.reproducible;
    let mut clock = 0.0f32;

    SceneControl {
        advance: Box::new(move |world, frame, delta, frame_size| {
            let (fox_time, morph_time) = if reproducible {
                // The sequence stops one step short of the animation's end, so
                // the six stored frames span the clip without repeating its
                // first pose at the end.
                let phase = (frame as usize % FRAMES) as f32 / FRAMES as f32;
                (fox_duration * phase, morph_duration * phase)
            } else {
                clock += delta;
                // A clip with no duration has one pose to show, not a clock
                // to take the remainder of: `x % 0.0` is NaN, and a NaN time
                // samples nothing.
                let loop_time = |duration: f32| {
                    if duration > 0.0 {
                        clock % duration
                    } else {
                        0.0
                    }
                };
                (loop_time(fox_duration), loop_time(morph_duration))
            };
            let scope = world.scope(source_entity);
            fox.apply_animation(
                &scope,
                unlit3d::gltf::AnimationId::new(fox_animation),
                fox_time,
                &fox_nodes,
            );
            morph.apply_animation(
                &scope,
                unlit3d::gltf::AnimationId::new(morph_animation),
                morph_time,
                &morph_nodes,
            );
            // The camera the whole sequence shares, re-aimed at the frame's
            // aspect so a resized window does not stretch the assets.
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
            (frame < FRAMES as u32).then(|| format!("gltf/frame_{frame:02}.webp"))
        }),
    }
}

/// The index of the animation `name` in `document`.
///
/// The assets name their animations, so the scene asks for one by name rather
/// than by a position that a re-export could shuffle.
fn animation_index(document: &LitGltf, name: &str) -> usize {
    (0..document.animation_count())
        .find(|&animation| {
            document.animation_name(unlit3d::gltf::AnimationId::new(animation)) == Some(name)
        })
        .unwrap_or_else(|| panic!("the document has an animation named `{name}`"))
}
