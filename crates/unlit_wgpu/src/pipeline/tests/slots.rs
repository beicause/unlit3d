//! The vertex-slot tests: each channel's slot width follows its format.

use super::*;

#[test]
fn vertex_channels_for_vertex_buffer_maps_each_slot() {
    use crate::specialize::VertexBufferLayoutDesc;

    let attribute = |format, shader_location| wgpu::VertexAttribute {
        format,
        offset: 0,
        shader_location,
    };
    let layout = |step_mode, attributes: &[wgpu::VertexAttribute]| VertexBufferLayoutDesc {
        array_stride: 8,
        step_mode,
        attributes: attributes.into(),
    };
    let channels =
        |slot, layout: &VertexBufferLayoutDesc| LitVertexChannels::for_vertex_buffer(slot, layout);
    let position_of =
        |slot, layout: &VertexBufferLayoutDesc| channels(slot, layout).position.position;

    // The position slot: presence, and the compressed/uncompressed split.
    let compressed = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION)],
    );
    assert_eq!(
        position_of(PRIMARY_SLOT, &compressed),
        Some(ChannelEncoding::CompressedPosition)
    );
    let uncompressed = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x3, location::POSITION)],
    );
    assert_eq!(
        position_of(PRIMARY_SLOT, &uncompressed),
        Some(ChannelEncoding::UncompressedPosition)
    );

    // The joint pair rides the position stream, so it is this slot's answer
    // rather than a slot of its own.
    let skinned_layout = layout(
        wgpu::VertexStepMode::Vertex,
        &[
            attribute(wgpu::VertexFormat::Snorm16x4, location::POSITION),
            attribute(wgpu::VertexFormat::Uint32, location::JOINTS),
        ],
    );
    let skinned = channels(PRIMARY_SLOT, &skinned_layout);
    assert_eq!(
        skinned.position.position,
        Some(ChannelEncoding::CompressedPosition)
    );
    assert!(
        skinned.position.joints,
        "the joint indices are the evidence"
    );

    // The secondary slot: each channel is independent.
    let secondary = layout(
        wgpu::VertexStepMode::Vertex,
        &[
            attribute(wgpu::VertexFormat::Snorm16x2, location::UV),
            attribute(wgpu::VertexFormat::Unorm8x4, location::COLOR),
            attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL),
        ],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &secondary).secondary,
        SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL
    );
    let normal_only = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Snorm16x2, location::NORMAL)],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &normal_only).secondary,
        SecondaryFlags::NORMAL
    );
    let uncompressed_uv = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x2, location::UV)],
    );
    assert_eq!(
        channels(SECONDARY_SLOT, &uncompressed_uv).secondary,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
    );

    // The instance slot contributes nothing: the per-instance record is not
    // a vertex channel.
    let instance = layout(
        wgpu::VertexStepMode::Instance,
        &[attribute(
            wgpu::VertexFormat::Uint32,
            location::METADATA_INDEX,
        )],
    );
    assert_eq!(
        channels(INSTANCE_SLOT, &instance),
        LitVertexChannels::empty(),
        "the per-instance record is not a vertex channel"
    );

    // A whole layout is the union of its slots, and a slot that is not one of
    // the pipeline's own contributes nothing either way.
    let foreign = layout(
        wgpu::VertexStepMode::Vertex,
        &[attribute(wgpu::VertexFormat::Float32x3, 9)],
    );
    let whole = LitVertexChannels::for_vertex_layout(&[
        (PRIMARY_SLOT, skinned_layout),
        (SECONDARY_SLOT, uncompressed_uv),
        (INSTANCE_SLOT, instance),
        (7, foreign),
    ]);
    assert_eq!(
        whole.position.position,
        Some(ChannelEncoding::CompressedPosition)
    );
    assert!(whole.position.joints);
    assert_eq!(
        whole.secondary,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV
    );
}

