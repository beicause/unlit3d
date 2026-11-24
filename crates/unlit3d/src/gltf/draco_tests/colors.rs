//! COLOR_0 across the widths an exporter may store it in.

use super::fixture::*;
use crate::gltf::{DracoPrimitiveError, Error, LitGltf};
use serde_json::json;

#[test]
fn normalized_integer_colors_are_rescaled_from_the_accessor_declaration() {
    // Real exporters store normalized COLOR_0 as raw integers in the stream
    // with the Draco normalized flag clear; only the accessor declares the
    // normalization. Decoding must bake it into the rewritten F32 values.
    let (root, bin) = build_parts_with(
        true,
        encode_square(0, u16_color_attribute()),
        json!({
            "componentType": 5123,
            "count": 4,
            "type": "VEC4",
            "normalized": true
        }),
        false,
    );
    let bytes = glb(root.to_string(), &bin);
    let gltf = LitGltf::from_bytes(&bytes)
        .expect("a normalized-integer Draco GLB imports as validated accessors");
    let colors = streams_of(&gltf)
        .colors
        .expect("the primitive declares COLOR_0");
    assert_eq!(
        colors,
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 127],
            [127, 127, 0, 63],
        ],
        "normalized colors are rescaled into 0..255"
    );
    assert!(
        colors.iter().any(|color| color[3] != 255),
        "alpha is not saturated to white"
    );
}

#[test]
fn normalized_u8_colors_are_rescaled_from_the_accessor_declaration() {
    let (root, bin) = build_parts_with(
        true,
        encode_square(0, u8_color_attribute()),
        json!({
            "componentType": 5121,
            "count": 4,
            "type": "VEC4",
            "normalized": true
        }),
        false,
    );
    let bytes = glb(root.to_string(), &bin);
    let gltf = LitGltf::from_bytes(&bytes)
        .expect("a normalized UNSIGNED_BYTE Draco GLB imports as validated accessors");
    let colors = streams_of(&gltf)
        .colors
        .expect("the primitive declares COLOR_0");
    assert_eq!(
        colors,
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 127],
            [127, 127, 0, 63],
        ],
        "normalized bytes are rescaled from 0..255"
    );
    assert!(
        colors.iter().any(|color| color[3] != 255),
        "alpha is not saturated to white"
    );
}

#[test]
fn a_short_draco_attribute_is_rejected_instead_of_zero_filled() {
    // The accessor declares VEC4 but the stream carries only three components.
    // Zero-filling the fourth would silently hide a malformed stream.
    let (mut root, bin) = build_parts(true, 3, 0, false);
    root["accessors"][3]["type"] = json!("VEC4");
    let bytes = glb(root.to_string(), &bin);
    let error = match LitGltf::from_bytes(&bytes) {
        Err(error) => error,
        Ok(_) => panic!("a Draco attribute with too few components is rejected"),
    };
    let Error::DracoPrimitive {
        mesh,
        primitive,
        detail,
    } = &error
    else {
        panic!("the malformed stream is reported against its primitive: {error:?}");
    };
    assert_eq!((*mesh, *primitive), (0, 0));
    assert_eq!(*detail, DracoPrimitiveError::TooFewComponents);
}

#[test]
fn extra_draco_components_are_dropped_rather_than_rejected() {
    // The reverse of the short-attribute case: the stream carries VEC4 but the
    // accessor declares VEC3. That is legal — glTF takes the first three
    // components — so the fourth is dropped instead of failing the load.
    let (mut root, bin) = build_parts(true, 4, 0, false);
    root["accessors"][3]["type"] = json!("VEC3");
    let bytes = glb(root.to_string(), &bin);
    let gltf =
        LitGltf::from_bytes(&bytes).expect("a stream wider than its accessor is still legal");
    let colors = streams_of(&gltf)
        .colors
        .expect("the primitive declares COLOR_0");
    assert_eq!(
        colors,
        [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ],
        "the dropped fourth component leaves an opaque color"
    );
}
