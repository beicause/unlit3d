//! Unit tests for the KHR_draco_mesh_compression import path.
//!
//! A Draco bitstream is synthesised in memory with the decoder's companion
//! encoder and wrapped in a GLB whose primitive accessors deliberately carry
//! no buffer views (their data lives in the stream). The loader decodes the
//! stream back into ordinary accessors and validates the repaired document,
//! so the tests inspect the resulting streams through the same accessor reader
//! an uncompressed primitive uses. A committed, self-contained fixture covers
//! LitGltf::load without writing any file. No GPU is involved: the streams are
//! inspected before they would reach allocate_lit_mesh.

use super::draco;
use super::streams::MeshStreams;
use draco_core::draco_types::DataType;
use draco_core::encoder_buffer::EncoderBuffer;
use draco_core::encoder_options::EncoderOptions;
use draco_core::geometry_attribute::{GeometryAttributeType, PointAttribute};
use draco_core::geometry_indices::PointIndex;
use draco_core::mesh::Mesh;
use draco_core::mesh_encoder::MeshEncoder;
use serde_json::Value;
use serde_json::json;

const POSITIONS: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
];
const NORMALS: [[f32; 3]; 4] = [[0.0, 0.0, 1.0]; 4];
const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
const COLORS4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0, 0.5],
    [1.0, 1.0, 0.0, 0.25],
];
const COLORS3: [[f32; 3]; 4] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 1.0, 0.0],
];
const JOINTS: [[f32; 4]; 4] = [
    [0.0, 1.0, 2.0, 3.0],
    [1.0, 2.0, 3.0, 0.0],
    [2.0, 3.0, 0.0, 1.0],
    [3.0, 0.0, 1.0, 2.0],
];
const WEIGHTS: [[f32; 4]; 4] = [
    [0.5, 0.3, 0.2, 0.0],
    [0.4, 0.4, 0.1, 0.1],
    [0.7, 0.1, 0.1, 0.1],
    [0.25, 0.25, 0.25, 0.25],
];
const FACES: [[u32; 3]; 2] = [[0, 1, 2], [0, 2, 3]];
/// Per-vertex position displacements for a morph target on the synthetic mesh.
const MORPH_DISPLACEMENTS: [[f32; 3]; 4] = [
    [0.5, 0.0, 0.0],
    [0.0, 0.5, 0.0],
    [0.0, 0.0, 0.5],
    [0.25, 0.25, 0.25],
];

/// A 1x1 RGBA PNG, embedded so the textured variant has a real image to import.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xdf, 0xe0, 0xf0,
    0x1f, 0x00, 0x07, 0x00, 0x02, 0xbf, 0x2b, 0xd7, 0xc7, 0xe2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn push_attribute<const N: usize>(mesh: &mut Mesh, ty: GeometryAttributeType, rows: &[[f32; N]]) {
    let mut attribute = PointAttribute::new();
    attribute.init(ty, N as u8, DataType::Float32, false, rows.len());
    for (index, row) in rows.iter().enumerate() {
        for (component, value) in row.iter().enumerate() {
            attribute
                .buffer_mut()
                .write((index * N + component) * 4, &value.to_le_bytes());
        }
    }
    mesh.add_attribute(attribute);
}

