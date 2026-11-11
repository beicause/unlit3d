use super::*;
use wgpu::WriteOnly;
use zerocopy::IntoBytes;

/// Write raw UVs, compressed colors and raw normals through the
/// mapped-buffer path and return the bytes.
fn write_stream(
    stream: SecondaryStreamWriter,
    uvs: &[[f32; 2]],
    colors: &[CompressedColor],
    normals: &[[f32; 3]],
    vertex_count: usize,
    metadata: &mut MeshMetadata,
) -> Vec<u8> {
    let mut out = vec![0u8; stream.byte_len(vertex_count)];
    stream.write(
        uvs,
        colors,
        normals,
        metadata,
        WriteOnly::from_mut(out.as_mut_slice()),
    );
    out
}

/// Both writers must produce byte-identical streams, since the raw one
/// compresses its UVs into exactly what the compressed one expects and
/// both copy the colors verbatim.
#[test]
fn raw_and_compressed_writers_agree() {
    let uvs = [[0.0f32, 0.25], [0.5, 1.0], [1.0, 0.0]];
    let colors: Vec<CompressedColor> = quantize_colors(&[
        [0.0f32, 0.25, 0.5, 1.0],
        [1.0, 0.0, 0.0, 1.0],
        [0.2, 0.4, 0.6, 0.8],
    ])
    .collect();

    let normals = [[0.0f32, 0.0, 1.0], [0.6, 0.8, 0.0], [-0.6, 0.0, -0.8]];

    for stream in [
        SecondaryStreamWriter {
            flags: SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
        },
        SecondaryStreamWriter {
            flags: SecondaryFlags::UV,
        },
        SecondaryStreamWriter {
            flags: SecondaryFlags::COLOR,
        },
        SecondaryStreamWriter {
            flags: SecondaryFlags::NORMAL,
        },
    ] {
        let mut metadata = MeshMetadata::default();
        let from_raw = write_stream(stream, &uvs, &colors, &normals, 3, &mut metadata);

        // Compress separately, exactly as a caller sharing one stream
        // across meshes would.
        let packed_uvs: Vec<_> = if stream.uv() {
            compress_uvs(&uvs, &mut metadata).collect()
        } else {
            Vec::new()
        };
        let packed_colors: Vec<_> = if stream.color() {
            colors.clone()
        } else {
            Vec::new()
        };
        let packed_normals: Vec<_> = if stream.normal() {
            compress_normals(&normals).collect()
        } else {
            Vec::new()
        };
        let mut from_compressed = vec![0u8; stream.byte_len(3)];
        stream.write_compressed(
            &packed_uvs,
            &packed_colors,
            &packed_normals,
            WriteOnly::from_mut(from_compressed.as_mut_slice()),
        );

        assert_eq!(from_raw, from_compressed, "stream {stream:?}");
        assert_eq!(from_raw.len(), stream.byte_len(3), "stream {stream:?}");
    }
}

#[test]
fn secondary_stream_interleaves_in_attribute_order() {
    let uvs = [[0.0f32, 0.0], [1.0, 1.0]];
    // Colors are linear RGBA in [0, 1]; they quantize to Unorm8.
    let colors = [[0u8, 64, 128, 255], [255, 0, 0, 255]];
    let mut metadata = MeshMetadata::default();

    let out = write_stream(
        SecondaryStreamWriter {
            flags: SecondaryFlags::UV | SecondaryFlags::COLOR,
        },
        &uvs,
        &colors,
        &[],
        2,
        &mut metadata,
    );
    assert_eq!(out.len(), 2 * 8);
    // Vertex 0 starts with the UV then the color, matching the shader's
    // location order 1 then 2. The UV remaps [0, 1] to Snorm16 [-1, 1],
    // so 0.0 quantizes to -32767 (little-endian) and 1.0 to 32767.
    let expected_uv = [(-32767i16).to_le_bytes(), (-32767i16).to_le_bytes()].concat();
    assert_eq!(&out[0..4], &expected_uv[..]);
    assert_eq!(&out[4..8], &[0, 64, 128, 255]);
    // The second vertex sits at the far corner of the UV range.
    assert_eq!(
        &out[8..12],
        &[32767i16.to_le_bytes(), 32767i16.to_le_bytes()].concat()
    );

    // The UV compression still produced the decode parameters.
    assert_eq!(
        metadata.uv_min_and_extents,
        glam::Vec4::new(0.0, 0.0, 1.0, 1.0)
    );

    // UV only: four bytes per vertex.
    let out = write_stream(
        SecondaryStreamWriter {
            flags: SecondaryFlags::UV,
        },
        &uvs,
        &[],
        &[],
        2,
        &mut MeshMetadata::default(),
    );
    assert_eq!(out.len(), 2 * 4);

    // Color only: four bytes per vertex, no UV bytes.
    let out = write_stream(
        SecondaryStreamWriter {
            flags: SecondaryFlags::COLOR,
        },
        &[],
        &colors,
        &[],
        2,
        &mut MeshMetadata::default(),
    );
    assert_eq!(out.len(), 2 * 4);
    assert_eq!(&out[0..4], &[0, 64, 128, 255]);
}

