//! What the conversion and the booleans promise.

use super::boolean::{BooleanOp, boolean};
use super::convert::{from_mesh, from_mesh_robust, streams};
use super::{Error, Manifold};
use crate::lit::OwnedLitMeshDesc;
use manifold_rust::linalg::Vec3;

/// A cube stated as streams rather than built by the kernel, so the import path
/// is what the test exercises.
///
/// Eight corners, twelve triangles, wound counter-clockwise seen from outside.
fn cube_mesh() -> OwnedLitMeshDesc {
    let positions = [
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ];
    let indices = [
        0, 2, 1, 0, 3, 2, // -z
        4, 5, 6, 4, 6, 7, // +z
        0, 1, 5, 0, 5, 4, // -y
        3, 7, 6, 3, 6, 2, // +y
        0, 4, 7, 0, 7, 3, // -x
        1, 2, 6, 1, 6, 5, // +x
    ];
    OwnedLitMeshDesc {
        positions: positions.to_vec(),
        indices: Some(indices.to_vec()),
        ..Default::default()
    }
}

/// The kernel's own cube, as a solid to compare against.
fn cube_solid() -> Manifold {
    Manifold::cube(Vec3::new(2.0, 2.0, 2.0), true)
}

#[test]
fn a_closed_mesh_becomes_a_solid_of_the_same_volume() {
    let solid = from_mesh(cube_mesh()).expect("the cube is a closed mesh");
    assert_eq!(solid.num_tri(), 12);
    assert!((solid.volume() - 8.0).abs() < 1e-4, "{}", solid.volume());
}

#[test]
fn a_triangle_soup_welds_into_a_solid_only_where_the_corners_meet() {
    let indexed = cube_mesh();
    let indices = indexed.indices.expect("the cube is indexed");
    let mesh = OwnedLitMeshDesc {
        positions: indices
            .iter()
            .map(|&index| indexed.positions[index as usize])
            .collect(),
        ..Default::default()
    };
    assert_eq!(
        mesh.positions.len(),
        36,
        "twelve triangles of three vertices"
    );
    // Shared corners are what make a surface manifold, and stating triangles
    // by position states each corner once per triangle. The strict import
    // therefore refuses the mesh, which is what the robust one repairs.
    let Err(error) = from_mesh(mesh.clone()) else {
        panic!("a triangle soup has no shared corners");
    };
    assert!(matches!(error, Error::Rejected { .. }), "{error:?}");
    let solid = from_mesh_robust(mesh).expect("the soup welds into a closed surface");
    assert_eq!(solid.num_tri(), 12);
}

#[test]
fn a_mesh_whose_vertex_count_is_not_a_whole_number_of_triangles_is_refused() {
    let indexed = cube_mesh();
    let indices = indexed.indices.expect("the cube is indexed");
    let mut mesh = OwnedLitMeshDesc {
        positions: indices
            .iter()
            .map(|&index| indexed.positions[index as usize])
            .collect(),
        ..Default::default()
    };
    mesh.positions.pop();
    let Err(error) = from_mesh(mesh) else {
        panic!("thirty-five vertices are not a whole number of triangles");
    };
    assert!(
        matches!(error, Error::RaggedIndices { count: 35 }),
        "{error:?}"
    );
}

#[test]
fn a_mesh_with_no_vertices_is_refused() {
    let Err(error) = from_mesh(OwnedLitMeshDesc::default()) else {
        panic!("an empty mesh has no solid");
    };
    assert!(matches!(error, Error::EmptyInput), "{error:?}");
}

/// One way of setting a channel a solid cannot hold.
type ChannelSetter = fn(&mut OwnedLitMeshDesc);

#[test]
fn every_channel_a_solid_cannot_hold_is_refused_by_name() {
    fn uvs(mesh: &mut OwnedLitMeshDesc) {
        mesh.uvs = Some(vec![[0.0; 2]; 8]);
    }
    fn colors(mesh: &mut OwnedLitMeshDesc) {
        mesh.colors = Some(vec![[0; 4]; 8]);
    }
    fn normals(mesh: &mut OwnedLitMeshDesc) {
        mesh.normals = Some(vec![[0.0; 3]; 8]);
    }
    fn joints(mesh: &mut OwnedLitMeshDesc) {
        mesh.joints = Some(vec![[0; 4]; 8]);
    }
    fn morph_deltas(mesh: &mut OwnedLitMeshDesc) {
        mesh.morph_deltas = Some(crate::mesh::MorphDeltas {
            deltas: vec![0.0; 8 * 3],
            target_count: 1,
        });
    }
    let cases: [(ChannelSetter, &str); 5] = [
        (uvs, "uvs"),
        (colors, "colors"),
        (normals, "normals"),
        (joints, "joints"),
        (morph_deltas, "morph_deltas"),
    ];
    for (set, channel) in cases {
        let mut mesh = cube_mesh();
        set(&mut mesh);
        let Err(error) = from_mesh(mesh) else {
            panic!("a solid holds positions and indices alone");
        };
        match error {
            Error::UnsupportedChannel { channel: named } => assert_eq!(named, channel),
            other => panic!("expected the {channel} channel to be refused, got {other:?}"),
        }
    }
}

