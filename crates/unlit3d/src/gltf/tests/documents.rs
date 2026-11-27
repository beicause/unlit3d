//! The documents the loader tests read, built byte by byte.
//!
//! A document states its JSON inline and its binary chunk through
//! [`add_view`] / [`glb`], so every test names exactly the glTF it exercises
//! and nothing has to be kept in the asset submodule.

use zerocopy::{Immutable, IntoBytes};

/// 4-aligns `bin`, records a bufferView for `bytes` and returns its index.
pub(super) fn add_view(
    bin: &mut Vec<u8>,
    views: &mut Vec<String>,
    bytes: &[u8],
    target: Option<u32>,
) -> usize {
    pad(bin);
    let target = target.map_or(String::new(), |target| format!(r#", "target": {target}"#));
    views.push(format!(
        r#"{{ "buffer": 0, "byteOffset": {}, "byteLength": {}{target} }}"#,
        bin.len(),
        bytes.len()
    ));
    bin.extend_from_slice(bytes);
    views.len() - 1
}

/// Pads `bin` to the four-byte multiple a GLB's `BIN` chunk states.
pub(super) fn pad(bin: &mut Vec<u8>) {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
}

/// Wraps a JSON document and its binary chunk into a GLB.
pub(crate) fn glb(json: String, bin: &[u8]) -> Vec<u8> {
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut glb = Vec::new();
    glb.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    glb.extend_from_slice(bin);
    glb
}

/// The little-endian bytes of `values`, the form a buffer view holds them in.
pub(super) fn bytes<T: IntoBytes + Immutable>(values: &[T]) -> Vec<u8> {
    values
        .iter()
        .flat_map(IntoBytes::as_bytes)
        .copied()
        .collect()
}

/// The triangle every document here draws: three vertices at the origin, at
/// `+X` and at `+Y`, wound counter-clockwise.
pub(super) const TRIANGLE_POSITIONS: [[f32; 3]; 3] =
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

/// The indices of [`TRIANGLE_POSITIONS`].
pub(super) const TRIANGLE_INDICES: [u16; 3] = [0, 1, 2];

/// A 1x1 RGBA PNG, embedded so a textured document has a real image to import.
pub(crate) const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xdf, 0xe0, 0xf0,
    0x1f, 0x00, 0x07, 0x00, 0x02, 0xbf, 0x2b, 0xd7, 0xc7, 0xe2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