/// An uncompressed UV is copied at full precision: it neither derives
/// metadata nor quantizes, so the bytes are the input `f32`s.
#[test]
fn uncompressed_uv_is_written_at_full_precision() {
    let uvs = [[0.0f32, 0.0], [0.5, 0.75]];
    let stream = SecondaryStreamWriter {
        flags: SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV,
    };
    let mut metadata = MeshMetadata::default();
    let out = write_stream(stream, &uvs, &[], &[], 2, &mut metadata);

    let stride = wgpu::VertexFormat::Float32x2.size() as usize;
    assert_eq!(out.len(), 2 * stride);
    for (vertex, uv) in out.chunks(stride).zip(&uvs) {
        assert_eq!(vertex, uv.as_bytes());
    }
    // No compression happened, so the metadata is untouched.
    assert_eq!(metadata, MeshMetadata::default());
}

/// Decode exactly like `mesh_compression.wesl::decode_position`.
fn decode_position(encoded: CompressedPosition, metadata: &MeshMetadata) -> glam::Vec3 {
    let encode = glam::Vec3::new(
        encoded[0] as f32 / i16::MAX as f32,
        encoded[1] as f32 / i16::MAX as f32,
        encoded[2] as f32 / i16::MAX as f32,
    );
    metadata.aabb_center + encode * metadata.aabb_half_extents
}

/// Decode exactly like `mesh_compression.wesl::decode_uv`.
fn decode_uv(encoded: CompressedUv, metadata: &MeshMetadata) -> glam::Vec2 {
    let encode = glam::Vec2::new(
        encoded[0] as f32 / i16::MAX as f32,
        encoded[1] as f32 / i16::MAX as f32,
    );
    let t = encode * 0.5 + 0.5;
    glam::Vec2::new(metadata.uv_min_and_extents.x, metadata.uv_min_and_extents.y)
        + t * glam::Vec2::new(metadata.uv_min_and_extents.z, metadata.uv_min_and_extents.w)
}

#[test]
fn position_roundtrip_stays_within_quantization_error() {
    let positions = [[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-1.0, 0.5, 2.0]];
    let mut metadata = MeshMetadata::default();
    let encoded: Vec<_> = compress_positions(&positions, &mut metadata).collect();

    assert_eq!(encoded.len(), positions.len());
    for (position, packed) in positions.iter().zip(&encoded) {
        let decoded = decode_position(*packed, &metadata);
        let expected = glam::Vec3::from(*position);
        // One quantization step of the box's half-extent.
        let tolerance = metadata.aabb_half_extents.max_element() * 2.0 / i16::MAX as f32;
        assert!(
            (decoded - expected).abs().max_element() <= tolerance,
            "{decoded} vs {expected}"
        );
    }
    assert_eq!(metadata.aabb_center, glam::Vec3::new(0.0, 1.0, 1.5));
    assert_eq!(metadata.aabb_half_extents, glam::Vec3::new(1.0, 1.0, 1.5));
}

