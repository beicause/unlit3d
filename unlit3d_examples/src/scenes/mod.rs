//! The example's selectable scenes.
//!
//! A scene is a [`SceneDef`]: the fixed size and frame count its snapshots
//! were captured at, what offscreen target it draws into, and a
//! [`build`](SceneDef::build) that populates an ECS world. The world it builds is the ordinary one every
//! frame loop drives — the frame's context and [`Renderer`] are already in it
//! — so the windowed and headless paths differ only in what they bind and what
//! they do with the result.
//!
//! A scene's [`SceneControl`] carries the per-frame behaviour (an animated
//! sequence, the orbit of the camera) and the snapshot each frame verifies
//! against in the headless path. Scenes without snapshots — the windowed
//! example's own — report none.
//!
//! Every scene here is ported from a GPU snapshot test that used to live in
//! `crates/unlit3d/tests`, which is why its size, frame count and content are
//! exactly what that test froze: the stored snapshots in the
//! `unlit3d_asset_files` submodule still verify them.

pub mod animated;
pub mod instanced_skinned_morph;
pub mod mesh_and_ui;
pub mod mesh_topologies;
pub mod morphed;
pub mod skinned;
pub mod spin_cube;
pub mod transparent;
pub mod ui_only;

use unlit_wgpu::pipeline::UnlitOptions;
use unlit3d::prelude::*;

/// Advances a scene's behaviour by one frame.
///
/// `frame` counts the frames the scene has drawn; the scenes that animate
/// through a fixed sequence loop it, so a windowed scene keeps moving after
/// the sequence that produced its snapshots ends. `delta` is the seconds since
/// the previous frame. `size` is the render target's size in physical pixels,
/// which a scene reads here rather than capturing the size it was built at: a
/// camera whose projection has to follow the target's aspect is re-aimed from
/// this every frame, so a window that changes shape never stretches the scene.
pub type Advance = Box<dyn FnMut(&mut World, u32, f32, (u32, u32))>;

/// Names the snapshot a frame verifies against, if it has one.
pub type Snapshot = Box<dyn Fn(u32) -> Option<String>>;

/// How the example's runner drives one scene.
///
/// The two closures are what remains of the scene once its world is built: the
/// behaviour that advances every frame, and the snapshot the headless path
/// compares each frame against.
pub struct SceneControl {
    /// Advance the scene's behaviour by `delta` seconds.
    pub advance: Advance,
    /// The snapshot the headless path compares frame `frame` against, if any.
    ///
    /// The name is relative to the snapshot directory, exactly as the test the
    /// scene was ported from stored it.
    pub snapshot: Snapshot,
}

/// One selectable scene.
///
/// The fields the headless path draws the scene at — [`Self::size`],
/// [`Self::frames`], [`Self::samples`] and [`Self::depth`] — reproduce the
/// test the scene came from, so a captured frame is comparable with the
/// snapshot it was stored against. The windowed path draws at the window's own
/// size instead, and only [`Self::build`] is shared.
pub struct SceneDef {
    /// The scene's stable id, used by `--scene <ID>`.
    pub id: &'static str,
    /// A short title for the GUI and the scene list.
    pub title: &'static str,
    /// What the scene shows, one line for the GUI.
    pub description: &'static str,
    /// The render target size the scene's snapshots were captured at.
    pub size: (u32, u32),
    /// How many frames the headless path draws before finishing.
    pub frames: u32,
    /// How long each frame stays on screen in the windowed loop, in seconds,
    /// or `None` for a scene that animates continuously from the frame delta.
    ///
    /// The headless path ignores this and draws the frames back to back, so a
    /// capture stays reproducible; the windowed path uses it so a scene whose
    /// frames were frozen as a sequence plays at a watchable pace rather than
    /// as fast as the display refreshes.
    pub step_seconds: Option<f32>,
    /// The sample count of the offscreen target the headless path binds.
    pub samples: u32,
    /// Whether that offscreen target carries a depth-stencil attachment.
    pub depth: bool,
    /// Whether the scene draws UI panels of its own.
    ///
    /// The headless path mounts a UI source to drive them; the windowed path
    /// always mounts one, for the scene selector.
    pub ui: bool,
    /// Whether the scene's UI animates against egui's clock.
    ///
    /// A scene whose snapshots were captured with the clock suppressed — the
    /// example's own, whose panels live in egui windows — declares `true`, so
    /// the headless path zeroes egui's animation time for it. The ported test
    /// scenes were captured with the clock live, and declare `false`.
    pub reproducible_ui: bool,
    /// The SSIMULACRA2 score this scene's snapshots accept, when the default is
    /// not the right bar for it.
    ///
    /// The default suits a scene whose frame covers whole surfaces: a
    /// regression changes large regions of it, so its score falls far below the
    /// line, while two implementations of the same drawing land close together.
    /// A scene of thin primitives is different, because its pixels are edges —
    /// and which triangle an edge pixel belongs to is a rasterizer's
    /// tie-breaking rule to decide, one the API leaves to each implementation.
    /// A handful of such pixels is worth tens of points there, so the default
    /// would reject a frame that is right in every way that matters.
    ///
    /// A scene that declares one should put it in the gap its platform
    /// differences and its real regressions leave: above every score a correct
    /// frame has been seen to reach on any backend, below every score a broken
    /// one has. [`mesh_topologies`] is the scene that needs one, and its
    /// definition carries the measurements.
    ///
    /// `--min-score` overrides it, since a caller asking for a specific bar
    /// means that bar.
    pub min_score: Option<f64>,
    /// Build the scene's world content and return its per-frame behaviour.
    ///
    /// The world already holds the frame's context and the renderer resource
    /// entity; the scene spawns its sources, meshes, camera and entities.
    pub build: fn(&mut World, RenderContext, Entity, (u32, u32), SceneOptions) -> SceneControl,
}

