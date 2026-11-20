//! The synthetic Draco bitstream, the GLB that wraps it, and the readers the
//! tests inspect the repaired document through.
//!
//! Every test builds the same square mesh with the decoder's companion encoder,
//! so the attributes a stream can carry are stated once here rather than per
//! test.

use crate::gltf::LitGltf;
use crate::gltf::streams;
use crate::gltf::streams::MeshStreams;
use crate::gltf::tests::documents::PNG;
use draco_core::draco_types::DataType;
use draco_core::encoder_buffer::EncoderBuffer;
use draco_core::encoder_options::EncoderOptions;
use draco_core::geometry_attribute::{GeometryAttributeType, PointAttribute};
use draco_core::geometry_indices::PointIndex;
use draco_core::mesh::Mesh;
use draco_core::mesh_encoder::MeshEncoder;
use serde_json::Value;
use serde_json::json;

pub(super) const POSITIONS: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
];
pub(super) const NORMALS: [[f32; 3]; 4] = [[0.0, 0.0, 1.0]; 4];
pub(super) const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
pub(super) const COLORS4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0, 0.5],
    [1.0, 1.0, 0.0, 0.25],
];
pub(super) const COLORS3: [[f32; 3]; 4] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 1.0, 0.0],
];
pub(super) const JOINTS: [[f32; 4]; 4] = [
    [0.0, 1.0, 2.0, 3.0],
    [1.0, 2.0, 3.0, 0.0],
    [2.0, 3.0, 0.0, 1.0],
    [3.0, 0.0, 1.0, 2.0],
];
pub(super) const WEIGHTS: [[f32; 4]; 4] = [
    [0.5, 0.3, 0.2, 0.0],
    [0.4, 0.4, 0.1, 0.1],
    [0.7, 0.1, 0.1, 0.1],
    [0.25, 0.25, 0.25, 0.25],
];
pub(super) const FACES: [[u32; 3]; 2] = [[0, 1, 2], [0, 2, 3]];
/// Per-vertex position displacements for a morph target on the synthetic mesh.
pub(super) const MORPH_DISPLACEMENTS: [[f32; 3]; 4] = [
    [0.5, 0.0, 0.0],
    [0.0, 0.5, 0.0],
    [0.0, 0.0, 0.5],
    [0.25, 0.25, 0.25],
];

pub(super) fn make_attribute<const N: usize>(
    ty: GeometryAttributeType,
    rows: &[[f32; N]],
) -> PointAttribute {
    let mut attribute = PointAttribute::new();
    attribute.init(ty, N as u8, DataType::Float32, false, rows.len());
    for (index, row) in rows.iter().enumerate() {
        for (component, value) in row.iter().enumerate() {
            attribute
                .buffer_mut()
                .write((index * N + component) * 4, &value.to_le_bytes());
        }
    }
    attribute
}

pub(super) fn push_attribute<const N: usize>(
    mesh: &mut Mesh,
    ty: GeometryAttributeType,
    rows: &[[f32; N]],
) {
    mesh.add_attribute(make_attribute(ty, rows));
}

