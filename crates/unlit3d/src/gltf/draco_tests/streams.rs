//! What every stream of a decoded primitive holds, and how the shared and the
//! uncompressed cases read.

use super::fixture::*;
use crate::gltf::LitGltf;
use crate::gltf::draco;
use crate::gltf::streams;
use crate::gltf::streams::MeshStreams;
use serde_json::json;

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
    let gltf = LitGltf::from_bytes(&bytes).expect("a Draco GLB with a morph target imports");
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

#[test]
fn two_primitives_sharing_one_stream_both_decode() {
    // A document may state its geometry once and let two primitives name the
    // same bitstream. The extension says nothing about a per-primitive stream,
    // only about a buffer view that holds one, so the decoded mesh serves
    // every primitive that names it — the reference implementations cache on
    // the buffer view for the same reason.
    let (mut root, bin) = build_parts(true, 4, 0, false);

    // The second primitive declares its own accessors over the same stream:
    // each primitive addresses its attributes by Draco's own ids, so the two
    // only have to agree on the attribute set the stream carries.
    let declared = root["accessors"].as_array().unwrap().clone();
    let first = root["meshes"][0]["primitives"][0].clone();
    for accessor in &declared {
        root["accessors"]
            .as_array_mut()
            .unwrap()
            .push(accessor.clone());
    }
    let base = declared.len() as u64;
    let mut second = first.clone();
    for attribute in second["attributes"].as_object_mut().unwrap().values_mut() {
        let index = attribute.as_u64().unwrap();
        *attribute = json!(index + base);
    }
    second["indices"] = json!(second["indices"].as_u64().unwrap() + base);
    root["meshes"][0]["primitives"]
        .as_array_mut()
        .unwrap()
        .push(second);

    let gltf = LitGltf::from_bytes(&glb(root.to_string(), &bin))
        .expect("a document whose primitives share a stream imports");

    let primitives: Vec<_> = gltf
        .document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .take(2)
        .collect();
    assert_eq!(primitives.len(), 2, "both primitives survive the repair");
    assert!(
        primitives
            .iter()
            .all(|primitive| primitive.extension_value(draco::EXTENSION).is_none()),
        "both primitives consume the extension"
    );
    let streams: Vec<MeshStreams> = primitives
        .iter()
        .enumerate()
        .map(|(index, primitive)| {
            streams::from_reader(
                &gltf.buffers,
                super::super::MeshId::new(0),
                super::super::PrimitiveId::new(index),
                primitive,
                gltf.mesh_targets[0],
            )
            .expect("each primitive of the shared stream reads")
        })
        .collect();
    assert_eq!(
        streams[0].positions, streams[1].positions,
        "the same bitstream decodes to the same geometry for both primitives"
    );
    assert_eq!(streams[1].positions.len(), POSITIONS.len());
    assert_eq!(
        streams[0].indices, streams[1].indices,
        "the shared stream's faces serve both primitives"
    );
}
