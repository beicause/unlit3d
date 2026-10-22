//! The shapes the scene cycles through: their meshes, colliders and textures.
//!
//! A shape kind decides three things at once — what the render world draws,
//! what physics simulates, and what pattern the material wears — so the three
//! live together and a new kind is one match arm per function.

use glam::Vec3;
use unlit3dx::prelude::*;

use crate::render::{OwnedUnlitMeshDesc, WHITE_COLOR, cuboid_mesh, uv_sphere};

/// Every ball's radius, in metres.
pub const BALL_RADIUS: f32 = 0.8;

/// The base colors the balls cycle through, as sRGB bytes.
pub const BALL_COLORS: [[u8; 4]; 4] = [
    [228, 92, 74, 255],
    [96, 178, 92, 255],
    [86, 132, 228, 255],
    [232, 192, 76, 255],
];

/// The pattern textures the shape materials draw from, in pixels per
/// side: big enough that a pattern reads across a metre-wide body,
/// small enough that sixteen of them stay cheap.
pub const PATTERN_SIZE: u32 = 128;

/// One body shape of the scene: the mesh it draws, the collider physics
/// simulates, and the texture pattern its material wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    /// A sphere: a dotted texture over the round mesh.
    Ball,
    /// A box: a checker across the six faces.
    Cube,
    /// A cylinder along y: vertical stripes wrap the wall.
    Cylinder,
    /// A cone pointing up y: horizontal rings stack down the wall.
    Cone,
}

/// Every shape the scene cycles through, in spawn order.
pub const SHAPE_KINDS: [ShapeKind; 4] = [
    ShapeKind::Ball,
    ShapeKind::Cube,
    ShapeKind::Cylinder,
    ShapeKind::Cone,
];

/// The shape kind an index draws from the palette.
pub fn shape_kind(index: usize) -> ShapeKind {
    SHAPE_KINDS[index % SHAPE_KINDS.len()]
}

/// Scale a color's bytes by a factor, for the pattern accents.
fn tinted(color: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (f32::from(color[0]) * factor).round().clamp(0.0, 255.0) as u8,
        (f32::from(color[1]) * factor).round().clamp(0.0, 255.0) as u8,
        (f32::from(color[2]) * factor).round().clamp(0.0, 255.0) as u8,
        color[3],
    ]
}

/// The texture pattern one shape kind wears, tinted with a base color:
/// dots for the ball, a checker for the cube, stripes for the cylinder
/// and rings for the cone. Every pattern puts the accents where the
/// body's silhouette bends, so the tumbling reads as motion.
#[must_use]
pub fn pattern_texture(kind: ShapeKind, color: [u8; 4]) -> Vec<[u8; 4]> {
    let accent = tinted(color, 0.72);
    let size = PATTERN_SIZE;
    let block = match kind {
        // The checker blocks: four per side.
        ShapeKind::Cube => size / 4,
        // The stripe width: eight stripes wrap the wall.
        ShapeKind::Cylinder | ShapeKind::Cone => size / 8,
        // The dot pitch: a six by six grid of dots.
        ShapeKind::Ball => size / 6,
    };
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        for x in 0..size {
            let on = match kind {
                // The cube's checker: blocks alternate both ways.
                ShapeKind::Cube => (x / block + y / block).is_multiple_of(2),
                // The cylinder's stripes: vertical, so they wrap the
                // wall and read as rotation while the body rolls.
                ShapeKind::Cylinder => (x / block).is_multiple_of(2),
                // The cone's rings: horizontal, so they stack down the
                // wall and read as height while the body rolls.
                ShapeKind::Cone => (y / block).is_multiple_of(2),
                ShapeKind::Ball => {
                    // A dot at every block corner, a quarter block
                    // across, so the sphere wears a polka grid.
                    let cx = x % block;
                    let cy = y % block;
                    let half = block / 2;
                    let dx = cx.abs_diff(half);
                    let dy = cy.abs_diff(half);
                    dx * dx + dy * dy < half * half / 2
                }
            };
            pixels.push(if on { accent } else { color });
        }
    }
    pixels
}

/// The mesh one shape kind draws. Every kind is about [`BALL_RADIUS`]
/// across, so every shape shares one visual weight.
#[must_use]
pub fn shape_mesh(kind: ShapeKind) -> OwnedUnlitMeshDesc {
    match kind {
        ShapeKind::Ball => uv_sphere(BALL_RADIUS, 32, 22),
        ShapeKind::Cube => cuboid_mesh(Vec3::splat(BALL_RADIUS * 0.8)),
        ShapeKind::Cylinder => into_desc(uv_cylinder(BALL_RADIUS * 0.72, BALL_RADIUS, 24)),
        ShapeKind::Cone => into_desc(uv_cone(BALL_RADIUS * 0.9, BALL_RADIUS, 24)),
    }
}

/// Lift a raw mesh's arrays into the owned description the render server
/// takes.
fn into_desc(mesh: RawMesh) -> OwnedUnlitMeshDesc {
    let (positions, uvs, colors, indices) = mesh;
    OwnedUnlitMeshDesc {
        positions,
        uvs: Some(uvs),
        colors: Some(colors),
        indices: Some(indices),
    }
}

/// The collider physics simulates for one shape kind, sized to match
/// its mesh.
#[must_use]
pub fn shape_body(kind: ShapeKind) -> BodyShape {
    match kind {
        ShapeKind::Ball => BodyShape::Ball(BALL_RADIUS),
        ShapeKind::Cube => BodyShape::Cuboid(Vec3::splat(BALL_RADIUS * 0.8)),
        ShapeKind::Cylinder => BodyShape::Cylinder {
            half_height: BALL_RADIUS,
            radius: BALL_RADIUS * 0.72,
        },
        ShapeKind::Cone => BodyShape::Cone {
            half_height: BALL_RADIUS,
            radius: BALL_RADIUS * 0.9,
        },
    }
}

