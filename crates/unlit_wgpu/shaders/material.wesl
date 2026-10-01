// material -- one material's own values, bound alongside its texture.
//
// Must stay in sync with `crate::globals::MaterialCutoff` (Rust side, which
// binds as `var<uniform>`); this module is validated by WESL/WGSL validation
// at compose time.

/// One material's alpha cutoff, bound under [ALPHA_CUTOFF].
public struct MaterialCutoff {
    /// The alpha a fragment has to reach to be drawn.
    cutoff: f32,
    /// Explicit WGSL tail padding (the struct size rounds up to 16).
    pad0: f32,
    /// Explicit WGSL tail padding, continued.
    pad1: f32,
    /// Explicit WGSL tail padding, continued.
    pad2: f32,
}
