//! Decode `KHR_draco_mesh_compression` into ordinary glTF accessors at import.
//!
//! The extension moves a primitive's geometry into a Draco bitstream: its
//! `attributes` object maps each glTF semantic to the unique id of the
//! attribute inside that stream, while the primitive's accessors are left
//! without buffer views because the stream, not a buffer, holds their data.
//! [`fill_accessors`] decodes every such stream and writes the decompressed
//! values back as real buffer views and accessors. The document is then
//! validated normally, so every later reader -- including morph targets, skins
//! and animations -- sees only ordinary accessors and the loader never has to
//! trust an unvalidated document.

use draco_core::decoder_buffer::DecoderBuffer;
use draco_core::draco_types::DataType;
use draco_core::geometry_attribute::PointAttribute;
use draco_core::mesh::Mesh as DracoMesh;
use draco_core::mesh_decoder::MeshDecoder;
use gltf::json::validation::{Checked, USize64};
use gltf::json::{self, Index, Path, Value, accessor, buffer, mesh};
use std::mem::size_of;
use zerocopy::IntoBytes;
use zerocopy::byteorder::little_endian::{F32, U16};

/// The extension a compressed primitive declares.
pub(super) const EXTENSION: &str = "KHR_draco_mesh_compression";

/// One compressed primitive and the accessors its stream supplies.
struct Compressed {
    mesh_index: usize,
    primitive_index: usize,
    stream_view: usize,
    attributes: Vec<(String, u32, usize)>,
    indices: Option<usize>,
}

/// Decode every Draco primitive and fill its accessors with real buffer views.
///
/// Returns `false` when the document carries no compressed primitive, so the
/// caller can keep the original validation error. Decoded values live in a new
/// buffer appended to `buffers`; the original buffers and images stay valid.
pub(super) fn fill_accessors(
    root: &mut json::Root,
    buffers: &mut Vec<gltf::buffer::Data>,
) -> Result<bool, gltf::Error> {
    let compressed = collect(root)?;
    if compressed.is_empty() {
        return Ok(false);
    }

    let buffer_index = Index::new(root.buffers.len() as u32);
    let mut blob = Vec::new();
    for compressed in &compressed {
        let geometry = decode_stream(root, buffers, compressed.stream_view)?;
        let points = geometry.num_points();

        for (semantic, unique_id, accessor_index) in &compressed.attributes {
            let attribute = geometry
                .attribute_by_unique_id(*unique_id)
                .ok_or_else(|| invalid("a Draco attribute id is missing from the stream"))?;
            let (components, accessor_normalized) = match root.accessors.get(*accessor_index) {
                Some(accessor) => match &accessor.type_ {
                    Checked::Valid(kind) => (kind.multiplicity(), accessor.normalized),
                    Checked::Invalid => {
                        return Err(invalid("a Draco accessor has an invalid type"));
                    }
                },
                None => return Err(invalid("a Draco primitive names a missing accessor")),
            };
            if (attribute.num_components() as usize) < components {
                return Err(invalid(
                    "a Draco attribute has fewer components than its accessor declares",
                ));
            }
            let joints = semantic.starts_with("JOINTS_");
            // The accessor declaration decides normalization; the bitstream flag is
            // only a fallback for exporters that set it, since most leave it clear.
            // Joint indices are written back as integers, so they are never rescaled
            // into 0..1.
            let normalize = !joints && (accessor_normalized || attribute.normalized());
            let values = read_attribute(attribute, points, components, normalize);
            let bytes = encode(&values, joints);
            let view = push_view(root, buffer_index, &mut blob, bytes);

            let accessor = &mut root.accessors[*accessor_index];
            accessor.buffer_view = Some(view);
            accessor.byte_offset = None;
            accessor.count = USize64(points as u64);
            accessor.component_type = Checked::Valid(accessor::GenericComponentType(if joints {
                accessor::ComponentType::U16
            } else {
                accessor::ComponentType::F32
            }));
            // min/max are kept as the document wrote them. glTF requires them on
            // POSITION and a conformant generator writes them for the decoded data.
            // A normalized integer accessor stores them unscaled while this rewrite
            // bakes the scale into the values, so for that case alone they describe
            // the pre-rewrite values; nothing in this loader reads them.
            accessor.normalized = false;
        }

        if let Some(accessor_index) = compressed.indices {
            let indices: Vec<u32> = geometry
                .faces()
                .iter()
                .flat_map(|face| face.iter().map(|point| point.0))
                .collect();
            let bytes: Vec<u8> = indices
                .iter()
                .flat_map(|index| index.to_le_bytes())
                .collect();
            let view = push_view(root, buffer_index, &mut blob, bytes);

            let accessor = root
                .accessors
                .get_mut(accessor_index)
                .ok_or_else(|| invalid("a Draco primitive names a missing indices accessor"))?;
            accessor.buffer_view = Some(view);
            accessor.byte_offset = None;
            accessor.count = USize64(indices.len() as u64);
            accessor.component_type =
                Checked::Valid(accessor::GenericComponentType(accessor::ComponentType::U32));
            accessor.normalized = false;
        }

        // The decoder yields explicit triangle faces whatever the source mode
        // was. A strip-mode primitive is rewritten as a triangle list so the
        // ordinary reader draws exactly the geometry the bitstream carried.
        let primitive =
            &mut root.meshes[compressed.mesh_index].primitives[compressed.primitive_index];
        primitive.mode = Checked::Valid(mesh::Mode::Triangles);
        if let Some(extensions) = &mut primitive.extensions {
            extensions.others.remove(EXTENSION);
            if extensions.others.is_empty() {
                primitive.extensions = None;
            }
        }
    }

    root.buffers.push(json::Buffer {
        byte_length: USize64(blob.len() as u64),
        name: None,
        uri: None,
        extensions: None,
        extras: Default::default(),
    });
    buffers.push(gltf::buffer::Data(blob));
    root.extensions_used
        .retain(|extension| extension != EXTENSION);
    root.extensions_required
        .retain(|extension| extension != EXTENSION);
    Ok(true)
}