/// Encode the shared square, giving COLOR_0 a caller-chosen width.
/// `method` selects sequential (0) or EdgeBreaker (1) connectivity.
fn encode_draco<const C: usize>(colors: &[[f32; C]], method: i32) -> Vec<u8> {
    let mut mesh = Mesh::new();
    push_attribute(&mut mesh, GeometryAttributeType::Position, &POSITIONS);
    push_attribute(&mut mesh, GeometryAttributeType::Normal, &NORMALS);
    push_attribute(&mut mesh, GeometryAttributeType::TexCoord, &UVS);
    push_attribute(&mut mesh, GeometryAttributeType::Color, colors);
    push_attribute(&mut mesh, GeometryAttributeType::Generic, &JOINTS);
    push_attribute(&mut mesh, GeometryAttributeType::Generic, &WEIGHTS);
    for face in FACES {
        mesh.add_face([
            PointIndex(face[0]),
            PointIndex(face[1]),
            PointIndex(face[2]),
        ]);
    }
    let mut encoder = MeshEncoder::new();
    encoder.set_mesh(mesh);
    let mut options = EncoderOptions::new();
    options.set_global_int("encoding_method", method);
    options.set_attribute_int(0, "quantization_bits", 14);
    options.set_attribute_int(1, "quantization_bits", 10);
    options.set_attribute_int(2, "quantization_bits", 12);
    let mut buffer = EncoderBuffer::new();
    encoder
        .encode(&options, &mut buffer)
        .expect("the synthetic mesh encodes");
    buffer.data().to_vec()
}

/// Wrap a JSON document and its binary chunk into a GLB.
fn glb(json: String, bin: &[u8]) -> Vec<u8> {
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(bin);
    out
}

/// The JSON and binary chunk of a Draco GLB whose accessors have no buffer
/// views, exactly as a compressed primitive declares them. Toggling textured
/// and color_components exercises the UV gate and the three-component colour
/// alpha rule; `morph_targets` adds one positional target whose displacements
/// live in an ordinary accessor, as the extension requires.
fn build_parts(
    textured: bool,
    color_components: u8,
    method: i32,
    morph_targets: bool,
) -> (Value, Vec<u8>) {
    let draco = if color_components == 4 {
        encode_draco(&COLORS4, method)
    } else {
        encode_draco(&COLORS3, method)
    };
    let mut bin = Vec::new();
    bin.extend_from_slice(&draco);
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let png_offset = bin.len();
    bin.extend_from_slice(&PNG);
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let morph_offset = bin.len();
    if morph_targets {
        for displacement in &MORPH_DISPLACEMENTS {
            for value in displacement {
                bin.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    let material = if textured {
        json!({"pbrMetallicRoughness": {
            "baseColorTexture": {"index": 0},
            "baseColorFactor": [1.0, 1.0, 1.0, 1.0]
        }})
    } else {
        json!({"pbrMetallicRoughness": {"baseColorFactor": [1.0, 1.0, 1.0, 1.0]}})
    };
    let color_type = if color_components == 4 {
        "VEC4"
    } else {
        "VEC3"
    };

    let mut root = json!({
        "asset": {"version": "2.0"},
        "extensionsRequired": ["KHR_draco_mesh_compression"],
        "extensionsUsed": ["KHR_draco_mesh_compression"],
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{
            "attributes": {
                "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2,
                "COLOR_0": 3, "JOINTS_0": 4, "WEIGHTS_0": 5
            },
            "indices": 6,
            "mode": 4,
            "material": 0,
            "extensions": {
                "KHR_draco_mesh_compression": {
                    "bufferView": 0,
                    "attributes": {
                        "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2,
                        "COLOR_0": 3, "JOINTS_0": 4, "WEIGHTS_0": 5
                    }
                }
            }
        }]}],
        "materials": [material],
        "accessors": [
            {"componentType": 5126, "count": 4, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"componentType": 5126, "count": 4, "type": "VEC3"},
            {"componentType": 5126, "count": 4, "type": "VEC2"},
            {"componentType": 5126, "count": 4, "type": color_type},
            {"componentType": 5126, "count": 4, "type": "VEC4"},
            {"componentType": 5126, "count": 4, "type": "VEC4"},
            {"componentType": 5123, "count": 6, "type": "SCALAR"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": draco.len()},
            {"buffer": 0, "byteOffset": png_offset, "byteLength": PNG.len()}
        ],
        "buffers": [{"byteLength": bin.len()}],
        "images": [{"bufferView": 1, "mimeType": "image/png"}],
        "samplers": [{}],
        "textures": [{"sampler": 0, "source": 0}]
    });
    if morph_targets {
        root["accessors"].as_array_mut().unwrap().push(json!({
            "bufferView": 2,
            "componentType": 5126,
            "count": 4,
            "type": "VEC3",
            "min": [0.0, 0.0, 0.0],
            "max": [0.5, 0.5, 0.5]
        }));
        root["bufferViews"].as_array_mut().unwrap().push(json!({
            "buffer": 0,
            "byteOffset": morph_offset,
            "byteLength": MORPH_DISPLACEMENTS.len() * 3 * std::mem::size_of::<f32>()
        }));
        root["meshes"][0]["primitives"][0]["targets"] = json!([{"POSITION": 7}]);
    }
    (root, bin)
}

fn build_glb(textured: bool, color_components: u8, method: i32, morph_targets: bool) -> Vec<u8> {
    let (root, bin) = build_parts(textured, color_components, method, morph_targets);
    glb(root.to_string(), &bin)
}

fn streams_of(gltf: &super::LitGltf) -> MeshStreams {
    let mesh = gltf
        .document
        .meshes()
        .next()
        .expect("the document has a mesh");
    let primitive = mesh.primitives().next().expect("the mesh has a primitive");
    super::streams::from_reader(&gltf.buffers, &primitive)
}

fn decoded(textured: bool, color_components: u8, method: i32) -> MeshStreams {
    let bytes = build_glb(textured, color_components, method, false);
    let gltf =
        super::LitGltf::from_bytes(&bytes).expect("a Draco GLB imports as validated accessors");
    streams_of(&gltf)
}

#[test]
fn a_draco_document_becomes_ordinary_accessors() {
    let bytes = build_glb(true, 4, 0, false);
    let gltf =
        super::LitGltf::from_bytes(&bytes).expect("a Draco GLB imports as validated accessors");

    assert!(
        !gltf
            .document
            .extensions_required()
            .any(|extension| extension == draco::EXTENSION),
        "the repaired document no longer requires Draco"
    );
    assert!(
        !gltf
            .document
            .extensions_used()
            .any(|extension| extension == draco::EXTENSION),
        "the repaired document no longer uses Draco"
    );
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
        "the primitive's Draco extension is consumed"
    );
    assert_eq!(
        primitive.mode(),
        gltf::mesh::Mode::Triangles,
        "the decoded triangle list is the mode an ordinary reader draws"
    );
    assert!(
        gltf.document
            .accessors()
            .all(|accessor| accessor.view().is_some()),
        "every accessor now has a buffer view"
    );
}

