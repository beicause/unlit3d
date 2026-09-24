//! Multi-frame snapshot coverage for the ECS rendering path.
//!
//! One scene is evolved over several frames: meshes are allocated, some are
//! recycled, the moving ones translate and rotate, and the camera orbits the
//! grid. Every frame is stored as its own snapshot, so the sequence shows what
//! a wrong pooled range, a stale metadata entry or a mispacked instance looks
//! like from frame to frame — a draw that reads another mesh's range paints a
//! different cube exactly in the frames that reuse the range, which a single
//! snapshot of a static scene cannot show.

pub mod common;

use common::*;
use unlit3d::prelude::*;

/// The frames rendered, one snapshot each.
const FRAMES: usize = 8;

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
///
/// The cube's vertex colors are a position-derived gradient, so a flat tint
/// per instance is what tells one cell's cube from its neighbours and one
/// allocation from the next.
const TINTS: [glam::Vec4; 6] = [
    glam::Vec4::new(1.0, 0.25, 0.2, 1.0),
    glam::Vec4::new(1.0, 0.75, 0.2, 1.0),
    glam::Vec4::new(0.35, 0.9, 0.3, 1.0),
    glam::Vec4::new(0.25, 0.75, 1.0, 1.0),
    glam::Vec4::new(0.55, 0.35, 1.0, 1.0),
    glam::Vec4::new(1.0, 0.4, 0.8, 1.0),
];

/// The tint of the `allocated`-th mesh the scene allocates.
fn tint(allocated: usize) -> InstanceColor {
    InstanceColor::new(TINTS[allocated % TINTS.len()])
}

/// What a frame changes structurally, on top of the motion every frame applies.
struct Step {
    /// Cells the frame allocates a mesh in.
    allocate: &'static [usize],
    /// Cells the frame retires, before it allocates anything.
    free: &'static [usize],
}

/// The structural change of each frame.
///
/// The grid fills from its centre outwards while the pool grows, then a cell
/// is retired and its ranges are handed to the mesh allocated right after it,
/// then two more are retired and both ranges are taken again on the last
/// frame.
const STEPS: [Step; FRAMES] = [
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
    camera_looking_at(
        eye,
        glam::Vec3::new(0.0, 0.2, 0.0),
        WIDTH as f32 / HEIGHT as f32,
    )
}

/// The animated scene renders one snapshot per frame, and every frame differs
/// from the one before it.
#[test]
fn ecs_animated_scene_matches_snapshots() {
    let ctx = Ctx::headless();
    let mut world = LocalWorld::new();
    let gpu = TestGpu::new(&mut world, &ctx);
    let key = gpu.key.clone();
    let camera_entity = world.spawn((orbit_camera(0),));
    let target = gpu.bind_offscreen_target(&world, "test::ecs_animated");

    // One entry per cell, holding the entity and the mesh while it is
    // occupied.
    let mut cells: Vec<Option<(Entity, GpuMesh)>> = (0..CELLS).map(|_| None).collect();
    // Counts allocations, not cells: a cell that is filled twice gets two
    // visibly different meshes, so a draw reading the range of the mesh it
    // replaced paints a different cube.
    let mut allocated = 0usize;
    let mut previous: Option<Vec<u8>> = None;

    for (frame, step) in STEPS.iter().enumerate() {
        // Retire this frame's cells: the entity goes first, so nothing in the
        // world still names a mesh the renderer is about to free.
        for &cell in step.free {
            let (entity, mesh) = cells[cell].take().expect("a freed cell holds a mesh");
            assert!(world.despawn(entity));
            gpu.remove_mesh(&world, mesh);
        }

        for &cell in step.allocate {
            assert!(cells[cell].is_none(), "cell {cell} is already occupied");
            // The allocation's offset is baked into the vertices, so every
            // mesh draws different geometry even at the same transform.
            let offset = glam::Vec3::splat(0.06 * allocated as f32);
            let mesh = gpu.allocate_offset_cube_mesh(&world, offset);
            let entity = world.spawn((
                cell_transform(cell, frame),
                tint(allocated),
                mesh.clone(),
                UnlitPipeline::new(key.clone()),
            ));
            cells[cell] = Some((entity, mesh));
            allocated += 1;
        }

        // Move the live meshes, then the camera: the world this frame renders
        // is this frame's.
        for (cell, entry) in cells.iter().enumerate() {
            if let Some((entity, _)) = entry {
                let transform = cell_transform(cell, frame);
                world
                    .with_mut::<Transform, _>(*entity, |current| *current = transform)
                    .expect("a live mesh carries a transform");
            }
        }
        let camera = orbit_camera(frame);
        world
            .with_mut::<Camera, _>(camera_entity, |current| *current = camera)
            .expect("the camera entity carries a camera");

        gpu.render(&world);

        let pixels = Frame {
            rgba: read_texture_bytes(&ctx, &target, WIDTH, HEIGHT, texel_bytes(&target)),
            width: WIDTH,
            height: HEIGHT,
        };

        // What the snapshot may freeze: the frame shows the grid, and it has
        // moved on from the frame before it. Without this a blank frame or a
        // frozen one would be stored as the reference.
        let covered = count_pixels_off_background(&pixels, CLEAR, 12);
        assert!(
            covered > (WIDTH * HEIGHT) as usize / 100,
            "frame {frame} should show the grid, got {covered} lit pixels"
        );
        if let Some(previous) = &previous {
            assert_ne!(
                &pixels.rgba, previous,
                "frame {frame} should differ from the one before it"
            );
        }
        previous = Some(pixels.rgba.clone());

        // The name carries what the frame did to the scene, so a stored
        // reference says what it should look like without reopening the test.
        let live = cells.iter().filter(|cell| cell.is_some()).count();
        let name = format!(
            "ecs_animated/frame_{frame:02}_alloc{}_free{}_live{live}.webp",
            step.allocate.len(),
            step.free.len(),
        );
        assert_image_snapshot(&name, &pixels, WIDTH, HEIGHT);
    }
}
