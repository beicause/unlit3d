// view -- what one pass frames, as a uniform.
//
// Must stay in sync with `crate::globals::View` (Rust side, which binds as
// `var<uniform>`); this module is validated by WESL/WGSL validation at compose
// time.
//
// This is the pass's value rather than the camera's: a letterboxed pass
// stretches its clip space across its own rectangle, so a shader measuring in
// pixels divides by that rectangle's size, not by the attachment's.

/// The region one pass's clip space is stretched across.
public struct View {
    /// The viewport in physical pixels: one pixel is `2 / viewport_size` on
    /// each axis. A shader that measures in pixels -- a constant-width line --
    /// reads this; the viewport rather than the attachment, so a letterboxed
    /// view keeps its pixel sizes.
    viewport_size: vec2<f32>,
    /// Explicit tail padding, matching the Rust struct's own.
    pad0: vec2<f32>,
}
