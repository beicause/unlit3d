//! The mesh-and-UI scene: a cube and a rich egui panel in one pass.
//!
//! Ported from the `mesh_and_ui_in_one_frame` snapshot test: the frame where
//! the mesh source and the UI source have to agree on record order, scissor
//! state and the frame's load ops. The snapshot `mesh_and_ui.webp` freezes the
//! second frame.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_view, cube, unlit_options};
use unlit_ecs::{Bundle, DynamicBundle, bundle};
use unlit3d::prelude::*;

/// The colour the scene clears to.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.02,
    b: 0.02,
    a: 1.0,
};

/// The scene's one visible object: a cube tilted towards the camera.
///
/// The mesh and its pipeline are what a spawn has to name, so they are the
/// fields; the placement belongs to the scene, so it is the bundle's own.
struct TiltedCube {
    mesh: GpuMesh,
    pipeline: UnlitPipeline,
}

impl Bundle for TiltedCube {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            Transform {
                translation: glam::Vec3::new(0.0, 0.2, 0.0),
                rotation: glam::Quat::from_rotation_y(0.6),
                scale: glam::Vec3::splat(0.7),
            },
            self.mesh,
            self.pipeline,
        ]
    }
}

/// The scene: a cube with the same rich interface over it.
pub static SCENE: SceneDef = SceneDef {
    id: "mesh_and_ui",
    title: "Mesh and UI",
    description: "a cube with the rich egui panel over it, one pass",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: Some(CLEAR_COLOR),
    frames: 2,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: true,
    reproducible_ui: false,
    build,
};

/// Build the mesh-and-UI scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_unlit_family(&world.root_scope());
    let key = UnlitPipelineKey::new(unlit_options(&source.device(&world.root_scope())));
    let source_entity = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(source)]);

    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        let (positions, uvs, colors, indices) = cube();
        source.allocate_unlit_mesh(
            world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    let camera_entity = super::spawn_in(world, source_entity, bundle![camera_view(size)]);
    super::spawn_in(
        world,
        source_entity,
        TiltedCube {
            mesh,
            pipeline: UnlitPipeline::new(key),
        },
    );

    if options.ui {
        super::spawn_in(
            world,
            ui.expect("the UI source was mounted"),
            bundle![super::ui_only::rich_panel()],
        );
    }

    SceneControl {
        // Static: egui needs no per-frame advance, and the camera only has to
        // keep the frame's aspect so a resized window does not stretch it.
        advance: Box::new(move |world, _frame, _delta, frame_size| {
            super::aim_camera(
                world,
                source_entity,
                camera_entity,
                super::DEFAULT_EYE,
                super::DEFAULT_TARGET,
                frame_size,
            );
        }),
        snapshot: Box::new(|frame| (frame == 1).then(|| "mesh_and_ui.webp".to_owned())),
    }
}

// The panel the mesh shows through is the UI-only scene's own rich interface:
// the two scenes share one panel, and only the world behind it differs.