/// Append one tightly packed view over the loader-owned buffer.
fn push_view(
    root: &mut json::Root,
    buffer: Index<buffer::Buffer>,
    blob: &mut Vec<u8>,
    bytes: Vec<u8>,
) -> Index<buffer::View> {
    // glTF requires a buffer view's byte offset into its buffer to be a
    // multiple of the accessor's component size. Padding the loader-owned
    // blob to the widest component the decoder writes keeps that true even
    // when a stream is not naturally a multiple of four bytes.
    const ALIGNMENT: usize = size_of::<f32>();
    blob.resize(blob.len().div_ceil(ALIGNMENT) * ALIGNMENT, 0);
    let offset = blob.len();
    let length = bytes.len();
    blob.extend(bytes);
    let view = buffer::View {
        buffer,
        byte_length: USize64(length as u64),
        byte_offset: Some(USize64(offset as u64)),
        byte_stride: None,
        name: None,
        target: None,
        extensions: None,
        extras: Default::default(),
    };
    let index = Index::new(root.buffer_views.len() as u32);
    root.buffer_views.push(view);
    index
}

/// Encode decoded floats the way the rewritten accessor declares them.
fn encode(values: &[f32], joints: bool) -> Vec<u8> {
    // Joint indices name a joint, not a quantity: they are never normalized,
    // and glTF only allows unsigned integers for them.
    let mut bytes = Vec::with_capacity(
        values.len()
            * if joints {
                size_of::<U16>()
            } else {
                size_of::<F32>()
            },
    );
    for value in values {
        if joints {
            bytes.extend_from_slice(U16::new(*value as u16).as_bytes());
        } else {
            bytes.extend_from_slice(F32::new(*value).as_bytes());
        }
    }
    bytes
}

