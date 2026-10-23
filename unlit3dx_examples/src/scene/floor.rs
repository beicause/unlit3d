//! The ground the balls land on: one fixed body in physics, one instance in
//! the render world, wearing a checker so motion reads against it.
//!
//! The floor is the scene's only fixed body, and the only instance that never
//! moves, so it is built once and then left alone: the ground is queued before
//! anything can drop on it, and afterwards there is nothing to do. The
//! instance is built by one job in the render world, so the mesh, the checker
//! and the instance that names them need no round trip between them.

use glam::Vec3;
use unlit_ecs::World;
use unlit3d::components::Transform;
use unlit3dx::prelude::*;

use super::Scene;
use crate::render::{
    allocate_mesh, allocate_rgba_material, cuboid_mesh, instance_of, linear_sampler,
};

/// The ground box's half extents, in metres.
pub const GROUND_HALF: Vec3 = Vec3::new(12.0, 0.5, 12.0);

/// Where the ground box sits; its top face is the y = 0 plane.
pub const GROUND_POSITION: Vec3 = Vec3::new(0.0, -0.5, 0.0);

/// The ground's base color, as sRGB bytes.
pub const GROUND_COLOR: [u8; 4] = [126, 130, 138, 255];

/// The checker's darker square, as sRGB bytes.
pub const GROUND_CHECKER_DARK: [u8; 4] = [88, 92, 100, 255];

/// The ground texture's edge length in pixels; the checker draws eight
/// squares per side, so each square is a block of
/// `GROUND_CHECKER_SIZE / 8` pixels.
pub const GROUND_CHECKER_SIZE: u32 = 256;

/// The checker squares per texture side.
const GROUND_CHECKER_SQUARES: u32 = 8;

/// The friction the ground gives the balls.
const GROUND_FRICTION: f32 = 0.9;

/// The ground's checker texture: `GROUND_CHECKER_SQUARES` squares per
/// side, the light ones in [`GROUND_COLOR`] and the dark ones in
/// [`GROUND_CHECKER_DARK`].
#[must_use]
pub fn checker_ground() -> Vec<[u8; 4]> {
    let block = GROUND_CHECKER_SIZE / GROUND_CHECKER_SQUARES;
    let mut pixels = Vec::with_capacity((GROUND_CHECKER_SIZE * GROUND_CHECKER_SIZE) as usize);
    for y in 0..GROUND_CHECKER_SIZE {
        for x in 0..GROUND_CHECKER_SIZE {
            let square = (x / block + y / block).is_multiple_of(2);
            pixels.push(if square {
                GROUND_COLOR
            } else {
                GROUND_CHECKER_DARK
            });
        }
    }
    pixels
}

/// Put the ground under the scene: one fixed body in physics, one instance in
/// the render world.
///
/// Nothing waits for the instance: one that arrives a frame late is simply a
/// frame without a floor.
pub fn spawn_floor(scene: &Scene) {
    if let Err(error) = scene.physics.send(Box::new(|world: &mut World| {
        spawn_fixed_cuboid(world, GROUND_POSITION, GROUND_HALF, GROUND_FRICTION);
    })) {
        log::error!("the physics world refused the ground ({error:?})");
    }
    if let Err(error) = scene.render.send(ground_job()) {
        log::error!("the render world refused the ground ({error:?})");
    }
}

/// The job that builds the ground's instance in the render world.
fn ground_job() -> Job {
    Box::new(|world: &mut World| {
        let mesh = allocate_mesh(world, &cuboid_mesh(GROUND_HALF));
        let material = allocate_rgba_material(
            world,
            (GROUND_CHECKER_SIZE, GROUND_CHECKER_SIZE),
            &checker_ground(),
            linear_sampler(),
        );
        instance_of(
            world,
            mesh,
            material,
            Transform {
                translation: GROUND_POSITION,
                ..Default::default()
            },
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::cuboid_mesh;
    use crate::scene::shapes::tests::assert_faces_outward;

    /// The ground's box faces outward like every other mesh: the unlit
    /// pipeline culls back faces, so an inward face would show the floor as a
    /// hole.
    #[test]
    fn the_ground_mesh_faces_outward() {
        assert_faces_outward("cuboid", &cuboid_mesh(GROUND_HALF));
    }

    /// The checker alternates both ways: a texture of one color would draw a
    /// flat floor, and the tumbling balls would read as sliding.
    #[test]
    fn the_checker_alternates_both_ways() {
        let block = GROUND_CHECKER_SIZE / GROUND_CHECKER_SQUARES;
        let pixels = checker_ground();
        let at = |x: u32, y: u32| pixels[(y * GROUND_CHECKER_SIZE + x) as usize];
        assert_ne!(at(0, 0), at(block, 0));
        assert_ne!(at(0, 0), at(0, block));
        assert_eq!(at(0, 0), at(block, block));
    }
}