#[test]
fn a_draco_primitive_decodes_every_stream_it_declares() {
    let streams = decoded(true, 4, 0);

    assert_eq!(streams.positions.len(), POSITIONS.len());
    for (got, want) in streams.positions.iter().zip(POSITIONS.iter()) {
        for component in 0..3 {
            assert!(
                (got[component] - want[component]).abs() < 2.0 / 16384.0,
                "position {got:?} differs from {want:?}"
            );
        }
    }

    let normals = streams.normals.expect("NORMAL is decoded");
    assert_eq!(normals.len(), NORMALS.len());

    let uvs = streams
        .uvs
        .expect("TEXCOORD_0 is decoded for a textured draw");
    assert_eq!(uvs.len(), UVS.len());
    for (got, want) in uvs.iter().zip(UVS.iter()) {
        for component in 0..2 {
            assert!(
                (got[component] - want[component]).abs() < 2.0 / 4096.0,
                "uv {got:?} differs from {want:?}"
            );
        }
    }

    let colors = streams.colors.expect("COLOR_0 is decoded");
    assert_eq!(colors.len(), COLORS4.len());
    for (got, want) in colors.iter().zip(COLORS4.iter()) {
        for component in 0..4 {
            // COLOR_0 is encoded as plain floats, so the ordinary reader's
            // conversion must match glTF's Normalize<u8> for f32 exactly.
            let expected = (want[component].max(0.0) * 255.0) as u8;
            assert_eq!(
                got[component], expected,
                "color {got:?} differs from {want:?}"
            );
        }
    }

    let indices = streams.indices.expect("faces become indices");
    assert_eq!(indices.len(), FACES.len() * 3);
    assert!(indices.iter().all(|&index| index < POSITIONS.len() as u32));

    let joints = streams.joints.expect("JOINTS_0 is decoded");
    let weights = streams.weights.expect("WEIGHTS_0 is decoded");
    assert_eq!(joints.len(), POSITIONS.len());
    assert_eq!(weights.len(), POSITIONS.len());

    assert!(
        streams.morph_deltas.is_none(),
        "a primitive declaring no morph targets uploads no displacements"
    );
}

