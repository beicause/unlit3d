//! The documents a load refuses: a mesh whose primitives disagree about their
//! morph targets, primitives too short to draw, indices past their own
//! vertices, and images whose pixel buffer cannot fill their stated size.

use super::*;

/// A GLB whose single mesh states two primitives over one position accessor:
/// the first declares the morph target that accessor 1 holds, the second
/// declares none.
fn disagreeing_morph_targets_document() -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let position_view = documents::add_view(
        &mut bin,
        &mut views,
        &documents::bytes(&documents::TRIANGLE_POSITIONS),
        Some(34962),
    );
    let delta_view = documents::add_view(
        &mut bin,
        &mut views,
        &documents::bytes(&[[0.0f32, 1.0, 0.0]; 3]),
        None,
    );
    documents::pad(&mut bin);
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
            "scene":0,
            "scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[
                {{"attributes":{{"POSITION":0}},"targets":[{{"POSITION":1}}]}},
                {{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[
                {{"bufferView":{position_view},"componentType":5126,"count":3,"type":"VEC3",
                  "min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]}},
                {{"bufferView":{delta_view},"componentType":5126,"count":3,"type":"VEC3"}}],
            "bufferViews":[{}],
            "buffers":[{{"byteLength":{}}}]}}"#,
        views.join(", "),
        bin.len(),
    );
    documents::glb(json, &bin)
}

#[test]
fn primitives_of_one_mesh_must_agree_on_their_morph_targets() {
    // A weights channel states one value per target, so a mesh with primitives
    // that disagree has no single count to slice the channel with: the load is
    // refused rather than letting the two stages count differently.
    let error = match LitGltf::from_bytes(&disagreeing_morph_targets_document()) {
        Ok(_) => panic!("a mesh whose primitives disagree is refused"),
        Err(error) => error,
    };
    match error {
        super::Error::MorphTargetCountMismatch {
            mesh,
            primitive,
            targets,
            expected,
        } => {
            assert_eq!((mesh, primitive), (0, 1));
            assert_eq!(targets, 0);
            assert_eq!(expected, 1);
        }
        other => panic!("the mismatch is reported against its primitive: {other:?}"),
    }
}

#[test]
fn a_primitive_too_short_for_its_topology_is_refused() {
    // Two vertices cannot state a triangle, so uploading them would draw
    // something the document never stated.
    let gltf = LitGltf::from_bytes(&mode_document(
        gltf::mesh::Mode::Triangles,
        &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
        Some(&[0, 1]),
    ))
    .expect("the short document parses");
    let error = match super::streams::from_reader(&gltf.buffers, 0, 0, &gltf.primitive(0, 0), 0) {
        Ok(_) => panic!("a two-vertex triangle is refused"),
        Err(error) => error,
    };
    match error {
        super::Error::PrimitiveTooShort {
            mesh,
            primitive,
            mode,
            vertices,
        } => {
            assert_eq!((mesh, primitive), (0, 0));
            assert_eq!(mode, "TRIANGLES");
            assert_eq!(vertices, 2);
        }
        other => panic!("the short primitive is named: {other:?}"),
    }
}

#[test]
fn an_index_past_the_vertices_is_refused() {
    // The index stream names vertex 3 of a three-vertex primitive. Drawing it
    // would read a vertex the primitive does not carry.
    let gltf = LitGltf::from_bytes(&mode_document(
        gltf::mesh::Mode::Triangles,
        &documents::TRIANGLE_POSITIONS,
        Some(&[0, 1, 3]),
    ))
    .expect("the out-of-range index parses");
    let error = match super::streams::from_reader(&gltf.buffers, 0, 0, &gltf.primitive(0, 0), 0) {
        Ok(_) => panic!("an index past the vertices is refused"),
        Err(error) => error,
    };
    match error {
        super::Error::IndexOutOfRange {
            mesh,
            primitive,
            index,
            vertices,
        } => {
            assert_eq!((mesh, primitive), (0, 0));
            assert_eq!(index, 3);
            assert_eq!(vertices, 3);
        }
        other => panic!("the out-of-range index is named: {other:?}"),
    }
}

#[test]
fn an_image_whose_pixels_cannot_fill_its_size_is_refused() {
    // The glTF crate decodes an image's pixels without checking them against
    // the size it states, so the buffer is measured here: a short one would be
    // read past its end while the image is encoded.
    let bytes = sided_document(false);
    let parsed = gltf::Gltf::from_slice(&bytes).expect("the sided document parses");
    let document = parsed.document;
    let buffers = gltf::import_buffers(&document, None, parsed.blob).expect("the buffers import");
    let images = vec![gltf::image::Data {
        pixels: vec![0u8; 3],
        format: gltf::image::Format::R8G8B8A8,
        width: 2,
        height: 2,
    }];
    let error = match LitGltf::from_parts(document, buffers, images) {
        Ok(_) => panic!("an image short of its pixels is refused"),
        Err(error) => error,
    };
    match error {
        super::Error::ImagePixelsShort {
            image,
            width,
            height,
            texel,
            expected,
            actual,
        } => {
            assert_eq!((image, width, height, texel), (0, 2, 2, 4));
            assert_eq!(expected, 16);
            assert_eq!(actual, 3);
        }
        other => panic!("the short image is named: {other:?}"),
    }
}