#[test]
fn degenerate_position_axis_collapses_to_center() {
    // All vertices share z = 4.0, so that axis has zero extent.
    let positions = [[0.0, 0.0, 4.0], [1.0, 1.0, 4.0]];
    let mut metadata = MeshMetadata::default();
    let encoded: Vec<_> = compress_positions(&positions, &mut metadata).collect();

    assert_eq!(metadata.aabb_half_extents.z, 0.0);
    assert!(encoded.iter().all(|packed| packed[2] == 0));
    assert_eq!(decode_position(encoded[0], &metadata).z, 4.0);
}

#[test]
fn uv_roundtrip_stays_within_quantization_error() {
    let uvs = [[0.25, 0.5], [1.0, 0.75], [-0.5, 2.0]];
    let mut metadata = MeshMetadata::default();
    let encoded: Vec<_> = compress_uvs(&uvs, &mut metadata).collect();

    assert_eq!(encoded.len(), uvs.len());
    for (uv, packed) in uvs.iter().zip(&encoded) {
        let decoded = decode_uv(*packed, &metadata);
        let expected = glam::Vec2::from(*uv);
        let extents = glam::Vec2::new(metadata.uv_min_and_extents.z, metadata.uv_min_and_extents.w);
        let tolerance = extents.max_element() * 2.0 / i16::MAX as f32;
        assert!(
            (decoded - expected).abs().max_element() <= tolerance,
            "{decoded} vs {expected}"
        );
    }
}

/// Decode exactly like `mesh_compression.wesl::decode_normal`.
fn decode_normal(encoded: CompressedNormal) -> glam::Vec3 {
    let encode = glam::Vec2::new(
        encoded[0] as f32 / i16::MAX as f32,
        encoded[1] as f32 / i16::MAX as f32,
    );
    let mut normal = glam::Vec3::new(encode.x, encode.y, 1.0 - encode.abs().element_sum());
    if normal.z < 0.0 {
        normal = glam::Vec3::new(
            (1.0 - normal.y.abs()) * normal.x.signum(),
            (1.0 - normal.x.abs()) * normal.y.signum(),
            normal.z,
        );
    }
    normal.normalize()
}

/// A normal survives the octahedral round trip to well under the angular
/// error shading would notice, including the folded lower hemisphere.
#[test]
fn normal_roundtrip_stays_within_quantization_error() {
    let normals = [
        [0.0f32, 0.0, 1.0],
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.6, 0.8, 0.0],
        [-0.6, 0.0, -0.8],
        [0.36, -0.48, 0.8],
    ];
    let encoded: Vec<_> = compress_normals(&normals).collect();
    assert_eq!(encoded.len(), normals.len());

    // Snorm16 carries a direction to about `1 / i16::MAX` radians; allow
    // that plus normalization slack.
    let tolerance = 1e-3;
    for (normal, packed) in normals.iter().zip(&encoded) {
        let decoded = decode_normal(*packed);
        let expected = glam::Vec3::from(*normal).normalize();
        assert!(
            (decoded - expected).abs().max_element() <= tolerance,
            "{decoded} vs {expected}"
        );
    }
}

/// A normal has no per-mesh range, so compressing one is self-contained:
/// every direction survives without a metadata array to read back.
#[test]
fn normals_need_no_decode_parameters() {
    let encoded: Vec<_> = compress_normals(&[[0.0, 0.0, 1.0], [0.0, 1.0, 0.0]]).collect();
    assert_eq!(decode_normal(encoded[0]), glam::Vec3::Z);
    assert_eq!(decode_normal(encoded[1]), glam::Vec3::Y);

    // A zero normal has no direction to encode; it collapses to the center
    // of the octahedron, which decodes to `+z`, rather than to a NaN.
    let zero: Vec<_> = compress_normals(&[[0.0, 0.0, 0.0]]).collect();
    assert_eq!(zero[0], [0, 0]);
    assert_eq!(decode_normal(zero[0]), glam::Vec3::Z);
}

#[test]
fn quantize_colors_clamp_out_of_range_components() {
    let colors: Vec<_> = quantize_colors(&[[0.0, 0.5, 1.0, 2.0], [-1.0, 0.0, 0.0, 0.0]]).collect();
    assert_eq!(colors[0], [0, 128, 255, 255]);
    assert_eq!(colors[1], [0, 0, 0, 0]);
}