/// Every selectable scene, in the order the GUI lists and `--scene all` runs
/// them.
pub static SCENES: &[&SceneDef] = &[
    &spin_cube::SCENE,
    &ui_only::SCENE,
    &mesh_and_ui::SCENE,
    &animated::SCENE,
    &skinned::SCENE,
    &morphed::SCENE,
    &instanced_skinned_morph::SCENE,
    &mesh_topologies::SCENE,
    &transparent::SCENE,
];

/// The scene whose id is `id`, if any.
pub fn by_id(id: &str) -> Option<&'static SceneDef> {
    SCENES.iter().copied().find(|scene| scene.id == id)
}

/// The scene the example runs when none is asked for.
pub fn default() -> &'static SceneDef {
    &spin_cube::SCENE
}

/// A table of every scene, for `--list-scenes` and `--help`.
pub fn list_text() -> String {
    let mut text = String::from("Scenes:\n");
    for scene in SCENES {
        text.push_str(&format!("  {:<22} {}\n", scene.id, scene.description));
    }
    text
}

impl SceneDef {
    /// The score this scene's snapshots have to reach, or the example's default
    /// when the scene does not say.
    pub fn min_score(&self) -> f64 {
        self.min_score.unwrap_or(crate::cli::DEFAULT_MIN_SCORE)
    }
}

/// What a scene is built with.
#[derive(Clone, Copy)]
pub struct SceneOptions {
    /// Mount the scene's own UI panels.
    pub ui: bool,
    /// Mount the windowed shell's scene-selector panel.
    pub selector: bool,
    /// Suppress everything egui animates against the clock, so a captured
    /// frame does not depend on when it was drawn.
    pub reproducible: bool,
    /// Seconds each frame of a fixed-sequence scene stays on screen, or `None`
    /// to advance the sequence once per drawn frame.
    ///
    /// A capture passes `None` so it draws exactly the frames the snapshots
    /// froze; a windowed run passes the scene's own [`SceneDef::step_seconds`]
    /// so the sequence plays at a watchable pace.
    pub sequence_step: Option<f32>,
}

// ---------------------------------------------------------------------------
// Shared geometry and camera helpers
//
// These are the fixtures the ported tests shared, restated here because the
// example cannot reach into a test crate.
// ---------------------------------------------------------------------------

/// The size the ported test scenes draw at, matching their snapshots.
pub const TEST_SIZE: (u32, u32) = (256, 192);

/// The format the test scenes render into: sRGB, like the example's window.
pub const TEST_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Seconds each frame of a fixed-sequence scene stays on screen in the
/// windowed loop.
///
/// The ported test scenes froze an animation as a handful of frames, so
/// playing them one per displayed frame would flash past in a fraction of a
/// second. The headless path ignores this and draws the frames back to back.
pub const SEQUENCE_STEP: f32 = 0.5;

