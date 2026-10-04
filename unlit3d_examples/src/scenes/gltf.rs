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

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE};
use unlit3d::gltf::UnlitGltf;
use unlit3d::prelude::*;

/// The frames a snapshot capture freezes, one snapshot each.
const FRAMES: usize = 6;

/// The fox asset, carried in the binary so the scene needs no file at run
/// time.
const FOX: &[u8] = include_bytes!("../../../unlit3d_asset_files/assets/Fox.glb");

/// The morph stress test asset, carried the same way.
const MORPH: &[u8] = include_bytes!("../../../unlit3d_asset_files/assets/MorphStressTest.glb");

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

/// The scene: two glTF documents whose clips play in real time, with six frames
/// stored as snapshots.
pub static SCENE: SceneDef = SceneDef {
    id: "gltf",
    title: "glTF documents",
    description: "a skinned fox and a morph-target cube playing their glTF clips",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    frames: FRAMES as u32,
    // The window plays each clip continuously from the frame delta; only the
    // snapshot capture walks the fixed sequence below.
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// Build the glTF scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    _renderer: Entity,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let fox = UnlitGltf::from_bytes(FOX).expect("the fox asset is a valid glTF document");
    let morph = UnlitGltf::from_bytes(MORPH).expect("the morph asset is a valid glTF document");

    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    let source_entity = spawn_source(world, source);

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
    let fox_nodes = fox.spawn_default_scene(world, &fox_resources);
    let morph_nodes = morph.spawn_default_scene(world, &morph_resources);

    // Neither document animates the node its mesh hangs from — the fox's mesh
    // sits on a root beside the skeleton, and the cube's node is animated only
    // in its morph weights — so the placement written here survives every
    // frame instead of being recomposed away.
    for node in &fox_nodes {
        for &entity in &node.entities {
            let _ = world.with_mut::<Transform, _>(entity, |current| *current = FOX_PLACEMENT);
        }
    }
    for node in &morph_nodes {
        for &entity in &node.entities {
            let _ = world.with_mut::<Transform, _>(entity, |current| *current = MORPH_PLACEMENT);
        }
    }

    let camera_entity = world.spawn((super::camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size),));

    let fox_animation = animation_index(&fox, FOX_ANIMATION);
    let morph_animation = animation_index(&morph, MORPH_ANIMATION);
    let fox_duration = fox.animation_duration(fox_animation);
    let morph_duration = morph.animation_duration(morph_animation);

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
                (clock % fox_duration, clock % morph_duration)
            };
            fox.apply_animation(world, fox_animation, fox_time, &fox_nodes);
            morph.apply_animation(world, morph_animation, morph_time, &morph_nodes);
            // The camera the whole sequence shares, re-aimed at the frame's
            // aspect so a resized window does not stretch the assets.
            super::aim_camera(world, camera_entity, CAMERA_EYE, CAMERA_TARGET, frame_size);
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
fn animation_index(document: &UnlitGltf, name: &str) -> usize {
    (0..document.animation_count())
        .find(|&animation| document.animation_name(animation) == Some(name))
        .unwrap_or_else(|| panic!("the document has an animation named `{name}`"))
}