#[test]
fn weights_clamp_out_of_range_components() {
    let weights: Vec<_> = compress_weights(&[[0.0, 0.5, 1.0, 2.0]]).collect();
    assert_eq!(weights[0], [0, 32_768, 65_535, 65_535]);
}

/// The joint pair follows the position in the same stream, in the shader's
/// attribute order, and a stream that declares no joints stays as narrow as
/// the position alone.
#[test]
fn position_stream_appends_the_joint_pair_after_the_position() {
    let positions = [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
    let joints = [[1u16, 2, 3, 4], [5, 6, 7, 8]];
    let weights: Vec<_> = compress_weights(&[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]).collect();

    let mut metadata = MeshMetadata::default();
    let plain = PositionStreamWriter {
        channels: PositionStreamChannels {
            position: Some(ChannelEncoding::CompressedPosition),
            joints: false,
        },
    };
    assert_eq!(plain.stride(), ChannelEncoding::CompressedPosition.size());

    let skinned = PositionStreamWriter {
        channels: PositionStreamChannels {
            position: Some(ChannelEncoding::CompressedPosition),
            joints: true,
        },
    };
    assert_eq!(
        skinned.stride(),
        ChannelEncoding::CompressedPosition.size() + JointPair::SIZE
    );

    let mut out = vec![0u8; skinned.byte_len(2)];
    skinned.write(
        &positions,
        joints.iter().copied(),
        weights.iter().copied(),
        &mut metadata,
        WriteOnly::from_mut(out.as_mut_slice()),
    );

    // The position comes first, exactly as the plain stream writes it, so
    // the joint pair cannot disturb the decode the metadata describes.
    let mut plain_out = vec![0u8; plain.byte_len(2)];
    plain.write(
        &positions,
        core::iter::empty(),
        core::iter::empty(),
        &mut MeshMetadata::default(),
        WriteOnly::from_mut(plain_out.as_mut_slice()),
    );
    let position_size = ChannelEncoding::CompressedPosition.size() as usize;
    let pair_size = JointPair::SIZE as usize;
    let stride = skinned.stride() as usize;
    for vertex in 0..2 {
        assert_eq!(
            &out[vertex * stride..vertex * stride + position_size],
            &plain_out[vertex * position_size..(vertex + 1) * position_size],
            "vertex {vertex} position"
        );
        let pair =
            &out[vertex * stride + position_size..vertex * stride + position_size + pair_size];
        assert_eq!(&pair[..8], joints[vertex].as_bytes());
        assert_eq!(&pair[8..], weights[vertex].as_bytes());
    }

    // A stream with neither a position nor joints writes nothing at all.
    let empty = PositionStreamWriter {
        channels: PositionStreamChannels {
            position: None,
            joints: false,
        },
    };
    assert_eq!(empty.stride(), 0);
    assert_eq!(empty.byte_len(2), 0);
}

#[test]
fn indices_reject_primitive_restart_and_overflow() {
    assert_eq!(
        compress_indices(&[0, 65_536]).err(),
        Some(CompressError::TooManyVertices(65_536))
    );
    // 0xFFFF is the primitive-restart value and cannot name a vertex.
    assert_eq!(
        compress_indices(&[0, u16::MAX as u32]).err(),
        Some(CompressError::TooManyVertices(u16::MAX as u32))
    );
    assert_eq!(compress_indices(&[]).err(), Some(CompressError::EmptyInput));

    let indices: Vec<_> = compress_indices(&[0, u16::MAX as u32 - 1])
        .expect("valid indices")
        .collect();
    assert_eq!(indices, vec![0, u16::MAX - 1]);
}

#[test]
fn empty_input_is_rejected() {
    let mut metadata = MeshMetadata::default();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compress_positions(&[], &mut metadata).count()
        }))
        .is_err(),
        "an empty position stream must be rejected"
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compress_uvs(&[], &mut metadata).count()
        }))
        .is_err(),
        "an empty UV stream must be rejected"
    );
}
