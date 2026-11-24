//! The streams a primitive is uploaded from: the topology its mode names, the
//! index list an unsupported mode expands into, and the UV set its material
//! samples through.

use super::documents::TRIANGLE_POSITIONS;
use super::*;
use gltf::mesh::Mode;

#[test]
fn every_mode_reaches_the_topology_that_draws_it() {
    // Six of the seven glTF modes are a wgpu topology under another name or
    // with a list expansion; only LINE_LOOP and TRIANGLE_FAN have no name of
    // their own.
    for (mode, topology, expanded) in [
        (Mode::Points, wgpu::PrimitiveTopology::PointList, false),
        (Mode::Lines, wgpu::PrimitiveTopology::LineList, false),
        (Mode::LineStrip, wgpu::PrimitiveTopology::LineStrip, false),
        (Mode::LineLoop, wgpu::PrimitiveTopology::LineList, true),
        (
            Mode::Triangles,
            wgpu::PrimitiveTopology::TriangleList,
            false,
        ),
        (
            Mode::TriangleStrip,
            wgpu::PrimitiveTopology::TriangleStrip,
            false,
        ),
        (
            Mode::TriangleFan,
            wgpu::PrimitiveTopology::TriangleList,
            true,
        ),
    ] {
        assert_eq!(topology::topology(mode), topology, "{mode:?}");
        assert_eq!(topology::needs_expansion(mode), expanded, "{mode:?}");
    }
}

#[test]
fn the_uploaded_primitive_draws_the_topology_its_mode_names() {
    let positions = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    for (mode, topology) in [
        (Mode::Triangles, wgpu::PrimitiveTopology::TriangleList),
        (Mode::TriangleStrip, wgpu::PrimitiveTopology::TriangleStrip),
        (Mode::TriangleFan, wgpu::PrimitiveTopology::TriangleList),
        (Mode::Lines, wgpu::PrimitiveTopology::LineList),
        (Mode::LineStrip, wgpu::PrimitiveTopology::LineStrip),
        (Mode::LineLoop, wgpu::PrimitiveTopology::LineList),
        (Mode::Points, wgpu::PrimitiveTopology::PointList),
    ] {
        let gltf = LitGltf::from_bytes(&mode_document(mode, &positions, Some(&[0, 1, 2, 3])))
            .expect("the mode document parses");
        assert_eq!(
            gltf.pipeline_key(&[], MeshId::new(0), PrimitiveId::new(0))
                .expect("the mode document samples no image")
                .options
                .primitive
                .topology,
            topology,
            "{mode:?}"
        );
    }
}

#[test]
fn a_triangle_fan_is_expanded_with_the_centre_last() {
    // glTF states a fan as {v_{i+1}, v_{i+2}, v_0}, so the shared vertex closes
    // every triangle: a fan turned into a list with the centre first would wind
    // each of them the other way round and cull the geometry.
    let positions = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let gltf = LitGltf::from_bytes(&mode_document(
        Mode::TriangleFan,
        &positions,
        Some(&[0, 1, 2, 3]),
    ))
    .expect("the fan document parses");
    let streams = super::streams::from_reader(
        &gltf.buffers,
        0,
        0,
        &gltf.primitive(0, 0),
        gltf.mesh_targets[0],
    )
    .expect("the primitive's streams read");
    assert_eq!(
        streams.indices.expect("the fan expands into a list"),
        vec![1, 2, 0, 2, 3, 0]
    );
}

#[test]
fn a_fan_without_indices_expands_its_implicit_stream() {
    let positions = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let gltf = LitGltf::from_bytes(&mode_document(Mode::TriangleFan, &positions, None))
        .expect("the fan document parses");
    let streams = super::streams::from_reader(
        &gltf.buffers,
        0,
        0,
        &gltf.primitive(0, 0),
        gltf.mesh_targets[0],
    )
    .expect("the primitive's streams read");
    assert_eq!(
        streams.indices.expect("the fan expands into a list"),
        vec![1, 2, 0, 2, 3, 0]
    );
}

#[test]
fn a_line_loop_is_expanded_back_to_its_first_vertex() {
    let gltf = LitGltf::from_bytes(&mode_document(
        Mode::LineLoop,
        &TRIANGLE_POSITIONS,
        Some(&[0, 1, 2]),
    ))
    .expect("the loop document parses");
    let streams = super::streams::from_reader(
        &gltf.buffers,
        0,
        0,
        &gltf.primitive(0, 0),
        gltf.mesh_targets[0],
    )
    .expect("the primitive's streams read");
    assert_eq!(
        streams.indices.expect("the loop expands into a list"),
        vec![0, 1, 1, 2, 2, 0],
        "the closing segment from the last vertex back to the first is what a line list lacks"
    );
}

#[test]
fn a_strip_reaches_the_draw_untouched() {
    // A strip's winding alternates with every triangle, which no index rewrite
    // reproduces: the rasterizer assembles it, so the stream is passed on as
    // the document wrote it.
    for mode in [Mode::TriangleStrip, Mode::LineStrip] {
        let gltf = LitGltf::from_bytes(&mode_document(mode, &TRIANGLE_POSITIONS, Some(&[0, 1, 2])))
            .expect("the strip document parses");
        let streams = super::streams::from_reader(
            &gltf.buffers,
            0,
            0,
            &gltf.primitive(0, 0),
            gltf.mesh_targets[0],
        )
        .expect("the primitive's streams read");
        assert_eq!(
            streams.indices.expect("the strip keeps its indices"),
            vec![0, 1, 2],
            "{mode:?}"
        );
    }
}

#[test]
fn a_primitive_samples_the_uv_set_its_material_names() {
    // The material names TEXCOORD_1 and the primitive carries both sets, each
    // holding a constant that names itself: a loader that always read
    // TEXCOORD_0 would upload 0.25 rather than 1.25.
    let gltf = LitGltf::from_bytes(&uv_document(1, &[0, 1])).expect("the UV document parses");
    let primitive = gltf.primitive(0, 0);
    assert_eq!(base_color_uv_set(&primitive), Some(1));
    let streams =
        super::streams::from_reader(&gltf.buffers, 0, 0, &primitive, gltf.mesh_targets[0])
            .expect("the primitive's streams read");
    let uvs = streams
        .uvs
        .expect("the primitive carries the set it samples");
    assert_eq!(uvs.len(), 3);
    for uv in &uvs {
        assert_eq!(*uv, [1.25, 0.5], "the stream is TEXCOORD_1's");
    }
}

#[test]
fn a_primitive_missing_the_set_its_material_names_draws_untextured() {
    // The accessors carry TEXCOORD_0 and TEXCOORD_1 while the material names
    // set 2, which the primitive does not have. The specification states no
    // fallback, so the draw drops the texture rather than failing the load or
    // sampling a set the document never meant to use.
    let gltf = LitGltf::from_bytes(&uv_document(2, &[0, 1])).expect("the UV document parses");
    let primitive = gltf.primitive(0, 0);
    assert_eq!(base_color_uv_set(&primitive), None);
    let streams =
        super::streams::from_reader(&gltf.buffers, 0, 0, &primitive, gltf.mesh_targets[0])
            .expect("the primitive's streams read");
    assert!(streams.uvs.is_none(), "no UV stream is uploaded");
}
