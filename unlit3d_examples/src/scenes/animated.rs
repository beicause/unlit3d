//! The animated-grid scene: meshes allocated, recycled and moved across eight
//! frames.
//!
//! Ported from the `ecs_animated_scene_matches_snapshots` test. One world is
//! evolved over several frames: meshes are allocated, some are recycled, the
//! moving ones translate and rotate, and the camera orbits the grid. Every
//! frame is stored as its own snapshot, so the sequence shows what a wrong
//! pooled range, a stale metadata entry or a mispacked instance looks like
//! from frame to frame.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, unlit_options};
use unlit3d::prelude::*;

/// The grid of cells a cube can occupy, in the XZ plane.
const COLUMNS: usize = 3;
const ROWS: usize = 3;
const CELLS: usize = COLUMNS * ROWS;
const CELL_SPACING: f32 = 1.5;

/// Where the camera orbits, and how high above the grid it sits.
const ORBIT_RADIUS: f32 = 4.3;
const ORBIT_HEIGHT: f32 = 2.9;
/// Radians the camera advances per frame.
const ORBIT_STEP: f32 = 0.55;

/// The per-instance base colors the allocations cycle through.
const TINTS: [glam::Vec4; 6] = [
    glam::Vec4::new(1.0, 0.25, 0.2, 1.0),
    glam::Vec4::new(1.0, 0.75, 0.2, 1.0),
    glam::Vec4::new(0.35, 0.9, 0.3, 1.0),
    glam::Vec4::new(0.25, 0.75, 1.0, 1.0),
    glam::Vec4::new(0.55, 0.35, 1.0, 1.0),
    glam::Vec4::new(1.0, 0.4, 0.8, 1.0),
];

/// What a frame changes structurally, on top of the motion every frame applies.
struct Step {
    /// Cells the frame allocates a mesh in.
    allocate: &'static [usize],
    /// Cells the frame retires, before it allocates anything.
    free: &'static [usize],
}

/// The structural change of each frame, as the snapshot test froze it.
const STEPS: [Step; 8] = [
    Step {
        allocate: &[4, 1, 6],
        free: &[],
    },
    Step {
        allocate: &[0, 7],
        free: &[],
    },
    Step {
        allocate: &[],
        free: &[],
    },
    Step {
        allocate: &[2, 5, 8],
        free: &[],
    },
    Step {
        allocate: &[],
        free: &[1],
    },
    Step {
        allocate: &[3],
        free: &[],
    },
    Step {
        allocate: &[],
        free: &[4, 0],
    },
    Step {
        allocate: &[1, 4],
        free: &[],
    },
];

/// The scene: a 3x3 grid of cubes that fills, moves and recycles over eight
/// frames, each stored as its own snapshot.
pub static SCENE: SceneDef = SceneDef {
    id: "ecs_animated",
    title: "Animated grid",
    description: "a grid of cubes filling, moving and recycling over eight frames",
    size: TEST_SIZE,
    frames: STEPS.len() as u32,
    samples: 1,
    depth: true,
    ui: false,
    reproducible_ui: false,
    build,
};

