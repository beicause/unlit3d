//! The real 3DGarden asset and the file path, both end to end.

use super::fixture::*;
use crate::gltf::LitGltf;
use crate::gltf::draco;

/// The real 3DGarden flower and its gltf-transform-decoded twin, kept in the
/// asset submodule. The twin is the exact same bitstream decoded with the
/// reference decoder, so its streams are the values ours must reproduce.
const DRACO_FLOWER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../unlit3d_asset_files/assets/flower-draco.glb"
);
const PLAIN_FLOWER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../unlit3d_asset_files/assets/flower-plain.glb"
);

#[test]
fn a_real_3dgarden_asset_decodes_like_its_plain_twin() {
    let draco_bytes = std::fs::read(DRACO_FLOWER).expect("the Draco fixture reads");
    let plain_bytes = std::fs::read(PLAIN_FLOWER).expect("the plain fixture reads");
    let compressed = LitGltf::from_bytes(&draco_bytes)
        .expect("the real Draco GLB imports as validated accessors");
    let reference = LitGltf::from_bytes(&plain_bytes).expect("the plain GLB imports normally");

    let decoded = streams_of(&compressed);
    let plain = streams_of(&reference);

    // 1872 vertices and 4332 indices are the counts gltf-transform wrote to
    // the decoded twin; the bitstream must reconstruct the same topology.
    assert_eq!(decoded.positions.len(), 1872);
    assert_eq!(plain.positions.len(), 1872);
    let decoded_indices = decoded.indices.expect("the Draco faces become indices");
    let plain_indices = plain.indices.expect("the plain accessor carries indices");
    assert_eq!(decoded_indices.len(), 4332);
    assert_eq!(decoded_indices, plain_indices, "face order survives Draco");

    let decoded_uvs = decoded.uvs.expect("the textured Draco primitive reads UVs");
    let plain_uvs = plain.uvs.expect("the textured plain primitive reads UVs");
    assert_eq!(decoded_uvs.len(), plain_uvs.len());

    let decoded_normals = decoded.normals.expect("the Draco primitive reads normals");
    let plain_normals = plain.normals.expect("the plain primitive reads normals");
    assert_eq!(decoded_normals.len(), plain_normals.len());

    // Both sides decode the same quantised bitstream, so the floats agree to
    // the decoder's own rounding rather than to a loose visual threshold.
    let mut position_diff = 0.0f32;
    for (got, want) in decoded.positions.iter().zip(plain.positions.iter()) {
        for component in 0..3 {
            position_diff = position_diff.max((got[component] - want[component]).abs());
        }
    }
    assert!(
        position_diff < 1e-3,
        "positions differ from the reference decode by {position_diff}"
    );
    let mut uv_diff = 0.0f32;
    for (got, want) in decoded_uvs.iter().zip(plain_uvs.iter()) {
        for component in 0..2 {
            uv_diff = uv_diff.max((got[component] - want[component]).abs());
        }
    }
    assert!(
        uv_diff < 1e-3,
        "UVs differ from the reference decode by {uv_diff}"
    );
}

#[test]
fn the_file_loader_accepts_a_draco_glb() {
    // The committed fixture is self-contained: no external URIs and its image
    // comes from a buffer view, so loading it exercises the file path without
    // writing any file.
    let gltf = LitGltf::load(DRACO_FLOWER).expect("a Draco GLB loads from disk");
    assert_eq!(gltf.document.meshes().count(), 1);
    let primitive = gltf
        .document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .next()
        .unwrap();
    assert!(
        primitive.extension_value(draco::EXTENSION).is_none(),
        "the file path repairs the document too"
    );
}
