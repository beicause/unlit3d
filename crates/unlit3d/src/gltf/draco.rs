//! Decode `KHR_draco_mesh_compression` primitives into uploadable streams.
//!
//! The extension moves a primitive's geometry into a Draco bitstream. Its
//! `attributes` object maps each glTF semantic to the unique id of the
//! attribute inside that stream, while the primitive's accessors are left
//! without buffer views because the stream, not a buffer, holds their data.
//! [`decode`] reads those semantics back out; morph targets are not part of a
//! Draco bitstream, so a compressed primitive does not upload them.

use super::material::samples_base_color_texture;
use super::streams::MeshStreams;
use draco_core::decoder_buffer::DecoderBuffer;
use draco_core::draco_types::DataType;
use draco_core::geometry_attribute::PointAttribute;
use draco_core::mesh::Mesh;
use draco_core::mesh_decoder::MeshDecoder;
use gltf::json::Value;

/// The extension a compressed primitive declares.
pub(super) const EXTENSION: &str = "KHR_draco_mesh_compression";

/// Decode a compressed primitive's geometry into the streams a lit mesh wants.
///
/// `extension` is the primitive's `KHR_draco_mesh_compression` value. A
/// semantic it does not list is left out of the result; a Draco bitstream does
/// not carry morph targets, so those are never present.
pub(super) fn decode(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    primitive: &gltf::Primitive<'_>,
    extension: &Value,
) -> MeshStreams {
    let view_index = extension
        .get("bufferView")
        .and_then(Value::as_u64)
        .expect("a Draco extension names the bufferView holding the stream")
        as usize;
    let attributes = extension
        .get("attributes")
        .and_then(Value::as_object)
        .expect("a Draco extension maps glTF semantics to attribute ids");
    let view = document
        .views()
        .nth(view_index)
        .expect("the Draco bufferView exists");
    let buffer: &[u8] = &buffers[view.buffer().index()];
    let bytes = &buffer[view.offset()..view.offset() + view.length()];

    let mut input = DecoderBuffer::new(bytes);
    let mut geometry = Mesh::new();
    MeshDecoder::new()
        .decode(&mut input, &mut geometry)
        .expect("the Draco bitstream decodes");

    let points = geometry.num_points();
    let attribute = |semantic: &str| -> Option<&PointAttribute> {
        let id = attributes.get(semantic).and_then(Value::as_u64)? as u32;
        geometry.attribute_by_unique_id(id)
    };

    let mut streams = MeshStreams {
        positions: rows::<3>(
            attribute("POSITION").expect("the Draco geometry carries POSITION"),
            points,
        ),
        ..MeshStreams::default()
    };
    // Only the UVs a textured draw reads are worth uploading, matching the
    // ordinary reader's rule.
    if samples_base_color_texture(primitive) {
        streams.uvs = attribute("TEXCOORD_0").map(|uv| rows::<2>(uv, points));
    }
    if primitive.get(&gltf::Semantic::Colors(0)).is_some() {
        streams.colors = attribute("COLOR_0").map(|color| colors(color, points));
    }
    if let Some(normal) = attribute("NORMAL") {
        streams.normals = Some(rows::<3>(normal, points));
    }
    // Joints and weights come as a pair, or the mesh is not skinned at all.
    let joints = attribute("JOINTS_0").map(|joint| joint_rows(joint, points));
    let weights = attribute("WEIGHTS_0").map(|weight| rows::<4>(weight, points));
    if joints.is_some() && weights.is_some() {
        streams.joints = joints;
        streams.weights = weights;
    }
    streams.indices = Some(
        geometry
            .faces()
            .iter()
            .flat_map(|face| face.iter().map(|point| point.0))
            .collect(),
    );
    streams
}

/// The largest scalar a normalized integer attribute can hold.
fn integral_max(data_type: DataType) -> f32 {
    match data_type {
        DataType::Int8 => i8::MAX as f32,
        DataType::Uint8 => u8::MAX as f32,
        DataType::Int16 => i16::MAX as f32,
        DataType::Uint16 => u16::MAX as f32,
        DataType::Int32 => i32::MAX as f32,
        DataType::Uint32 => u32::MAX as f32,
        _ => 1.0,
    }
}

/// Widen a decoded attribute into rows of floats, applying glTF's normalized
/// integer rule: Draco's reader widens integers but does not rescale them.
fn widen(attribute: &PointAttribute, points: usize, components: usize) -> Vec<f32> {
    let mut values = attribute.read_f32s(points, components);
    if attribute.normalized() && attribute.data_type().is_integral() {
        let max = integral_max(attribute.data_type());
        let signed = matches!(
            attribute.data_type(),
            DataType::Int8 | DataType::Int16 | DataType::Int32
        );
        for value in &mut values {
            *value = if signed {
                (*value / max).max(-1.0)
            } else {
                *value / max
            };
        }
    }
    values
}

fn rows<const N: usize>(attribute: &PointAttribute, points: usize) -> Vec<[f32; N]> {
    let values = widen(attribute, points, N);
    let (rows, remainder) = values.as_chunks::<N>();
    debug_assert!(remainder.is_empty());
    rows.iter()
        .map(|row| std::array::from_fn(|component| row[component]))
        .collect()
}

/// Convert decoded colors to the `RGBA8` the lit vertex channel stores,
/// supplying the alpha the spec gives a three-component `COLOR_0`.
fn colors(attribute: &PointAttribute, points: usize) -> Vec<[u8; 4]> {
    let alpha = attribute.num_components() >= 4;
    let values = widen(attribute, points, 4);
    let (rows, remainder) = values.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    rows.iter()
        .map(|row| {
            let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            [
                byte(row[0]),
                byte(row[1]),
                byte(row[2]),
                if alpha { byte(row[3]) } else { u8::MAX },
            ]
        })
        .collect()
}

/// Joint indices name a joint, not a quantity, so they are never normalized.
fn joint_rows(attribute: &PointAttribute, points: usize) -> Vec<[u16; 4]> {
    let values = attribute.read_f32s(points, 4);
    let (rows, remainder) = values.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    rows.iter()
        .map(|row| std::array::from_fn(|joint| row[joint].max(0.0) as u16))
        .collect()
}