/// Build the animated-grid scene's world content.
fn build(
    world: &mut LocalWorld,
    context: RenderContext,
    _renderer: Entity,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    let key = UnlitPipelineKey::new(unlit_options(&source.device(world)));
    let source_entity = spawn_source(world, source);

    let camera_entity = world.spawn((orbit_camera(0),));

    // One entry per cell, holding the entity and the mesh while it is
    // occupied. Counts allocations, not cells: a cell that is filled twice
    // gets two visibly different meshes.
    let mut cells: Vec<Option<(Entity, GpuMesh)>> = (0..CELLS).map(|_| None).collect();
    let mut allocated = 0usize;

    SceneControl {
        advance: Box::new(move |world, frame, _delta| {
            // The sequence loops, so a windowed run keeps animating.
            let step = &STEPS[frame as usize % STEPS.len()];

            // Retire this frame's cells: the entity goes first, so nothing in
            // the world still names a mesh the renderer is about to free.
            for &cell in step.free {
                let (entity, mesh) = cells[cell].take().expect("a freed cell holds a mesh");
                assert!(world.despawn(entity));
                super::remove_mesh(world, source_entity, mesh);
            }

            for &cell in step.allocate {
                // A cell that still holds a mesh — from an earlier pass over
                // the looped sequence — is retired first, so a mesh never
                // outlives the entity that names it.
                if let Some((entity, mesh)) = cells[cell].take() {
                    assert!(world.despawn(entity));
                    super::remove_mesh(world, source_entity, mesh);
                }
                // The allocation's offset is baked into the vertices, so every
                // mesh draws different geometry even at the same transform.
                let offset = glam::Vec3::splat(0.06 * allocated as f32);
                let mesh = super::allocate_offset_cube_mesh(world, source_entity, &key, offset);
                let entity = world.spawn((
                    cell_transform(cell, frame as usize % STEPS.len()),
                    InstanceColor::new(TINTS[allocated % TINTS.len()]),
                    mesh.clone(),
                    UnlitPipeline::new(key.clone()),
                ));
                cells[cell] = Some((entity, mesh));
                allocated += 1;
            }

            // Move the live meshes, then the camera: the world this frame
            // renders is this frame's.
            for (cell, entry) in cells.iter().enumerate() {
                if let Some((entity, _)) = entry {
                    let transform = cell_transform(cell, frame as usize % STEPS.len());
                    world
                        .with_mut::<Transform, _>(*entity, |current| *current = transform)
                        .expect("a live mesh carries a transform");
                }
            }
            let camera = orbit_camera(frame as usize % STEPS.len());
            world
                .with_mut::<Camera, _>(camera_entity, |current| *current = camera)
                .expect("the camera entity carries a camera");
        }),
        snapshot: Box::new(|frame| {
            // The live count walks the steps up to `frame`, applying each
            // frame's retirements and allocations in order — the same count
            // the snapshot name froze.
            let live = STEPS[..=frame as usize].iter().fold(0usize, |live, step| {
                live - step.free.len() + step.allocate.len()
            });
            let step = &STEPS[frame as usize];
            Some(format!(
                "ecs_animated/frame_{frame:02}_alloc{}_free{}_live{live}.webp",
                step.allocate.len(),
                step.free.len(),
            ))
        }),
    }
}

/// The world-space centre of `cell`.
fn cell_center(cell: usize) -> glam::Vec3 {
    let column = (cell % COLUMNS) as f32 - (COLUMNS as f32 - 1.0) * 0.5;
    let row = (cell / COLUMNS) as f32 - (ROWS as f32 - 1.0) * 0.5;
    glam::Vec3::new(column * CELL_SPACING, 0.2, row * CELL_SPACING)
}

/// The placement a cell's mesh has in `frame`.
///
/// Only every other cell moves: the others keep their placement, so a frame
/// mixes static and moving meshes rather than moving the whole scene.
fn cell_transform(cell: usize, frame: usize) -> Transform {
    let moving = cell.is_multiple_of(2);
    let phase = frame as f32 * 0.9 + cell as f32;
    Transform {
        translation: cell_center(cell)
            + if moving {
                glam::Vec3::Y * 0.35 * phase.sin()
            } else {
                glam::Vec3::ZERO
            },
        rotation: if moving {
            glam::Quat::from_rotation_y(frame as f32 * 0.4 + cell as f32)
        } else {
            glam::Quat::from_rotation_y(cell as f32 * 0.7)
        },
        scale: glam::Vec3::splat(0.34 + 0.03 * (cell % 3) as f32),
    }
}

/// The camera of `frame`, orbiting the grid once over the whole sequence.
fn orbit_camera(frame: usize) -> Camera {
    let angle = frame as f32 * ORBIT_STEP;
    let eye = glam::Vec3::new(
        ORBIT_RADIUS * angle.sin(),
        ORBIT_HEIGHT,
        ORBIT_RADIUS * angle.cos(),
    );
    super::camera_looking_at(
        eye,
        glam::Vec3::new(0.0, 0.2, 0.0),
        TEST_SIZE.0 as f32 / TEST_SIZE.1 as f32,
    )
}