#[test]
fn composes_each_variant_to_valid_wgsl() {
    for mut candidate in all_variants() {
        // Lighting is the one dimension the variant's channels do not already
        // cover: a lit draw reads the frame's lights and clusters with them,
        // which is why both are compiled here.
        for unlit in [false, true] {
            candidate.options.update(|options| options.unlit = unlit);
            // The array path is the device's, so both are exercised here.
            for texel_arrays in [false, true] {
                let wgsl = compose_shader(&candidate, texel_arrays)
                    .unwrap_or_else(|error| panic!("variant {candidate:?} failed: {error}"));
                assert!(wgsl.contains("fn vs_main"), "variant {candidate:?}");
                assert!(wgsl.contains("fn fs_main"), "variant {candidate:?}");
                // Disabled features must leave no conditional attributes behind.
                assert!(!wgsl.contains("@if"), "variant {candidate:?} kept @if");

                let vertex = candidate.channels;
                // Each channel appears exactly when the geometry carries it.
                assert_eq!(
                    wgsl.contains("base_color_tex"),
                    candidate.base_color_texture,
                    "variant {candidate:?}"
                );
                let compressed_uv = vertex.secondary.contains(SecondaryFlags::UV)
                    && !vertex.secondary.contains(SecondaryFlags::UNCOMPRESSED_UV);
                assert_eq!(
                    wgsl.contains("decode_uv"),
                    compressed_uv,
                    "variant {candidate:?}"
                );
                let compressed_position = matches!(
                    vertex.position.position,
                    Some(ChannelEncoding::CompressedPosition)
                );
                assert_eq!(
                    wgsl.contains("decode_position"),
                    compressed_position,
                    "variant {candidate:?}"
                );
                // A normal is decoded only where it shades, and only a lit
                // fragment shades with one.
                assert_eq!(
                    wgsl.contains("decode_normal"),
                    !candidate.options.unlit && vertex.secondary.contains(SecondaryFlags::NORMAL),
                    "variant {candidate:?}"
                );
                // The light arrays, the cluster lookup and the shading terms are
                // a lit draw's own.
                assert_eq!(
                    wgsl.contains("fragment_cluster_index"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_cell_word") && wgsl.contains("load_light"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("polygon_form_factor"),
                    !candidate.options.unlit,
                    "variant {candidate:?}"
                );
                // Every array read goes through an accessor, whose name survives
                // assembly unmangled as a suffix: the binding it reads is mangled
                // with its module path, so the accessor is what a variant can be
                // checked against.
                assert_eq!(
                    wgsl.contains("load_mesh_metadata"),
                    candidate.needs_metadata(),
                    "variant {candidate:?}"
                );
                // The pose arrays are the frame's, so they live in the global
                // group alongside the camera and the metadata.
                assert_eq!(
                    wgsl.contains("load_joint_matrix"),
                    candidate.needs_joints(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_morph_weight"),
                    candidate.needs_morphs(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    wgsl.contains("load_morph_delta"),
                    candidate.needs_morphs(),
                    "variant {candidate:?}"
                );
                // The path decides the resource type: storage buffers when the
                // device has them, textures when it does not. Neither path ever
                // leaves the other's declarations behind.
                let reads_an_array = candidate.needs_metadata()
                    || candidate.needs_joints()
                    || !candidate.options.unlit;
                if texel_arrays {
                    assert_eq!(
                        wgsl.matches("var<storage, read>").count(),
                        0,
                        "variant {candidate:?} kept a storage array"
                    );
                }
                assert_eq!(
                    wgsl.contains("texel_of"),
                    texel_arrays && reads_an_array,
                    "variant {candidate:?}"
                );
                // Without a position stream the vertex input declares no position
                // attribute, so the shader cannot read one.
                let vertex_input = wgsl
                    .split_once("struct VertexInput {")
                    .expect("the variant declares vertex inputs")
                    .1;
                let vertex_input = vertex_input
                    .split_once('}')
                    .expect("the vertex input struct ends")
                    .0;
                assert_eq!(
                    vertex_input.contains("position:"),
                    vertex.position.position.is_some(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    vertex_input.contains("joints:") && vertex_input.contains("weights:"),
                    candidate.needs_joints(),
                    "variant {candidate:?}"
                );
                assert_eq!(
                    vertex_input.contains("normal:"),
                    vertex.secondary.contains(SecondaryFlags::NORMAL),
                    "variant {candidate:?}"
                );
                // The instance record is unconditional, so its fields are
                // declared as plain attributes with no condition on them.
                assert!(
                    vertex_input.contains("joints_base:"),
                    "variant {candidate:?} declares no instance stream"
                );
                assert!(
                    vertex_input.contains("morph_base:"),
                    "variant {candidate:?} declares no instance stream"
                );
            }
        }
    }
}

/// A variant that contradicts itself must be rejected rather than composed
/// into a shader that cannot work.
///
/// The impossible states are no longer flags disagreeing with each other: an
/// encoding is a property of the channel it encodes, and the instance stream
/// is always bound, so the rejections left are the ones where a resource is
/// read that the variant does not carry — a texture sampled without a UV, a
/// deformation without a position, and a luminance layout without a texel.
#[test]
fn contradictory_variants_are_rejected() {
    let shape = LitOptions::standard;
    let compressed = Some(ChannelEncoding::CompressedPosition);
    let mut luminance_without_a_texture = shape();
    luminance_without_a_texture.base_color = BaseColorChannels::Luminance;
    for candidate in [
        // The base-color texture is sampled with the per-vertex UV.
        variant(
            shape(),
            channels(compressed, false, SecondaryFlags::empty()),
            true,
            false,
        ),
        // A deformation displaces the position.
        variant(
            shape(),
            channels(None, true, SecondaryFlags::empty()),
            false,
            false,
        ),
        variant(
            shape(),
            channels(None, false, SecondaryFlags::empty()),
            false,
            true,
        ),
        // A luminance layout describes a texel that is sampled.
        variant(
            luminance_without_a_texture,
            channels(compressed, false, SecondaryFlags::UV),
            false,
            false,
        ),
    ] {
        let result = std::panic::catch_unwind(|| compose_shader(&candidate, false));
        assert!(result.is_err(), "variant {candidate:?} must be rejected");
    }
}

#[test]
fn vertex_strides_follow_the_attribute_formats() {
    let layouts = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::CompressedPosition),
            false,
            SecondaryFlags::UV,
        ),
        false,
        false,
    )
    .vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Snorm16x4.size());

    let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
    assert_eq!(secondary.array_stride, wgpu::VertexFormat::Snorm16x2.size());

    let instance = layouts[INSTANCE_SLOT as usize]
        .as_ref()
        .expect("instance slot");
    // Every variant reads the same instance buffer, so the stride is the
    // whole record even when the variant declares only some of its fields.
    assert_eq!(
        instance.array_stride,
        std::mem::size_of::<crate::mesh::MeshInstance>() as u64
    );
    assert_eq!(instance.step_mode, wgpu::VertexStepMode::Instance);
}