#[test]
fn a_draco_primitive_uploads_morph_target_displacements() {
    let bytes = build_glb(true, 4, 0, true);
    let gltf = super::LitGltf::from_bytes(&bytes).expect("a Draco GLB with a morph target imports");
    let streams = streams_of(&gltf);
    let morph = streams
        .morph_deltas
        .expect("a compressed primitive uploads its target displacements");

    assert_eq!(morph.target_count, 1);
    assert_eq!(morph.deltas.len(), POSITIONS.len() * 3);
    for (vertex, displacement) in MORPH_DISPLACEMENTS.iter().enumerate() {
        let start = vertex * 3;
        assert_eq!(
            &morph.deltas[start..start + 3],
            &displacement[..],
            "vertex {vertex} keeps its displacement"
        );
    }
}

#[test]
fn a_draco_primitive_without_a_textured_material_skips_uvs() {
    let streams = decoded(false, 4, 0);
    assert!(streams.uvs.is_none(), "an untextured draw uploads no UVs");
    assert_eq!(streams.positions.len(), POSITIONS.len());
}

#[test]
fn a_three_component_color_supplies_opaque_alpha() {
    let streams = decoded(true, 3, 0);
    let colors = streams.colors.expect("COLOR_0 is decoded");
    assert_eq!(colors.len(), COLORS3.len());
    for color in &colors {
        assert_eq!(color[3], u8::MAX);
    }
}

#[test]
fn an_edgebreaker_stream_decodes_like_a_sequential_one() {
    // 3DGarden's models (and gltf-transform's Draco output) use EdgeBreaker
    // connectivity rather than the sequential coder the other tests exercise.
    let streams = decoded(true, 4, 1);
    assert_eq!(streams.positions.len(), POSITIONS.len());
    assert_eq!(
        streams.uvs.expect("UVs survive EdgeBreaker").len(),
        UVS.len()
    );
    assert_eq!(
        streams
            .indices
            .as_ref()
            .expect("faces survive EdgeBreaker")
            .len(),
        FACES.len() * 3
    );
    // EdgeBreaker reorders points, so the streams are checked as a set: every
    // decoded position is one of the four that went in, and the face indices
    // stay inside that set. The real-asset test below pins the exact topology.
    for got in &streams.positions {
        let matched = POSITIONS.iter().any(|want| {
            (0..3).all(|component| (got[component] - want[component]).abs() < 2.0 / 16384.0)
        });
        assert!(matched, "edgebreaker emitted an unknown position {got:?}");
    }
    assert!(
        streams
            .indices
            .as_ref()
            .expect("faces survive EdgeBreaker")
            .iter()
            .all(|&index| index < POSITIONS.len() as u32),
        "edgebreaker face indices stay in range"
    );
}

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
    let compressed = super::LitGltf::from_bytes(&draco_bytes)
        .expect("the real Draco GLB imports as validated accessors");
    let reference =
        super::LitGltf::from_bytes(&plain_bytes).expect("the plain GLB imports normally");

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
fn a_validation_failure_without_draco_is_returned_unchanged() {
    let root = json!({
        "asset": {"version": "2.0"},
        "extensionsRequired": ["KHR_materials_not_supported"],
        "meshes": []
    });
    let bytes = glb(root.to_string(), &[]);
    let error = match super::LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an unsupported extension is rejected"),
    };
    assert!(matches!(error, gltf::Error::Validation(_)));
}