/// The raw channels of one mesh: `(positions, uvs, colors, indices)`.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A unit cube centred at the origin, returned as
/// `(positions, uvs, colors, indices)`.
///
/// The colors are quantized to the `Unorm8x4` width the vertex stream stores,
/// exactly as the tests uploaded them.
pub fn cube() -> RawMesh {
    let faces = [
        ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, 1.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ];

    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();

    for (normal, tangent) in faces {
        let base = positions.len() as u32;
        let normal = glam::Vec3::from(normal);
        let tangent = glam::Vec3::from(tangent);
        let bitangent = normal.cross(tangent);
        for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let position = normal + tangent * u + bitangent * v;
            positions.push(position.to_array());
            uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
            let color = (position + 1.0) * 0.5;
            colors.push([color.x, color.y, color.z, 1.0]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // The stream stores colors as `Unorm8x4`, so quantize once here rather
    // than carrying a float copy through the scene.
    let colors = unlit_wgpu::mesh::quantize_colors(&colors).collect();
    (positions, uvs, colors, indices)
}

/// The aspect ratio of a `size`-pixel render target.
///
/// Every camera in the example takes its aspect from here rather than from the
/// size its scene was built at, so a target whose shape changes — a browser
/// window being stretched — re-aims instead of stretching.
pub fn aspect_of(size: (u32, u32)) -> f32 {
    size.0 as f32 / size.1.max(1) as f32
}

/// The eye [`camera_view`] looks from.
pub const DEFAULT_EYE: glam::Vec3 = glam::Vec3::new(0.0, 1.2, 3.2);
/// The point [`camera_view`] looks at.
pub const DEFAULT_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.2, 0.0);

/// Camera looking at [`DEFAULT_TARGET`] from [`DEFAULT_EYE`].
///
/// Uses reverse-z infinite perspective matching the built-in pipeline's
/// `CompareFunction::Greater` and `depth_clear = 0.0`.
pub fn camera_view(size: (u32, u32)) -> Camera {
    camera_looking_at(DEFAULT_EYE, DEFAULT_TARGET, size)
}

/// A camera at `eye` looking at `target`, with the same reverse-z infinite
/// perspective as [`camera_view`].
pub fn camera_looking_at(eye: glam::Vec3, target: glam::Vec3, size: (u32, u32)) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect_of(size),
        0.1,
    );
    let view = glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y);
    Camera {
        clip_from_world: projection * view,
        position: eye,
    }
}

/// Re-aim `entity`'s camera at `target` from `eye` for a frame of `size`.
///
/// The scenes whose camera never moves still call this every frame, so their
/// projection keeps the frame's own aspect. It is what a scene does in
/// [`SceneControl::advance`](SceneControl) instead of capturing the size it was
/// built at.
pub fn aim_camera(
    world: &mut World,
    entity: Entity,
    eye: glam::Vec3,
    target: glam::Vec3,
    size: (u32, u32),
) {
    let camera = camera_looking_at(eye, target, size);
    world
        .with_mut::<Camera, _>(entity, |current| *current = camera)
        .expect("the entity carries a camera");
}

/// Unlit options for the ported ECS scenes: vertex colour + instance, no
/// texture, no MSAA, with reverse-z depth.
///
/// The scene's key is built from these — the same options the snapshot tests
/// the scenes came from were drawn with.
pub fn unlit_options(device: &wgpu::Device) -> UnlitOptions {
    use unlit_wgpu::pipeline::UnlitFlags;
    use unlit_wgpu::render_attachments::default_depth_stencil_format;
    let flags =
        UnlitFlags::VERTEX_POSITION | UnlitFlags::VERTEX_COLOR | UnlitFlags::VERTEX_INSTANCE;
    UnlitOptions {
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: default_depth_stencil_format(device),
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        color_target: wgpu::ColorTargetState {
            format: TEST_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        },
        multisample: wgpu::MultisampleState {
            count: 1,
            ..Default::default()
        },
        // The device's own array path survives the variant's flags.
        ..UnlitOptions::standard(device).with_flags(flags)
    }
}

/// Unlit options for a variant that deforms its vertices.
///
/// `joints` adds the joint stream and the joint-matrix binding; `morphs` adds
/// the morph-delta and morph-weight bindings. Both start from
/// [`unlit_options`], so a deformed scene draws the same cube under the same
/// camera as an undeformed one.
pub fn deformation_options(device: &wgpu::Device, joints: bool, morphs: bool) -> UnlitOptions {
    use unlit_wgpu::pipeline::UnlitFlags;
    let mut options = unlit_options(device);
    if joints {
        options.flags |= UnlitFlags::VERTEX_JOINTS;
    }
    if morphs {
        options.flags |= UnlitFlags::MORPH_POSITIONS;
    }
    options
}

/// A rig of `joint_count` joints, all at rest, returned as joint matrices.
///
/// The identity pose deforms nothing: a vertex's weights sum to one, so the
/// weighted sum of identity matrices is the vertex itself.
pub fn rest_pose(joint_count: usize) -> Vec<JointMatrix> {
    vec![glam::Mat4::IDENTITY; joint_count]
}

