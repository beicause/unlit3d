//! The mesh-and-UI scene: a cube, a Blitz page and a rich egui panel in one
//! pass.
//!
//! The three sources share the pass and have to agree on record order, scissor
//! state and the frame's load ops: the meshes come first, then the page, which
//! composes over them, then the overlay. The snapshot `mesh_and_ui.webp`
//! freezes the second frame.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_view, cube, default_options};
use unlit_ecs::bundle;
use unlit3d::prelude::*;

/// The colour the scene clears to.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.02,
    b: 0.02,
    a: 1.0,
};

/// The scene: a cube with the same rich interface over it.
pub static SCENE: SceneDef = SceneDef {
    id: "mesh_and_ui",
    title: "Mesh, Blitz and egui",
    description: "a cube, a Blitz page and the rich egui panel in one pass",
    size: TEST_SIZE,
    baseline: Some(TEST_SIZE),
    clear: Some(CLEAR_COLOR),
    frames: 2,
    step_seconds: None,
    samples: 1,
    depth: true,
    egui: true,
    reproducible_egui: false,
    build,
};

/// Build the mesh-and-egui scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    pass: Entity,
    ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let key = LitPipelineKey::new(default_options());
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(FrameSourceOrder::MESH, source)]);

    let mesh = super::with_mesh_source(world, source_entity, |source, world| {
        let (positions, uvs, colors, indices) = cube();
        source.allocate_lit_mesh(
            world,
            &key.options,
            LitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    let camera_entity = world
        .scope_mut(source_entity)
        .spawn(bundle![camera_view(size)]);
    world.scope_mut(source_entity).spawn(LitMeshBundle {
        transform: Transform {
            translation: glam::Vec3::new(0.0, 0.2, 0.0),
            rotation: glam::Quat::from_rotation_y(-0.6),
            scale: glam::Vec3::splat(0.7),
        },
        primitive: LitMeshPrimitive::new(mesh, GpuRenderPipeline::new(key)),
        skin: None,
        morph: None,
    });

    // The page is a whole document, so it is a source of its own rather than a
    // panel: it is laid out and rasterized on the CPU and composited into the
    // same pass, under the overlay. Its markup is transparent except for the
    // card, which is translucent so the cube and the overlay's band all show
    // through it: the frame proves the three sources share one pass and stack
    // in the order their sources declare.
    let page_source = world.scope_mut(pass).spawn(bundle![Source::new(
        FrameSourceOrder::BLITZ,
        BlitzSource::new()
    )]);
    world
        .scope_mut(page_source)
        .spawn(bundle![BlitzDocument::new(PAGE)]);

    if options.egui {
        world
            .scope_mut(ui.expect("the egui source was mounted"))
            .spawn(bundle![super::egui_only::rich_panel()]);
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

/// The page the scene composites between the cube and the overlay.
///
/// It covers the same band the overlay's translucent strip does, so the frame
/// shows all three sources in one region: the mesh behind, the page's own card
/// over it, and the overlay's red band over the card. The card's background is
/// translucent and its text is not, so a frame that dropped the page, drew it
/// over the overlay instead of under it, or stopped blending it would differ.
///
/// The rest of the document is left transparent: the window is the page's
/// viewport, so an opaque body would hide the cube the scene exists to show.
const PAGE: &str = r#"<!doctype html>
<html>
<head>
<style>
  html, body { margin: 0; background: transparent; }
  .card {
    position: absolute;
    left: 8px;
    top: 136px;
    width: 240px;
    height: 40px;
    box-sizing: border-box;
    padding: 5px 10px;
    border: 2px solid rgba(22, 50, 79, 0.8);
    border-radius: 12px;
    background: rgba(232, 176, 75, 0.55);
    color: #16324f;
    font: 12px monospace;
  }
</style>
</head>
<body>
  <div class="card">blitz 页面</div>
</body>
</html>"#;

// The panel the mesh shows through is the overlay-only scene's own rich interface:
// the two scenes share one panel, and only the world behind it differs.
