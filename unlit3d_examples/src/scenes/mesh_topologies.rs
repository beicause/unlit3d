//! The mesh-topology scene: every primitive topology, drawn indexed and
//! non-indexed.
//!
//! The other scenes draw triangle lists, so the primitive a pipeline
//! assembles — and the draw call that feeds it — is the one thing none of them
//! pins. This scene draws one patch in each of the five topologies, once from
//! an index buffer and once straight from the vertex buffer, in a 5x2 grid:
//! the top row is indexed, the bottom row draws its vertices in order.
//!
//! The indexed row stores its vertices **shuffled** and restores the draw
//! order through the index buffer, so the two rows draw the same picture only
//! if that buffer is really read: a draw that ignored it would assemble the
//! patch from the storage order instead and show a differently-shaped patch.
//! The rows are therefore compared against each other, and the pair is what
//! carries the evidence.
//!
//! Strips carry a second trap. A strip pipeline has to declare the index width
//! its draw binds, and here that width is the mesh source's own choice — the
//! narrowest format the mesh's vertex count fits, widened while baking in a
//! pool offset on a device without `base_vertex`. The top row's strip cells
//! are what exercise the specializer that derives it from the mesh.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, camera_looking_at, unlit_options};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit3d::prelude::*;

/// The topologies drawn, left to right.
///
/// The top row indexes each one; the bottom row draws its vertices in order.
const TOPOLOGIES: [wgpu::PrimitiveTopology; 5] = [
    wgpu::PrimitiveTopology::PointList,
    wgpu::PrimitiveTopology::LineList,
    wgpu::PrimitiveTopology::LineStrip,
    wgpu::PrimitiveTopology::TriangleList,
    wgpu::PrimitiveTopology::TriangleStrip,
];

/// How many vertices one patch has.
///
/// Six is the shortest count every topology above draws something with: a
/// triangle list needs a multiple of three, a triangle strip needs at least
/// three, and a line list needs an even count. It is also what makes the patch
/// a zigzag with a readable shape in each topology.
const PATCH_VERTICES: usize = 6;

/// The patch in its own space, in draw order.
///
/// The shape is chosen so every topology draws something a reader can tell
/// apart: a point list draws six dots down it, a line list three separate
/// strokes, a line strip one connected zigzag, a triangle list two detached
/// triangles and a triangle strip a filled band.
///
/// Every consecutive triple winds the same way, because the triangle list
/// culls back faces: two triangles of opposite winding would draw as one, and
/// the snapshot would no longer show what the topology does.
const PATCH: [[f32; 2]; PATCH_VERTICES] = [
    [-1.0, -1.0],
    [0.0, -1.0],
    [-1.0, 1.0],
    [0.0, 1.0],
    [1.0, -1.0],
    [1.0, 1.0],
];

/// The colour of each patch vertex, in draw order.
///
/// Distinct per vertex so a shuffle reads as one: a colour rides on its
/// vertex, so a draw that ignored the index buffer would tie each colour to
/// the wrong corner as well.
const VERTEX_COLORS: [[u8; 4]; PATCH_VERTICES] = [
    [255, 80, 80, 255],
    [255, 200, 60, 255],
    [120, 230, 90, 255],
    [70, 200, 240, 255],
    [110, 130, 255, 255],
    [230, 100, 220, 255],
];

/// The draw vertex each storage slot holds in the indexed row.
///
/// A permutation of `0..PATCH_VERTICES`: the indexed row uploads vertex
/// `INDEX_ORDER[slot]` of [`PATCH`] into `slot`, so its vertex buffer is in a
/// different order from the one it draws in.
const INDEX_ORDER: [usize; PATCH_VERTICES] = [3, 0, 5, 1, 4, 2];

/// The storage slots the non-indexed row draws, which is the draw order
/// itself: a draw with no index buffer reads its vertices in the order they
/// are stored.
const DRAW_ORDER: [usize; PATCH_VERTICES] = [0, 1, 2, 3, 4, 5];

/// The distance between neighbouring columns, in world units.
const COLUMN_SPACING: f32 = 0.9;

/// How far each row's patches sit above and below the centre, in world units.
const ROW_OFFSET: f32 = 0.78;

/// Half of a patch's width and height, in world units.
///
/// Small enough that neighbouring patches stay clear of each other: the
/// columns are [`COLUMN_SPACING`] apart and the rows twice [`ROW_OFFSET`],
/// while a patch is only this wide.
const PATCH_HALF_WIDTH: f32 = 0.38;

/// Half of a patch's height, in world units.
///
/// Taller than it is wide because a cell is: five columns across the frame and
/// two rows down it leave each cell about twice as tall as it is wide, and a
/// patch fills its cell without touching its neighbours.
const PATCH_HALF_HEIGHT: f32 = 0.62;

/// How far in front of the patches the camera sits, in world units.
///
/// A perspective camera looking straight down `-Z` from here: every patch lies
/// in the `z = 0` plane, so they all project at the same scale and the grid
/// stays even on screen.
const CAMERA_DISTANCE: f32 = 3.0;