/// A two-joint skin that bends a mesh about its own base.
///
/// The lower joint is the base and stays at rest; the upper one rotates about
/// the mesh's lowest point, and every vertex is weighted between them by how
/// high it sits, so the mesh bends rather than shears. The weights exercise the
/// four-lane weighted sum the shader computes.
///
/// The joint stream and weights are what a mesh is uploaded with; the matrices
/// are the pose an entity carries, which [`Self::pose`] builds.
pub struct BendSkin {
    /// Four joint indices per vertex: the base and the bending joint.
    pub joints: Vec<[u16; 4]>,
    /// Four weights per vertex, summing to one.
    pub weights: Vec<[f32; 4]>,
    /// The angle the bending joint is rotated by.
    angle: f32,
    /// The height the bending joint pivots about, in the mesh's own space.
    pivot: f32,
}

impl BendSkin {
    /// Skin `positions` to two joints, splitting each vertex by its height.
    pub fn new(positions: &[[f32; 3]], angle: f32) -> Self {
        let (min, max) = positions.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(min, max), position| (min.min(position[1]), max.max(position[1])),
        );
        let height = (max - min).max(f32::EPSILON);
        let (joints, weights) = positions
            .iter()
            .map(|position| {
                let t = ((position[1] - min) / height).clamp(0.0, 1.0);
                ([0u16, 1, 0, 0], [1.0 - t, t, 0.0, 0.0])
            })
            .unzip();
        Self {
            joints,
            weights,
            angle,
            pivot: min,
        }
    }

    /// Replace the bending joint's rotation, which pivots about the mesh's
    /// lowest point rather than its origin.
    pub fn set_angle(&mut self, angle: f32) {
        self.angle = angle;
    }

    /// The pose the two joints are at, ready to attach to an entity.
    pub fn pose(&self) -> SkinPose {
        let mut matrices = rest_pose(2);
        matrices[1] = glam::Mat4::from_translation(glam::Vec3::Y * self.pivot)
            * glam::Mat4::from_rotation_z(self.angle)
            * glam::Mat4::from_translation(glam::Vec3::Y * -self.pivot);
        SkinPose::new(matrices)
    }
}

/// The per-vertex displacement of the two morph targets the deformed scenes
/// use: the first tapers the cube's top to a point, the second widens its base.
pub fn morph_targets(positions: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
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

/// Run `f` on the world's `MeshSource`, which lives on the source entity.
///
/// A shared-world entry point, so the scene's own `&mut World` can
/// reborrow it next to the mutable borrow of the source.
pub fn with_mesh_source<R>(
    world: &World,
    source_entity: Entity,
    f: impl FnOnce(&mut MeshSource, &World) -> R,
) -> R {
    let mut source = world
        .get_mut::<Source>(source_entity)
        .expect("the source entity exists");
    let mesh = source
        .as_mut::<MeshSource>()
        .expect("the source is a MeshSource");
    f(mesh, world)
}

/// Allocate the unit cube through the source, its vertices offset by `offset`
/// in mesh space.
///
/// The offset is baked into the vertices, so two cubes allocated from
/// different offsets draw differently even at the same transform — which is
/// what tells a mesh apart from the one whose pool range it sits next to.
pub fn allocate_offset_cube_mesh(
    world: &World,
    source_entity: Entity,
    key: &UnlitPipelineKey,
    offset: glam::Vec3,
) -> GpuMesh {
    let (positions, uvs, colors, indices) = cube();
    let positions = positions
        .into_iter()
        .map(|position| {
            [
                position[0] + offset.x,
                position[1] + offset.y,
                position[2] + offset.z,
            ]
        })
        .collect::<Vec<_>>();
    with_mesh_source(world, source_entity, |source, world| {
        source.allocate_unlit_mesh(
            world,
            key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    })
}

/// Free `mesh` through the source, releasing its pool ranges.
pub fn remove_mesh(world: &World, source_entity: Entity, mesh: GpuMesh) {
    with_mesh_source(world, source_entity, |source, world| {
        source.remove_mesh(world, mesh)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_without_a_threshold_uses_the_default() {
        let scene = by_id("spin_cube").expect("the default scene exists");
        assert_eq!(scene.min_score, None);
        assert_eq!(scene.min_score(), crate::cli::DEFAULT_MIN_SCORE);
    }

    #[test]
    fn a_scene_with_a_threshold_uses_its_own() {
        let scene = by_id("mesh_topologies").expect("the topology scene exists");
        assert_eq!(scene.min_score, Some(75.0));
        assert_eq!(scene.min_score(), 75.0);
    }

    /// A declared threshold has to be below the default, since the default is
    /// the bar a scene is held to when its pixels are not in question; one at
    /// or above it would say nothing.
    #[test]
    fn a_declared_threshold_is_below_the_default() {
        for scene in SCENES {
            if let Some(min_score) = scene.min_score {
                assert!(
                    (0.0..crate::cli::DEFAULT_MIN_SCORE).contains(&min_score),
                    "`{}` declares {min_score}, which the default already covers",
                    scene.id
                );
            }
        }
    }
}