/// Widen a decoded attribute into f32 rows, rescaling normalized integers.
///
/// Draco's reader widens integers without rescaling, and the rewritten
/// accessor is a float, so the normalization has to be baked into the values.
/// `normalize` comes from the accessor declaration (or the bitstream flag),
/// not from the bitstream alone: most exporters leave that flag clear.
fn read_attribute(
    attribute: &PointAttribute,
    points: usize,
    components: usize,
    normalize: bool,
) -> Vec<f32> {
    let mut values = attribute.read_f32s(points, components);
    if normalize && let Some(max) = integral_max(attribute.data_type()) {
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

/// The largest scalar a normalized integer attribute can hold. Draco's reader
/// only materialises the types listed here; the others need no rescale.
fn integral_max(data_type: DataType) -> Option<f32> {
    Some(match data_type {
        DataType::Int8 => i8::MAX as f32,
        DataType::Uint8 => u8::MAX as f32,
        DataType::Int16 => i16::MAX as f32,
        DataType::Uint16 => u16::MAX as f32,
        DataType::Int32 => i32::MAX as f32,
        DataType::Uint32 => u32::MAX as f32,
        _ => return None,
    })
}

/// Decode one primitive's bitstream out of the document's buffers.
fn decode_stream(
    root: &json::Root,
    buffers: &[gltf::buffer::Data],
    view_index: usize,
) -> Result<DracoMesh, gltf::Error> {
    let view = root
        .buffer_views
        .get(view_index)
        .ok_or_else(|| invalid("the Draco bufferView does not exist"))?;
    let data = buffers
        .get(view.buffer.value())
        .ok_or_else(|| invalid("the Draco bufferView names a missing buffer"))?;
    let offset = view
        .byte_offset
        .as_ref()
        .map_or(0, |value| value.0 as usize);
    let end = offset
        .checked_add(view.byte_length.0 as usize)
        .ok_or_else(|| invalid("the Draco stream overflows its buffer"))?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| invalid("the Draco stream extends past its buffer"))?;

    let mut input = DecoderBuffer::new(bytes);
    let mut geometry = DracoMesh::new();
    MeshDecoder::new()
        .decode(&mut input, &mut geometry)
        .map_err(|_| invalid("the Draco bitstream does not decode"))?;
    Ok(geometry)
}

/// Find every compressed primitive and the accessors its stream supplies.
fn collect(root: &json::Root) -> Result<Vec<Compressed>, gltf::Error> {
    let mut compressed = Vec::new();
    for (mesh_index, model) in root.meshes.iter().enumerate() {
        for (primitive_index, primitive) in model.primitives.iter().enumerate() {
            let Some(extensions) = &primitive.extensions else {
                continue;
            };
            let Some(extension) = extensions.others.get(EXTENSION) else {
                continue;
            };
            let stream_view = usize::try_from(
                extension
                    .get("bufferView")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| invalid("a Draco extension names its bufferView"))?,
            )
            .map_err(|_| invalid("the Draco bufferView index is too large"))?;
            let ids = extension
                .get("attributes")
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("a Draco extension maps semantics to attribute ids"))?;

            let mut attributes = Vec::new();
            for (semantic, accessor) in &primitive.attributes {
                // An invalid semantic cannot name a stream attribute; glTF's
                // own validation reports it, so leave it alone here.
                let Checked::Valid(semantic) = semantic else {
                    continue;
                };
                let name = semantic.to_string();
                let Some(unique_id) = ids.get(&name).and_then(Value::as_u64) else {
                    // An attribute outside the stream is processed as usual.
                    continue;
                };
                let unique_id = u32::try_from(unique_id)
                    .map_err(|_| invalid("a Draco attribute id is too large"))?;
                attributes.push((name, unique_id, accessor.value()));
            }
            compressed.push(Compressed {
                mesh_index,
                primitive_index,
                stream_view,
                attributes,
                indices: primitive.indices.as_ref().map(|index| index.value()),
            });
        }
    }
    Ok(compressed)
}

/// Report a malformed Draco extension through glTF's own validation error.
fn invalid(detail: &str) -> gltf::Error {
    gltf::Error::Validation(vec![(
        Path::new().field(EXTENSION).value_str(detail),
        gltf::json::validation::Error::Invalid,
    )])
}
