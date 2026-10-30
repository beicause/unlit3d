//! The morphed-cube scene: a cube blended by two morph targets across six
//! frames.
//!
//! Ported from the `ecs_morphed_cube_matches_snapshots` test. A cube with two
//! morph targets is drawn over several frames while its target weights
//! animate: the weights live in a [`MorphWeights`] component the CPU updates,
//! and each frame is stored as its own snapshot.

use super::{
    SEQUENCE_STEP, SceneControl, SceneDef, SceneOptions, TEST_SIZE, default_options, morph_deltas,
};
use unlit_ecs::{Bundle, DynamicBundle, bundle};
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 6;

/// The scene: a cube changing shape frame by frame, each stored as its own
/// snapshot.
pub static SCENE: SceneDef = SceneDef {
    id: "morphed",
    title: "Morphed cube",
    description: "a cube blended by two morph targets over six frames",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: None,
    frames: FRAMES as u32,
    step_seconds: Some(SEQUENCE_STEP),
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// The scene's one visible object: a cube blended by its own weights.
///
/// The mesh and the entity that carries the weights are what a spawn has to
/// name, so they are the fields; the transform and the tint belong to the
/// scene, so they are the bundle's own.
struct MorphedCube {
    mesh: GpuMesh,
    pipeline: GpuRenderPipeline,
    weights: Entity,
}

impl Bundle for MorphedCube {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            Transform {
                scale: glam::Vec3::splat(0.9),
                ..Default::default()
            },
            InstanceColor::new(glam::Vec4::new(0.55, 0.85, 1.0, 1.0)),
            self.mesh,
            self.pipeline,
            MorphBinding {
                weights: self.weights
            },
        ]
    }
}

/// Build the morphed-cube scene's world content.
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
    // The variant that reads the morph bindings: its mesh group carries the
    // displacement buffer a plain cube's does not.
    let key = LitPipelineKey::new(default_options(&source.device(&world.root_scope())));
    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);

    let (positions, uvs, colors, indices) = super::cube();
    let deltas = morph_deltas(&positions);
    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_lit_mesh(
            world,
            &key.options,
            LitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                morph_deltas: Some(deltas),
                ..Default::default()
            },
        )
    });

    let camera_entity = super::spawn_in(world, source_entity, bundle![camera(size)]);
    // The weights are an entity of their own: the mesh names it rather than
    // owning it, so blending a pose is one component write and nothing on the
    // mesh changes.
    let weights_entity = super::spawn_in(
        world,
        source_entity,
        bundle![MorphWeights {
            weights: weights(0).to_vec()
        }],
    );
    super::spawn_in(
        world,
        source_entity,
        MorphedCube {
            mesh,
            pipeline: GpuRenderPipeline::new(key),
            weights: weights_entity,
        },
    );

    SceneControl {
        advance: Box::new(move |world, frame, _delta, frame_size| {
            let frame = frame as usize % FRAMES;
            // The weights are a component write, not a mesh re-upload and not
            // a GPU call: the source packs and uploads them once per frame.
            world
                .scope(source_entity)
                .expect("the source is alive")
                .query_one::<&mut MorphWeights>(weights_entity)
                .expect("the weight entity carries a `MorphWeights`")
                .get()
                .weights = weights(frame).to_vec();
            // The camera the whole sequence shares, re-aimed at the frame's
            // aspect so a resized window does not stretch the cube.
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
            (frame < FRAMES as u32).then(|| format!("morphed/frame_{frame:02}.webp"))
        }),
    }
}

/// Where the camera sits and what it looks at.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(1.0, 1.0, 2.8);
/// The point the camera looks at.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.2, 0.0);

/// The camera the whole sequence shares, so a frame's difference is the morph's.
fn camera(size: (u32, u32)) -> Camera {
    super::camera_looking_at(CAMERA_EYE, CAMERA_TARGET, size)
}

/// The weights of `frame`, in target order.
///
/// The two targets trade off against each other and reverse halfway through,
/// so no frame repeats the one before it and both weights are exercised.
fn weights(frame: usize) -> [f32; 2] {
    let phase = frame as f32 * 0.5;
    [phase.cos().abs(), phase.sin().abs()]
}