#[test]
fn an_unrelated_validation_failure_on_a_draco_document_is_returned_unchanged() {
    // The extension is declared, but the document has no compressed primitive
    // to repair and its default scene index is out of range. The original
    // validation errors must be returned, including the bad scene.
    let root = json!({
        "asset": {"version": "2.0"},
        "extensionsRequired": ["KHR_draco_mesh_compression"],
        "scene": 1,
        "scenes": [{"nodes": []}],
        "meshes": []
    });
    let bytes = glb(root.to_string(), &[]);
    let error = match super::LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an out-of-range scene is rejected even on a Draco document"),
    };
    let gltf::Error::Validation(errors) = &error else {
        panic!("the original validation error is returned");
    };
    assert_eq!(
        errors.len(),
        2,
        "Draco's own rejection and the bad scene are both reported"
    );
    assert!(
        errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::IndexOutOfBounds)),
        "the out-of-range scene is among the returned errors"
    );
}

#[test]
fn an_unrelated_validation_failure_survives_the_repair() {
    // A compressed primitive is present, so the Draco errors are repaired, but
    // the out-of-range scene is not something Draco explains: re-validating the
    // repaired document must still report it.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["scene"] = json!(1);
    let bytes = glb(root.to_string(), &bin);
    let error = match super::LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an out-of-range scene is rejected after the Draco repair"),
    };
    let gltf::Error::Validation(errors) = &error else {
        panic!("the re-validation error is returned");
    };
    assert!(
        errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::IndexOutOfBounds)),
        "the bad scene is reported after the repair: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::Unsupported)),
        "the Draco extension rejection is gone after the repair: {errors:?}"
    );
}

#[test]
fn a_triangle_strip_primitive_is_normalized_to_triangles() {
    // The extension permits TRIANGLE_STRIP, but Draco always decodes to a
    // triangle list. The repair rewrites the mode so the ordinary reader draws
    // exactly the decoded geometry.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["meshes"][0]["primitives"][0]["mode"] = json!(5);
    let bytes = glb(root.to_string(), &bin);
    let gltf = super::LitGltf::from_bytes(&bytes).expect("a strip-mode Draco GLB imports");
    let primitive = gltf
        .document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .next()
        .unwrap();
    assert_eq!(primitive.mode(), gltf::mesh::Mode::Triangles);
    assert_eq!(
        streams_of(&gltf)
            .indices
            .expect("the strip becomes a triangle list")
            .len(),
        FACES.len() * 3
    );
}

#[test]
fn a_corrupt_draco_stream_returns_an_error() {
    // Truncate the stream's buffer view: decoding must fail with a returned
    // error naming the extension, not panic on the unvalidated document.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["bufferViews"][0]["byteLength"] = json!(4);
    let bytes = glb(root.to_string(), &bin);
    let error = match super::LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("a truncated Draco stream is rejected"),
    };
    let gltf::Error::Validation(errors) = &error else {
        panic!("the decode failure is reported as a validation error");
    };
    assert!(
        errors
            .iter()
            .any(|(path, _)| path.as_str().contains(draco::EXTENSION)),
        "the error names the extension that failed to decode: {errors:?}"
    );
}

#[test]
fn the_file_loader_accepts_a_draco_glb() {
    // The committed fixture is self-contained: no external URIs and its image
    // comes from a buffer view, so loading it exercises the file path without
    // writing any file.
    let gltf = super::LitGltf::load(DRACO_FLOWER).expect("a Draco GLB loads from disk");
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