#[test]
fn an_open_surface_is_refused_as_a_solid() {
    let positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut mesh = OwnedLitMeshDesc {
        positions,
        indices: Some(vec![0, 1, 2]),
        ..Default::default()
    };
    // Three vertices and one triangle are below the four the kernel requires of
    // a solid at all, which is the refusal this states.
    let Err(error) = from_mesh(mesh.clone()) else {
        panic!("one triangle is not a solid");
    };
    assert!(matches!(error, Error::Rejected { .. }), "{error:?}");

    // A soup of the same triangle is refused for its own reason.
    mesh.indices = None;
    let Err(error) = from_mesh_robust(mesh) else {
        panic!("one triangle is not a closed soup");
    };
    assert!(matches!(error, Error::Rejected { .. }), "{error:?}");
}

#[test]
fn a_difference_cuts_a_solid_rather_than_leaving_it_whole() {
    let block = cube_solid();
    let bore = Manifold::sphere(0.75, 32);
    let bored = boolean(&block, &bore, BooleanOp::Difference).expect("the sphere meets the block");
    assert!(bored.volume() < block.volume());
    assert!(bored.volume() > 0.0);
    // A cut surface closes: the mesh exports without an open edge.
    let mesh = streams(&bored).expect("the result is a solid");
    assert!(!mesh.positions.is_empty());
}

#[test]
fn an_intersection_of_solids_that_do_not_meet_answers_nothing() {
    let block = cube_solid();
    let far = Manifold::cube(Vec3::splat(2.0), true).translate(Vec3::new(100.0, 0.0, 0.0));
    let Err(error) = boolean(&block, &far, BooleanOp::Intersection) else {
        panic!("solids that do not meet intersect into nothing");
    };
    assert!(matches!(error, Error::EmptyResult), "{error:?}");
}

#[test]
fn a_boolean_over_an_empty_solid_is_a_mistake_rather_than_an_answer() {
    let block = cube_solid();
    let empty = Manifold::cube(Vec3::new(0.0, 0.0, 0.0), true);
    let Err(error) = boolean(&block, &empty, BooleanOp::Union) else {
        panic!("an empty input is a mistake rather than an answer");
    };
    assert!(matches!(error, Error::EmptyInput), "{error:?}");
}

#[test]
fn a_solid_exports_positions_alone_by_default() {
    let mesh = streams(&cube_solid()).expect("a cube exports");
    assert_eq!(mesh.positions.len(), 8);
    assert_eq!(mesh.indices.as_ref().map(Vec::len), Some(36));
    assert!(mesh.normals.is_none(), "the kernel wrote no normals");
    assert!(mesh.uvs.is_none());
    assert!(mesh.colors.is_none());
}

#[test]
fn a_solid_whose_normals_were_computed_exports_them() {
    let solid = cube_solid().calculate_normals(0, 30.0);
    let mesh = streams(&solid).expect("a cube with normals exports");
    let normals = mesh.normals.expect("the normals slot was filled");
    assert_eq!(normals.len(), mesh.positions.len());
    for normal in &normals {
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        assert!(
            (length - 1.0).abs() < 1e-5,
            "a normal is a unit vector: {length}"
        );
    }
}

#[test]
fn a_crease_angle_under_the_angle_between_faces_splits_them() {
    // A cube's faces meet at right angles, so a crease angle below ninety
    // degrees splits every shared corner into one vertex per face, and one
    // above it shares a single smoothed vertex per corner.
    let sharp = streams(&cube_solid().calculate_normals(0, 0.0)).expect("exports");
    let smooth = streams(&cube_solid().calculate_normals(0, 180.0)).expect("exports");
    assert!(
        sharp.positions.len() > smooth.positions.len(),
        "a sharper crease keeps more vertices apart: {} against {}",
        sharp.positions.len(),
        smooth.positions.len()
    );
}

#[test]
fn a_solid_carrying_a_property_layout_this_module_cannot_read_is_refused() {
    let solid = cube_solid().set_properties(4, |props, position, _| {
        props[0] = position.x;
        props[1] = position.y;
        props[2] = position.z;
        props[3] = 1.0;
    });
    let Err(error) = streams(&solid) else {
        panic!("four properties are neither of the known layouts");
    };
    assert!(
        matches!(error, Error::UnknownChannels { num_prop: 7 }),
        "{error:?}"
    );
}

#[test]
fn a_solid_with_no_triangles_is_refused_rather_than_uploaded() {
    // A solid subtracted from itself removes everything, which the export
    // refuses because a pooled mesh covers at least one vertex.
    let block = Manifold::cube(Vec3::splat(1.0), true);
    let gone = block.difference(&block);
    assert!(gone.is_empty());
    let Err(error) = streams(&gone) else {
        panic!("an empty solid has no streams");
    };
    assert!(matches!(error, Error::EmptyResult), "{error:?}");
}

#[test]
fn the_same_boolean_twice_states_the_same_mesh() {
    let block = cube_solid();
    let bore = Manifold::sphere(0.75, 32);
    let once = boolean(&block, &bore, BooleanOp::Difference).expect("the sphere meets the block");
    let twice = boolean(&block, &bore, BooleanOp::Difference).expect("the sphere meets the block");
    let (a, b) = (
        streams(&once).expect("exports"),
        streams(&twice).expect("exports"),
    );
    assert_eq!(a.positions, b.positions);
    assert_eq!(a.indices, b.indices);
}