/// Encode the shared square with a caller-supplied COLOR_0 attribute.
/// `method` selects sequential (0) or EdgeBreaker (1) connectivity.
pub(super) fn encode_square(method: i32, color: PointAttribute) -> Vec<u8> {
    let mut mesh = Mesh::new();
    push_attribute(&mut mesh, GeometryAttributeType::Position, &POSITIONS);
    push_attribute(&mut mesh, GeometryAttributeType::Normal, &NORMALS);
    push_attribute(&mut mesh, GeometryAttributeType::TexCoord, &UVS);
    mesh.add_attribute(color);
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

pub(super) fn encode_draco<const C: usize>(colors: &[[f32; C]], method: i32) -> Vec<u8> {
    encode_square(method, make_attribute(GeometryAttributeType::Color, colors))
}

/// COLOR_0 stored as UNSIGNED_SHORT in the bitstream. glTF declares
/// normalization on the accessor, not in the Draco stream, so real exporters
/// leave the bitstream's normalized flag clear and these arrive as 0..65535.
pub(super) const COLORS_U16: [[u16; 4]; 4] = [
    [u16::MAX, 0, 0, u16::MAX],
    [0, u16::MAX, 0, u16::MAX],
    [0, 0, u16::MAX, 32767],
    [32767, 32767, 0, 16383],
];

pub(super) fn u16_color_attribute() -> PointAttribute {
    let mut attribute = PointAttribute::new();
    attribute.init(
        GeometryAttributeType::Color,
        4,
        DataType::Uint16,
        false,
        COLORS_U16.len(),
    );
    for (index, row) in COLORS_U16.iter().enumerate() {
        for (component, value) in row.iter().enumerate() {
            attribute
                .buffer_mut()
                .write((index * 4 + component) * 2, &value.to_le_bytes());
        }
    }
    attribute
}

/// COLOR_0 stored as UNSIGNED_BYTE, the most common normalized color format.
pub(super) const COLORS_U8: [[u8; 4]; 4] = [
    [u8::MAX, 0, 0, u8::MAX],
    [0, u8::MAX, 0, u8::MAX],
    [0, 0, u8::MAX, 127],
    [127, 127, 0, 63],
];

pub(super) fn u8_color_attribute() -> PointAttribute {
    let mut attribute = PointAttribute::new();
    attribute.init(
        GeometryAttributeType::Color,
        4,
        DataType::Uint8,
        false,
        COLORS_U8.len(),
    );
    for (index, row) in COLORS_U8.iter().enumerate() {
        attribute.buffer_mut().write(index * 4, &row[..]);
    }
    attribute
}

/// Wrap a JSON document and its binary chunk into a GLB.
pub(super) fn glb(json: String, bin: &[u8]) -> Vec<u8> {
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
pub(super) fn build_parts(
    textured: bool,
    color_components: u8,
    method: i32,
    morph_targets: bool,
) -> (Value, Vec<u8>) {
    let (draco, color_accessor) = if color_components == 4 {
        (
            encode_draco(&COLORS4, method),
            json!({"componentType": 5126, "count": 4, "type": "VEC4"}),
        )
    } else {
        (
            encode_draco(&COLORS3, method),
            json!({"componentType": 5126, "count": 4, "type": "VEC3"}),
        )
    };
    build_parts_with(textured, draco, color_accessor, morph_targets)
}

/// The same document built from a caller-supplied Draco stream and COLOR_0
/// accessor, so a test can declare normalized integers the way an exporter does.
pub(super) fn build_parts_with(
    textured: bool,
    draco: Vec<u8>,
    color_accessor: Value,
    morph_targets: bool,
) -> (Value, Vec<u8>) {
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
            {"componentType": 5126, "count": 4, "type": "VEC4"},
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
    root["accessors"][3] = color_accessor;
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

pub(super) fn build_glb(
    textured: bool,
    color_components: u8,
    method: i32,
    morph_targets: bool,
) -> Vec<u8> {
    let (root, bin) = build_parts(textured, color_components, method, morph_targets);
    glb(root.to_string(), &bin)
}

pub(super) fn streams_of(gltf: &LitGltf) -> MeshStreams {
    let mesh = gltf
        .document
        .meshes()
        .next()
        .expect("the document has a mesh");
    let primitive = mesh.primitives().next().expect("the mesh has a primitive");
    streams::from_reader(&gltf.buffers, &primitive)
}

pub(super) fn decoded(textured: bool, color_components: u8, method: i32) -> MeshStreams {
    let bytes = build_glb(textured, color_components, method, false);
    let gltf = LitGltf::from_bytes(&bytes).expect("a Draco GLB imports as validated accessors");
    streams_of(&gltf)
}