/// A mesh as raw arrays: positions, uvs, colors, indices.
pub type RawMesh = (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>);

/// A lathe along the y axis: the side wall walks the rings top to
/// bottom, and the caps close whichever ends ask for it, so a cylinder
/// and a cone share one winding story.
fn lathe(rings: &[(f32, f32); 2], caps: (bool, bool), slices: usize) -> RawMesh {
    let rows = rings.len();
    let ring_len = slices + 1;
    let vertices = ring_len * rows + ring_len * (usize::from(caps.0) + usize::from(caps.1));
    let mut positions = Vec::with_capacity(vertices);
    let mut uvs = Vec::with_capacity(vertices);
    let mut colors = Vec::with_capacity(vertices);
    let mut indices = Vec::with_capacity(
        (rows - 1) * slices * 6 + slices * 3 * (usize::from(caps.0) + usize::from(caps.1)),
    );
    // The side wall: the same outward winding the sphere's quads read,
    // (top, top-next, bottom) and (top-next, bottom-next, bottom)
    // against the outward direction.
    for (row, &(radius, y)) in rings.iter().enumerate() {
        let v = row as f32 / (rows - 1).max(1) as f32;
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            positions.push([radius * cos, y, radius * sin]);
            uvs.push([u, v]);
            colors.push(WHITE_COLOR);
        }
    }
    for row in 0..rows - 1 {
        for slice in 0..slices {
            let top = (row * ring_len + slice) as u32;
            let bottom = top + ring_len as u32;
            indices.extend_from_slice(&[top, top + 1, bottom, top + 1, bottom + 1, bottom]);
        }
    }
    // A cap fans its ring around a centre point. The ring runs with
    // growing angle, which reads clockwise from above, so the top cap
    // (facing +y) walks its fan backwards and the bottom cap forwards.
    let cap = |lathe: &mut RawMesh, y: f32, up: bool| {
        let base = lathe.0.len() as u32;
        let centre = base;
        lathe.0.push([0.0, y, 0.0]);
        lathe.1.push([0.5, 0.5]);
        lathe.2.push(WHITE_COLOR);
        let &(radius, ring_y) = if up {
            rings.first().expect("a top ring")
        } else {
            rings.last().expect("a bottom ring")
        };
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let (sin, cos) = (u * std::f32::consts::TAU).sin_cos();
            lathe.0.push([radius * cos, ring_y, radius * sin]);
            lathe.1.push([0.5 + 0.5 * cos, 0.5 + 0.5 * sin]);
            lathe.2.push(WHITE_COLOR);
        }
        for slice in 0..slices {
            let one = centre + 1 + slice as u32;
            let two = one + 1;
            let fan = if up {
                [centre, two, one]
            } else {
                [centre, one, two]
            };
            lathe.3.extend_from_slice(&fan);
        }
    };
    let mut lathe = (positions, uvs, colors, indices);
    if caps.0 {
        cap(&mut lathe, rings[0].1, true);
    }
    if caps.1 {
        cap(&mut lathe, rings[rows - 1].1, false);
    }
    lathe
}

/// A cylinder along the y axis: flat caps top and bottom, uvs mapped so
/// a texture wraps the wall and spots the caps.
pub fn uv_cylinder(radius: f32, half_height: f32, slices: usize) -> RawMesh {
    lathe(
        &[(radius, half_height), (radius, -half_height)],
        (true, true),
        slices,
    )
}

/// A cone pointing up the y axis: an apex ring collapsed to a point on
/// top, a flat base cap below.
pub fn uv_cone(radius: f32, half_height: f32, slices: usize) -> RawMesh {
    lathe(
        &[(0.0, half_height), (radius, -half_height)],
        (false, true),
        slices,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every triangle of every generated mesh faces outward: the unlit
    /// pipeline culls back faces, so a single inward triangle shows as a
    /// hole in the surface.
    pub(crate) fn assert_faces_outward(name: &str, mesh: &OwnedUnlitMeshDesc) {
        let indices = mesh.indices.as_deref().unwrap_or(&[]);
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|index| Vec3::from(mesh.positions[index as usize]));
            let normal = (b - a).cross(c - a);
            // The sphere's pole rows collapse to a point, so their
            // triangles have zero area and no orientation to check.
            if normal.length_squared() <= f32::EPSILON {
                continue;
            }
            let center = (a + b + c) / 3.0;
            assert!(
                normal.dot(center) > 0.0,
                "{name}: triangle {triangle:?} faces inward",
            );
        }
    }

    /// A test's name for one shape kind.
    fn kind_name(kind: ShapeKind) -> &'static str {
        match kind {
            ShapeKind::Ball => "sphere",
            ShapeKind::Cube => "cube",
            ShapeKind::Cylinder => "cylinder",
            ShapeKind::Cone => "cone",
        }
    }

    #[test]
    fn meshes_face_outward() {
        for kind in SHAPE_KINDS {
            assert_faces_outward(kind_name(kind), &shape_mesh(kind));
        }
    }

    #[test]
    fn patterns_cover_their_canvas() {
        for kind in SHAPE_KINDS {
            for color in BALL_COLORS {
                let pixels = pattern_texture(kind, color);
                assert_eq!(pixels.len(), (PATTERN_SIZE * PATTERN_SIZE) as usize);
                let accents = pixels.iter().filter(|pixel| **pixel != color).count();
                assert!(accents > 0, "{kind:?}: the pattern painted nothing");
                assert!(
                    accents < pixels.len(),
                    "{kind:?}: the pattern left no base color",
                );
            }
        }
    }
}