/// The scene: one patch per topology, indexed and non-indexed.
pub static SCENE: SceneDef = SceneDef {
    id: "mesh_topologies",
    title: "Mesh topologies",
    description: "every primitive topology, indexed and non-indexed",
    size: TEST_SIZE,
    frames: 1,
    step_seconds: None,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// The unlit options one topology draws with.
///
/// The topology is part of the options, so it is part of the pipeline key: a
/// family variant is compiled per topology, which is why an entity cannot
/// share a key with a different one.
///
/// Culling is off, unlike the other scenes. What this scene pins is the
/// primitive each pipeline assembles, and culling would hide half of it: a
/// strip alternates the winding of the triangles it composes, so a back-face
/// test would drop every other one and the snapshot could no longer show the
/// topology whole. Pinning culling is the opaque scenes' job.
fn options_for(device: &wgpu::Device, topology: wgpu::PrimitiveTopology) -> UnlitOptions {
    let mut options = unlit_options(device);
    options.primitive.topology = topology;
    options.primitive.cull_mode = None;
    options
}

/// The world position of the cell at `column` in `row`, in world units.
fn cell_center(column: usize, row: usize) -> [f32; 2] {
    let last = TOPOLOGIES.len() as f32 - 1.0;
    let x = (column as f32 - last * 0.5) * COLUMN_SPACING;
    let y = if row == 0 { ROW_OFFSET } else { -ROW_OFFSET };
    [x, y]
}

/// Place patch vertex `vertex` into the cell whose centre is `center`.
fn place(vertex: usize, center: [f32; 2]) -> [f32; 3] {
    let [x, y] = PATCH[vertex];
    [
        center[0] + x * PATCH_HALF_WIDTH,
        center[1] + y * PATCH_HALF_HEIGHT,
        0.0,
    ]
}

/// The index buffer the indexed row binds.
///
/// Draw vertex `i` is stored in slot `slot` where `INDEX_ORDER[slot] == i`, so
/// the buffer names that slot in draw order and nothing else. It is an
/// `u32` list like any other caller's; the mesh source picks the width it
/// uploads them as.
fn index_buffer() -> Vec<u32> {
    (0..PATCH_VERTICES)
        .map(|draw_vertex| {
            INDEX_ORDER
                .iter()
                .position(|&stored| stored == draw_vertex)
                .expect("the storage order is a permutation of the draw order") as u32
        })
        .collect()
}

/// Build the topology scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    let device = source.device(world);
    let source_entity = spawn_source(world, source);

    // One key per topology, reused by that topology's two cells: an indexed
    // and a non-indexed draw share a pipeline wherever the topology cannot
    // tell them apart, and a strip is the one that can, through the index
    // width the specializer reads off the mesh.
    let keys: Vec<UnlitPipelineKey> = TOPOLOGIES
        .iter()
        .map(|&topology| UnlitPipelineKey::new(options_for(&device, topology)))
        .collect();
    let indices = index_buffer();

    world.spawn((camera_looking_at(
        glam::Vec3::new(0.0, 0.0, CAMERA_DISTANCE),
        glam::Vec3::ZERO,
        TEST_SIZE.0 as f32 / TEST_SIZE.1 as f32,
    ),));

    let mut entities = Vec::new();
    for (column, key) in keys.iter().enumerate() {
        for row in 0..2 {
            // The indexed row stores the shuffled order; the bottom row has no
            // index buffer to reorder with, so it stores what it draws.
            let stored: &[usize] = if row == 0 { &INDEX_ORDER } else { &DRAW_ORDER };
            let center = cell_center(column, row);
            let positions: Vec<[f32; 3]> =
                stored.iter().map(|&vertex| place(vertex, center)).collect();
            let colors: Vec<[u8; 4]> = stored.iter().map(|&vertex| VERTEX_COLORS[vertex]).collect();
            // The variant reads no UVs, but every other scene hands the mesh
            // one per vertex, and a slice of the right length keeps the
            // channels describing the same vertices.
            let uvs: Vec<[f32; 2]> = (0..PATCH_VERTICES).map(|_| [0.0, 0.0]).collect();
            let mesh = super::with_mesh_source(world, source_entity, |source, world| {
                source.allocate_unlit_mesh(
                    world,
                    key,
                    UnlitMeshDesc {
                        positions: &positions,
                        uvs: Some(&uvs),
                        colors: Some(&colors),
                        indices: (row == 0).then_some(indices.as_slice()),
                        ..Default::default()
                    },
                )
            });
            entities.push((column, mesh));
        }
    }

    for (column, mesh) in entities {
        world.spawn((mesh, UnlitPipeline::new(keys[column].clone())));
    }

    SceneControl {
        // Static: one frame, and nothing about it animates.
        advance: Box::new(|_world, _frame, _delta| {}),
        snapshot: Box::new(|frame| (frame == 0).then(|| "mesh_topologies.webp".to_owned())),
    }
}
