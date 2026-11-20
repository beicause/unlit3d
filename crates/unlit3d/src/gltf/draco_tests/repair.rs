//! What importing leaves of the document: which validation failures survive,
//! what the mode becomes, and what a corrupt stream reports.

use super::fixture::*;
use crate::gltf::Error;
use crate::gltf::LitGltf;
use crate::gltf::draco;
use serde_json::json;

#[test]
fn a_draco_document_becomes_ordinary_accessors() {
    let bytes = build_glb(true, 4, 0, false);
    let gltf = LitGltf::from_bytes(&bytes).expect("a Draco GLB imports as validated accessors");

    assert!(
        !gltf
            .document
            .extensions_required()
            .any(|extension| extension == draco::EXTENSION),
        "the repaired document no longer requires Draco"
    );
    assert!(
        !gltf
            .document
            .extensions_used()
            .any(|extension| extension == draco::EXTENSION),
        "the repaired document no longer uses Draco"
    );
    let primitive = gltf
        .document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .next()
        .unwrap();
    assert!(
        primitive.extension_value(draco::EXTENSION).is_none(),
        "the primitive's Draco extension is consumed"
    );
    assert_eq!(
        primitive.mode(),
        gltf::mesh::Mode::Triangles,
        "the decoded triangle list is the mode an ordinary reader draws"
    );
    assert!(
        gltf.document
            .accessors()
            .all(|accessor| accessor.view().is_some()),
        "every accessor now has a buffer view"
    );
}

#[test]
fn a_validation_failure_without_draco_is_returned_unchanged() {
    let root = json!({
        "asset": {"version": "2.0"},
        "extensionsRequired": ["KHR_materials_not_supported"],
        "meshes": []
    });
    let bytes = glb(root.to_string(), &[]);
    let error = match LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an unsupported extension is rejected"),
    };
    assert!(matches!(error, Error::Parse(gltf::Error::Validation(_))));
}

#[test]
fn an_unrelated_validation_failure_on_a_draco_document_is_returned_unchanged() {
    // The extension is declared, but the document has no compressed primitive
    // to repair and its default scene index is out of range. The original
    // validation errors must be returned, including the bad scene.
    let root = json!({
        "asset": {"version": "2.0"},
        "extensionsRequired": ["KHR_draco_mesh_compression"],
        "scene": 1,
        "scenes": [{"nodes": []}],
        "meshes": []
    });
    let bytes = glb(root.to_string(), &[]);
    let error = match LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an out-of-range scene is rejected even on a Draco document"),
    };
    let Error::Parse(gltf::Error::Validation(errors)) = &error else {
        panic!("the original validation error is returned");
    };
    assert!(
        errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::Unsupported)),
        "Draco's own rejection is among the returned errors: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::IndexOutOfBounds)),
        "the out-of-range scene is among the returned errors"
    );
}

#[test]
fn an_unrelated_validation_failure_survives_the_repair() {
    // A compressed primitive is present, so the Draco errors are repaired, but
    // the out-of-range scene is not something Draco explains: re-validating the
    // repaired document must still report it.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["scene"] = json!(1);
    let bytes = glb(root.to_string(), &bin);
    let error = match LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("an out-of-range scene is rejected after the Draco repair"),
    };
    let Error::Parse(gltf::Error::Validation(errors)) = &error else {
        panic!("the re-validation error is returned");
    };
    assert!(
        errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::IndexOutOfBounds)),
        "the bad scene is reported after the repair: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|(_, kind)| matches!(kind, gltf::json::validation::Error::Unsupported)),
        "the Draco extension rejection is gone after the repair: {errors:?}"
    );
}

#[test]
fn a_triangle_strip_primitive_is_normalized_to_triangles() {
    // The extension permits TRIANGLE_STRIP, but Draco always decodes to a
    // triangle list. The repair rewrites the mode so the ordinary reader draws
    // exactly the decoded geometry.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["meshes"][0]["primitives"][0]["mode"] = json!(5);
    let bytes = glb(root.to_string(), &bin);
    let gltf = LitGltf::from_bytes(&bytes).expect("a strip-mode Draco GLB imports");
    let primitive = gltf
        .document
        .meshes()
        .next()
        .unwrap()
        .primitives()
        .next()
        .unwrap();
    assert_eq!(primitive.mode(), gltf::mesh::Mode::Triangles);
    assert_eq!(
        streams_of(&gltf)
            .indices
            .expect("the strip becomes a triangle list")
            .len(),
        FACES.len() * 3
    );
}

#[test]
fn a_corrupt_draco_stream_returns_an_error() {
    // Truncate the stream's buffer view: decoding must fail with a returned
    // error naming the extension, not panic on the unvalidated document.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["bufferViews"][0]["byteLength"] = json!(4);
    let bytes = glb(root.to_string(), &bin);
    let error = match LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("a truncated Draco stream is rejected"),
    };
    let Error::Parse(gltf::Error::Validation(errors)) = &error else {
        panic!("the decode failure is reported as a validation error");
    };
    assert!(
        errors
            .iter()
            .any(|(path, _)| path.as_str().contains(draco::EXTENSION)),
        "the error names the extension that failed to decode: {errors:?}"
    );
}