/// An uncompressed channel widens its slot to the full-precision format and
/// drops the metadata bindings it no longer needs — but the instance stream
/// is still bound, because every built-in variant binds one.
#[test]
fn uncompressed_channels_widen_their_slots() {
    let candidate = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::UncompressedPosition),
            false,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        ),
        true,
        false,
    );
    assert!(!candidate.needs_metadata());

    let layouts = candidate.vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x3.size());
    assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x3);

    let secondary = layouts[SECONDARY_SLOT as usize].as_ref().expect("uv slot");
    assert_eq!(
        secondary.array_stride,
        wgpu::VertexFormat::Float32x2.size() + wgpu::VertexFormat::Unorm8x4.size()
    );

    // The instance stream is bound for every variant, so the slot exists even
    // though these channels are read entirely from full-precision attributes.
    assert!(layouts[INSTANCE_SLOT as usize].is_some());
}

/// A planar channel narrows its slot to the two-component format and needs
/// no metadata either, exactly as any other full-precision position.
#[test]
fn planar_channels_narrow_their_slots() {
    let candidate = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::PlanarPosition),
            false,
            SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        ),
        true,
        false,
    );
    assert!(!candidate.needs_metadata());

    let layouts = candidate.vertex_buffer_layouts();
    let position = layouts[PRIMARY_SLOT as usize]
        .as_ref()
        .expect("position slot");
    assert_eq!(position.array_stride, wgpu::VertexFormat::Float32x2.size());
    assert_eq!(position.attributes[0].format, wgpu::VertexFormat::Float32x2);
}

#[test]
fn secondary_slot_follows_its_channels() {
    let stride = |secondary: SecondaryFlags| {
        variant(
            LitOptions::standard(),
            channels(None, false, secondary),
            false,
            false,
        )
        .vertex_buffer_layouts()[SECONDARY_SLOT as usize]
            .as_ref()
            .map(|layout| layout.array_stride)
    };

    let uv = wgpu::VertexFormat::Snorm16x2.size();
    let uv_uncompressed = wgpu::VertexFormat::Float32x2.size();
    let color = wgpu::VertexFormat::Unorm8x4.size();
    let normal = wgpu::VertexFormat::Snorm16x2.size();

    // No channel: the slot disappears entirely, so a variant never binds a
    // buffer the shader does not declare.
    assert_eq!(stride(SecondaryFlags::empty()), None);
    assert_eq!(stride(SecondaryFlags::UV), Some(uv));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV),
        Some(uv_uncompressed)
    );
    assert_eq!(stride(SecondaryFlags::COLOR), Some(color));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::COLOR),
        Some(uv + color)
    );
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR),
        Some(uv_uncompressed + color)
    );
    // A normal is always the same two lanes, whatever else the stream
    // carries, and it sits in attribute order last.
    assert_eq!(stride(SecondaryFlags::NORMAL), Some(normal));
    assert_eq!(
        stride(SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL),
        Some(uv + color + normal)
    );
}

/// The position slot follows its channels, so a position-less variant binds
/// no position buffer at all.
#[test]
fn position_slot_follows_its_channels() {
    let position = |position: Option<ChannelEncoding>, joints: bool| {
        variant(
            LitOptions::standard(),
            channels(position, joints, SecondaryFlags::empty()),
            false,
            false,
        )
        .vertex_buffer_layouts()[PRIMARY_SLOT as usize]
            .as_ref()
            .map(|layout| (layout.array_stride, layout.step_mode))
    };

    assert_eq!(
        position(None, false),
        None,
        "no position stream means no position slot"
    );
    assert_eq!(
        position(Some(ChannelEncoding::CompressedPosition), false),
        Some((
            wgpu::VertexFormat::Snorm16x4.size(),
            wgpu::VertexStepMode::Vertex
        ))
    );
    assert_eq!(
        position(Some(ChannelEncoding::UncompressedPosition), false),
        Some((
            wgpu::VertexFormat::Float32x3.size(),
            wgpu::VertexStepMode::Vertex
        ))
    );
}
