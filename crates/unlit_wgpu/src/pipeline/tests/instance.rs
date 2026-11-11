//! The instance-slot tests: attribute offsets against the instance struct.

use super::*;

/// The instance slot's GPU attribute offsets and stride must match the CPU
/// struct. This is the vertex-buffer equivalent of the shader-layout
/// validation that `const_shader_layout` gives the uniform and storage
/// structs: vertex attributes are addressed by the offsets declared here, not
/// by WGSL alignment rules.
#[test]
fn instance_vertex_layout_matches_the_struct() {
    use crate::mesh::MeshInstance;
    use core::mem::{offset_of, size_of};

    let layouts = variant(
        LitOptions::standard(),
        channels(
            Some(ChannelEncoding::CompressedPosition),
            false,
            SecondaryFlags::empty(),
        ),
        false,
        false,
    )
    .vertex_buffer_layouts();
    let instance = layouts[INSTANCE_SLOT as usize]
        .as_ref()
        .expect("instance slot");

    assert_eq!(
        instance.array_stride,
        size_of::<MeshInstance>() as u64,
        "the instance stride must step exactly one MeshInstance"
    );

    let model = offset_of!(MeshInstance, model) as u64;
    let vec4 = size_of::<[f32; 4]>() as u64;
    // The record has to be tightly packed: `IntoBytes` rejects a type with
    // padding, so the sum of the record's field formats is the whole
    // stride. This variant neither cuts nor expands, so its layout omits
    // both words the shader would not declare; the declared formats plus
    // those two omitted words still cover the record exactly.
    let declared: u64 = instance
        .attributes
        .iter()
        .map(|attribute| attribute.format.size())
        .sum();
    assert_eq!(
        declared + 2 * wgpu::VertexFormat::Float32.size(),
        instance.array_stride,
        "the instance record carries no padding"
    );

    let expected = [
        model,
        model + vec4,
        model + 2 * vec4,
        offset_of!(MeshInstance, base_color) as u64,
        offset_of!(MeshInstance, joints_base) as u64,
        offset_of!(MeshInstance, metadata_index) as u64,
        offset_of!(MeshInstance, morph_base) as u64,
    ];
    let actual: Vec<u64> = instance.attributes.iter().map(|a| a.offset).collect();
    assert_eq!(
        actual, expected,
        "attribute offsets must line up with the MeshInstance fields"
    );

    let locations: Vec<u32> = instance
        .attributes
        .iter()
        .map(|a| a.shader_location)
        .collect();
    assert_eq!(
        locations,
        [
            location::MODEL_0,
            location::MODEL_1,
            location::MODEL_2,
            location::BASE_COLOR,
            location::JOINTS_BASE,
            location::METADATA_INDEX,
            location::MORPH_BASE,
        ]
    );
    // The matrix columns are floats, the color is quantized, and the two
    // bases and the metadata index are integers: each addresses a slot in
    // one of the frame's shared arrays.
    let expected_formats = [
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Float32x4,
        wgpu::VertexFormat::Unorm8x4,
        wgpu::VertexFormat::Uint32,
        wgpu::VertexFormat::Uint32,
        wgpu::VertexFormat::Uint32,
    ];
    let formats: Vec<wgpu::VertexFormat> = instance.attributes.iter().map(|a| a.format).collect();
    assert_eq!(formats, expected_formats, "{instance:?}");
}

/// A cutting variant's instance layout declares the cutoff and an expanding
/// one declares the hull width; a plain one omits both, matching the
/// shader's own conditional input declarations. The other instance inputs
/// are unconditional in the shader, so the layout declares them for every
/// variant.
#[test]
fn instance_layout_declares_the_cutoff_and_the_hull_width_only_when_read() {
    let locations = |alpha_cutoff: bool, expanded: bool| {
        let mut options = LitOptions::standard();
        options.alpha_cutoff = alpha_cutoff;
        options.expanded = expanded;
        variant(
            options,
            channels(None, false, SecondaryFlags::COLOR | SecondaryFlags::NORMAL),
            false,
            false,
        )
        .vertex_buffer_layouts()[INSTANCE_SLOT as usize]
            .as_ref()
            .expect("instance slot")
            .attributes
            .iter()
            .map(|a| a.shader_location)
            .collect::<Vec<u32>>()
    };

    let plain = [
        location::MODEL_0,
        location::MODEL_1,
        location::MODEL_2,
        location::BASE_COLOR,
        location::JOINTS_BASE,
        location::METADATA_INDEX,
        location::MORPH_BASE,
    ];
    let mut cutting = plain.to_vec();
    cutting.insert(6, location::CUTOFF);
    let mut expanding = plain.to_vec();
    expanding.push(location::HULL_WIDTH);
    let mut both = cutting.clone();
    both.push(location::HULL_WIDTH);

    assert_eq!(
        locations(false, false),
        plain,
        "a plain variant declares neither the cutoff nor the hull width"
    );
    assert_eq!(
        locations(true, false),
        cutting,
        "a cutting variant declares the cutoff"
    );
    assert_eq!(
        locations(false, true),
        expanding,
        "an expanding variant declares the hull width"
    );
    assert_eq!(
        locations(true, true),
        both,
        "one variant can do both, and the width stays last"
    );
}

/// The hull width rides the instance stream, so the vertex stage reads the
/// instance attribute and pushes the vertex along the transformed normal.
#[test]
fn expanded_flag_pushes_the_vertex_along_its_normal() {
    let composed = |expanded: bool| {
        let mut options = LitOptions::standard();
        options.expanded = expanded;
        compose_shader(
            &variant(
                options,
                channels(
                    Some(ChannelEncoding::CompressedPosition),
                    false,
                    SecondaryFlags::NORMAL,
                ),
                false,
                false,
            ),
            false,
        )
        .expect("compose")
    };

    let plain = composed(false);
    assert!(
        !plain.contains("hull_width"),
        "a variant that expands by nothing declares no hull width"
    );

    let expanded = composed(true);
    assert!(
        expanded.contains("@location(14)\n    hull_width: f32"),
        "the shader reads the hull width from its instance attribute"
    );
    assert!(
        expanded.contains("let reach = length(outward);"),
        "a degenerate instance matrix is checked before it can be normalized"
    );
    assert!(
        expanded.contains("world += outward / reach * in.hull_width"),
        "the vertex is pushed along its transformed normal"
    );
}

/// A variant cannot expand without a direction to expand in, so composing
/// one over a mesh with no normal channel is a mistake worth stopping at.
#[test]
#[should_panic(expected = "`EXPANDED` requires a normal channel")]
fn an_expanding_variant_without_normals_is_rejected() {
    let mut options = LitOptions::standard();
    options.expanded = true;
    let _ = compose_shader(
        &variant(
            options,
            channels(
                Some(ChannelEncoding::CompressedPosition),
                false,
                SecondaryFlags::empty(),
            ),
            false,
            false,
        ),
        false,
    );
}
