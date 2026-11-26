//! Decode `KHR_draco_mesh_compression` into ordinary glTF accessors at import.
//!
//! The extension moves a primitive's geometry into a Draco bitstream: its
//! `attributes` object maps each glTF semantic to the unique id of the
//! attribute inside that stream, while the primitive's accessors are left
//! without buffer views because the stream, not a buffer, holds their data.
//! [`fill_accessors`] decodes every such stream and writes the decompressed
//! values back as real buffer views and accessors. The document is then
//! validated normally, so every later reader — including morph targets, skins
//! and animations — sees only ordinary accessors and the loader never has to
//! trust an unvalidated document.
//!
//! The extension compresses a triangle primitive's attributes and indices and
//! nothing else, so a morph target keeps its own accessors: this module checks
//! that no target reads an accessor the stream also supplies, which would
//! leave the target with values the decoder never wrote. A stream that cannot
//! be decoded reports Draco's own error rather than a glTF validation failure,
//! because the bitstream is what is malformed and its reader is what says so.

use super::{DracoPrimitiveError, Error, MeshId, PrimitiveId};
use draco_core::decoder_buffer::DecoderBuffer;
use draco_core::draco_types::DataType;
use draco_core::geometry_attribute::PointAttribute;
use draco_core::mesh::Mesh as DracoMesh;
use draco_core::mesh_decoder::MeshDecoder;
use gltf::json::validation::{Checked, USize64};
use gltf::json::{self, Index, Value, accessor, buffer, mesh};
use hashbrown::HashMap;
use hashbrown::hash_map::Entry;
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
///
/// Several primitives may name one bitstream: a mesh that states its geometry
/// once and instances it, or two meshes sharing a buffer view. The bitstream is
/// then decoded once and every primitive reads its attributes out of the same
/// decoded mesh, which is what the reference implementations do — the extension
/// has no notion of a per-primitive stream, only of a buffer view that holds one.
pub(super) fn fill_accessors(
    root: &mut json::Root,
    buffers: &mut Vec<gltf::buffer::Data>,
) -> Result<bool, Error> {
    let compressed = collect(root)?;
    if compressed.is_empty() {
        return Ok(false);
    }

    let buffer_index = Index::new(root.buffers.len() as u32);
    let mut blob = Vec::new();
    let mut decoded: HashMap<usize, DracoMesh> = HashMap::new();
    for compressed in &compressed {
        let geometry = match decoded.entry(compressed.stream_view) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(decode_stream(
                root,
                buffers,
                MeshId::new(compressed.mesh_index),
                PrimitiveId::new(compressed.primitive_index),
                compressed.stream_view,
            )?),
        };
        let points = geometry.num_points();

        for (semantic, unique_id, accessor_index) in &compressed.attributes {
            let detail = |detail| Error::DracoPrimitive {
                mesh: MeshId::new(compressed.mesh_index),
                primitive: PrimitiveId::new(compressed.primitive_index),
                detail,
            };
            let attribute = geometry
                .attribute_by_unique_id(*unique_id)
                .ok_or_else(|| detail(DracoPrimitiveError::MissingAttribute { id: *unique_id }))?;
            let (components, accessor_normalized) = match root.accessors.get(*accessor_index) {
                Some(accessor) => match &accessor.type_ {
                    Checked::Valid(kind) => (kind.multiplicity(), accessor.normalized),
                    Checked::Invalid => {
                        return Err(detail(DracoPrimitiveError::InvalidAccessorType));
                    }
                },
                None => {
                    return Err(detail(DracoPrimitiveError::MissingAccessor {
                        accessor: *accessor_index,
                    }));
                }
            };
            if (attribute.num_components() as usize) < components {
                return Err(detail(DracoPrimitiveError::TooFewComponents));
            }
            let joints = semantic.starts_with("JOINTS_");
            // The accessor declaration decides normalization; the bitstream flag is
            // only a fallback for exporters that set it, since most leave it clear.
            // Joint indices are written back as integers, so they are never rescaled
            // into 0..1.
            let normalize = !joints && (accessor_normalized || attribute.normalized());
            let values =
                read_attribute(attribute, points, components, normalize).map_err(detail)?;
            let bytes = encode(&values, joints).map_err(detail)?;
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

            let accessor =
                root.accessors
                    .get_mut(accessor_index)
                    .ok_or_else(|| Error::DracoPrimitive {
                        mesh: MeshId::new(compressed.mesh_index),
                        primitive: PrimitiveId::new(compressed.primitive_index),
                        detail: DracoPrimitiveError::MissingAccessor {
                            accessor: accessor_index,
                        },
                    })?;
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
    blob.resize(super::image::align_up(blob.len(), size_of::<f32>()), 0);
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
///
/// Joint indices name a joint, not a quantity: they are never normalized, and
/// the rewritten accessor states them as 16-bit integers. A value the joint
/// stream cannot hold is refused here rather than truncated into a different
/// joint.
fn encode(values: &[f32], joints: bool) -> Result<Vec<u8>, DracoPrimitiveError> {
    let width = if joints {
        size_of::<u16>()
    } else {
        size_of::<f32>()
    };
    let mut bytes = Vec::with_capacity(values.len() * width);
    for value in values {
        if joints {
            if *value < 0.0 || *value > f32::from(u16::MAX) || value.fract() != 0.0 {
                return Err(DracoPrimitiveError::JointOutOfRange { joint: *value });
            }
            bytes.extend_from_slice(U16::new(*value as u16).as_bytes());
        } else {
            bytes.extend_from_slice(F32::new(*value).as_bytes());
        }
    }
    Ok(bytes)
}

/// Read a decoded attribute as the floats its accessor declares.
///
/// A normalized integer attribute is rescaled into `0..=1`. The 16-bit and
/// 8-bit integers are exactly representable in the 32-bit float the accessor
/// holds, so the scale is exact; a 32-bit integer is not, and is refused rather
/// than normalized into values that differ from what the document states.
fn read_attribute(
    attribute: &PointAttribute,
    points: usize,
    components: usize,
    normalize: bool,
) -> Result<Vec<f32>, DracoPrimitiveError> {
    let mut values = attribute.read_f32s(points, components);
    if !normalize {
        return Ok(values);
    }
    let max = match integral_max(attribute.data_type()) {
        IntegralMax::NotAnInteger => return Ok(values),
        IntegralMax::Exact(max) => max,
        IntegralMax::TooWide => {
            return Err(DracoPrimitiveError::NormalizationTooWide {
                data_type: data_type_name(attribute.data_type()),
            });
        }
    };
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
    Ok(values)
}

/// How far an integer component type has to be divided to reach `0..=1`.
enum IntegralMax {
    /// The type is already a float.
    NotAnInteger,
    /// The type's range maps onto `0..=1` exactly.
    Exact(f32),
    /// The type's range exceeds what a 32-bit float states exactly.
    TooWide,
}

/// The divisor a normalized integer component type uses.
fn integral_max(data_type: DataType) -> IntegralMax {
    match data_type {
        DataType::Int8 => IntegralMax::Exact(f32::from(i8::MAX)),
        DataType::Uint8 => IntegralMax::Exact(f32::from(u8::MAX)),
        DataType::Int16 => IntegralMax::Exact(f32::from(i16::MAX)),
        DataType::Uint16 => IntegralMax::Exact(f32::from(u16::MAX)),
        DataType::Int32 | DataType::Uint32 => IntegralMax::TooWide,
        _ => IntegralMax::NotAnInteger,
    }
}

/// The name a component type goes by in an error.
fn data_type_name(data_type: DataType) -> &'static str {
    match data_type {
        DataType::Int8 => "Int8",
        DataType::Uint8 => "Uint8",
        DataType::Int16 => "Int16",
        DataType::Uint16 => "Uint16",
        DataType::Int32 => "Int32",
        DataType::Uint32 => "Uint32",
        _ => "non-integer",
    }
}

/// Decode one bitstream out of the buffers it lives in.
fn decode_stream(
    root: &json::Root,
    buffers: &[gltf::buffer::Data],
    mesh: MeshId,
    primitive: PrimitiveId,
    view_index: usize,
) -> Result<DracoMesh, Error> {
    let detail = |detail| Error::DracoPrimitive {
        mesh,
        primitive,
        detail,
    };
    let view = root
        .buffer_views
        .get(view_index)
        .ok_or_else(|| detail(DracoPrimitiveError::MissingStream { view: view_index }))?;
    let buffer_index = view.buffer.value();
    let buffer = buffers.get(buffer_index).ok_or_else(|| {
        detail(DracoPrimitiveError::MissingStreamBuffer {
            view: view_index,
            buffer: buffer_index,
        })
    })?;
    let start = view.byte_offset.map_or(0, |offset| offset.0) as usize;
    let end = start
        .checked_add(view.byte_length.0 as usize)
        .ok_or_else(|| detail(DracoPrimitiveError::StreamOverflowsBuffer { view: view_index }))?;
    let bytes = buffer
        .0
        .get(start..end)
        .ok_or_else(|| detail(DracoPrimitiveError::StreamPastBuffer { view: view_index }))?;

    let mut input = DecoderBuffer::new(bytes);
    let mut geometry = DracoMesh::new();
    MeshDecoder::new()
        .decode(&mut input, &mut geometry)
        .map_err(|source| Error::Draco {
            mesh,
            primitive,
            source,
        })?;
    Ok(geometry)
}

/// Collect every compressed primitive the document carries.
///
/// Nothing is rewritten here: a document the extension cannot express is
/// refused before any accessor is touched, so a failed repair never leaves a
/// half-decoded document behind.
fn collect(root: &json::Root) -> Result<Vec<Compressed>, Error> {
    let mut compressed = Vec::new();
    for (mesh_index, mesh) in root.meshes.iter().enumerate() {
        for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
            let Some(extensions) = &primitive.extensions else {
                continue;
            };
            let Some(extension) = extensions.others.get(EXTENSION) else {
                continue;
            };
            let detail = |detail| Error::DracoPrimitive {
                mesh: MeshId::new(mesh_index),
                primitive: PrimitiveId::new(primitive_index),
                detail,
            };

            // The extension compresses a triangle primitive only, and the
            // decoder always yields a triangle list. A strip is accepted and
            // rewritten as a list; anything else has no decoded geometry to
            // become.
            match primitive.mode {
                Checked::Valid(mesh::Mode::Triangles | mesh::Mode::TriangleStrip) => {}
                _ => return Err(detail(DracoPrimitiveError::NotTriangles)),
            }

            let stream_view = extension
                .get("bufferView")
                .and_then(Value::as_u64)
                .ok_or_else(|| detail(DracoPrimitiveError::ExtensionNamesNoStream))?
                as usize;
            let ids = extension
                .get("attributes")
                .and_then(Value::as_object)
                .ok_or_else(|| detail(DracoPrimitiveError::ExtensionMapsNoAttributes))?;

            // The extension must map a subset of the semantics the primitive
            // declares: a semantic only the extension names has no accessor to
            // rewrite, and the values it decodes to would have nowhere to go.
            let mut declared: Vec<String> = Vec::with_capacity(primitive.attributes.len());
            for semantic in primitive.attributes.keys() {
                if let Checked::Valid(semantic) = semantic {
                    declared.push(semantic.to_string());
                }
            }
            for semantic in ids.keys() {
                if !declared.iter().any(|name| name == semantic) {
                    return Err(detail(DracoPrimitiveError::AttributeNotDeclared {
                        semantic: semantic.clone(),
                    }));
                }
            }

            let mut attributes = Vec::with_capacity(declared.len());
            for (semantic, accessor) in &primitive.attributes {
                let Checked::Valid(semantic) = semantic else {
                    continue;
                };
                let name = semantic.to_string();
                // An attribute outside the stream is processed as usual.
                let Some(unique_id) = ids.get(&name).and_then(Value::as_u64) else {
                    continue;
                };
                let unique_id = u32::try_from(unique_id).map_err(|_| {
                    detail(DracoPrimitiveError::AttributeIdTooLarge { id: unique_id })
                })?;
                attributes.push((name, unique_id, accessor.value()));
            }

            let indices = primitive.indices.as_ref().map(|index| index.value());

            // A morph target keeps its own accessors, because the extension
            // compresses a primitive's geometry and not its displacements. A
            // target naming an accessor the stream supplies would state
            // displacements the decoded geometry overwrites.
            for (target, morph_target) in primitive
                .targets
                .as_deref()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                for accessor in [
                    morph_target.positions,
                    morph_target.normals,
                    morph_target.tangents,
                ]
                .into_iter()
                .flatten()
                {
                    let accessor = accessor.value();
                    let in_stream = attributes
                        .iter()
                        .any(|(_, _, in_stream)| *in_stream == accessor)
                        || indices == Some(accessor);
                    if in_stream {
                        return Err(detail(DracoPrimitiveError::MorphTargetInStream {
                            target,
                            accessor,
                        }));
                    }
                }
            }

            compressed.push(Compressed {
                mesh_index,
                primitive_index,
                stream_view,
                attributes,
                indices,
            });
        }
    }
    Ok(compressed)
}
