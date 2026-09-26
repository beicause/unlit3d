//! The transparent, z-sorted scene: overlapping translucent panes composite in
//! draw order.
//!
//! The other scenes draw opaque geometry, so the only blending they exercise is
//! the UI's own. This scene is the one that pins the unlit pipeline's
//! transparent path: three translucent panes overlap, and what the middle of
//! that overlap looks like depends on two things at once — that the draws are
//! ordered back to front, and that the blend state frames the shader's
//! straight-alpha output correctly. Either one wrong changes the pixels, which
//! is what the snapshot freezes.
//!
//! The panes carry [`ZSortedDrawing`], so the renderer draws them after the
//! opaque cubes and sorts them by camera distance rather than by registration
//! order. They are spawned in an order that is deliberately *not* their draw
//! order, so a scene that lost the sort would composite them the other way
//! round and fail the snapshot rather than happening to look right.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_looking_at, unlit_options};
use unlit_wgpu::pipeline::UnlitFlags;
use unlit3d::prelude::*;

/// How far in front of the panes the camera sits.
///
/// The panes are spread along the view direction, so their camera distances
/// differ by enough that the sort is unambiguous.
const CAMERA_EYE: glam::Vec3 = glam::Vec3::new(0.0, 0.6, 3.2);

/// The point the camera looks at.
const CAMERA_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 0.0);

/// The tint of one pane, where it sits, and how far along the view direction.
///
/// The panes are offset sideways as well as in depth, so they overlap in
/// bands: one region shows each pane alone, another two of them, and the
/// middle all three. That is what makes the snapshot discriminate — a wrong
/// sort or a wrong blend state changes the tint of the overlap bands, not just
/// the overall brightness.
///
/// The order of this table is the order the panes are drawn in after the sort
/// — farthest first — so drawing them as declared, or reversed, would disagree
/// with it.
const PANES: [(glam::Vec4, f32, f32); 3] = [
    // Farthest: red.
    (glam::Vec4::new(1.0, 0.1, 0.1, 0.6), -0.85, -0.7),
    // Middle: green.
    (glam::Vec4::new(0.1, 1.0, 0.15, 0.6), 0.0, 0.0),
    // Nearest: blue.
    (glam::Vec4::new(0.15, 0.3, 1.0, 0.6), 0.85, 0.7),
];

/// The opaque cube behind the panes, tinted so the composite has something to
/// blend against that is not the clear colour alone.
const BACKDROP_TINT: glam::Vec4 = glam::Vec4::new(0.85, 0.8, 0.35, 1.0);

/// The scene: opaque cubes behind three overlapping translucent panes.
pub static SCENE: SceneDef = SceneDef {
    id: "transparent_zsorted",
    title: "Transparent z-sorted panes",
    description: "translucent panes composited back to front over opaque cubes",
    size: TEST_SIZE,
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// Build the transparent scene's world content.
fn build(
    world: &mut LocalWorld,
    context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    let device = source.device(world);

    // Two variants of the same unlit pipeline: the opaque one every other
    // scene draws with, and a translucent one that blends straight-alpha
    // output over the target without writing depth.
    //
    // Depth writes are off because a blended pane must not hide the pane
    // behind it, but depth *testing* stays on: the panes are composited among
    // themselves by their draw order, and an opaque cube in front of them must
    // still occlude them.
    let opaque_key = UnlitPipelineKey::new(unlit_options(&device));
    let mut translucent = unlit_options(&device);
    translucent.color_target.blend = Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    });
    // A pane is seen from the front, so culling its back face keeps it to one
    // blended layer; drawing both faces of the thin slab would blend it twice.
    translucent.primitive.cull_mode = Some(wgpu::Face::Back);
    if let Some(depth) = translucent.depth_stencil.as_mut() {
        // Depth *testing* stays on, so an opaque cube in front still occludes a
        // pane; writes are off because a blended pane must not hide the pane
        // behind it. The panes are composited among themselves by draw order.
        depth.depth_write_enabled = Some(false);
    }
    // The translucent variant reads no per-vertex colour: each pane's tint is
    // its own instance colour, so one mesh serves all of them.
    translucent.flags = UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_INSTANCE;
    let translucent_key = UnlitPipelineKey::new(translucent);

    let source_entity = spawn_source(world, source);

    let (positions, uvs, colors, indices) = super::cube();

    // The backdrop the panes composite over: an opaque cube drawn with depth
    // writes, so it also occludes nothing that should be in front of it.
    let backdrop = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_unlit_mesh(
            world,
            &opaque_key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    // The panes: one mesh, three instances, each tinted and placed by its
    // instance stream.
    let pane_mesh = super::with_mesh_source(world, source_entity, |source, world| {
        source.allocate_unlit_mesh(
            world,
            &translucent_key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: None,
                colors: None,
                indices: Some(&indices),
                ..Default::default()
            },
        )
    });

    let aspect = TEST_SIZE.0 as f32 / TEST_SIZE.1 as f32;
    world.spawn((camera_looking_at(CAMERA_EYE, CAMERA_TARGET, aspect),));

    world.spawn((
        Transform {
            translation: glam::Vec3::new(0.0, 0.0, -1.6),
            scale: glam::Vec3::splat(1.1),
            ..Default::default()
        },
        InstanceColor::new(BACKDROP_TINT),
        backdrop,
        UnlitPipeline::new(opaque_key),
    ));

    // Spawned nearest-first on purpose: registration order is not draw order
    // for a z-sorted entity, so only a correct sort composites them red,
    // green, blue from back to front.
    for (tint, x, z) in PANES.iter().rev() {
        world.spawn((
            Transform {
                translation: glam::Vec3::new(*x, 0.0, *z),
                rotation: glam::Quat::from_rotation_y(0.35),
                scale: glam::Vec3::new(1.05, 1.05, 0.05),
            },
            InstanceColor::new(*tint),
            pane_mesh.clone(),
            UnlitPipeline::new(translucent_key.clone()),
            ZSortedDrawing,
        ));
    }

    SceneControl {
        // A single frame: the composite is static, so it needs no advance.
        advance: Box::new(|_world, _frame, _delta| {}),
        // The first frame is the snapshot; whichever frame is drawn, the
        // picture is the same.
        snapshot: Box::new(|frame| (frame == 0).then(|| "transparent_zsorted.webp".to_owned())),
    }
}
