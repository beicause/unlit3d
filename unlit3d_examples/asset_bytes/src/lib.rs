#![doc = include_str!("../README.md")]

/// The skinned fox the `gltf` scene draws, carried in the binary so the scene
/// needs no file at run time.
///
/// `#[inline(never)]` keeps the bytes out of this crate's metadata: an
/// inlinable body carries what it embeds along with it.
#[inline(never)]
pub fn fox() -> &'static [u8] {
    include_bytes!("../../../unlit3d_asset_files/assets/Fox.glb")
}

/// The morph stress test the `gltf` scene draws, carried the same way.
#[inline(never)]
pub fn morph_stress_test() -> &'static [u8] {
    include_bytes!("../../../unlit3d_asset_files/assets/MorphStressTest.glb")
}
