// view -- fixed camera/view uniform layout.
//
// Must stay in sync with `crate::globals::View` (Rust side, which binds as
// `var<uniform>`); this module is validated by WESL/WGSL validation at
// compose time.

/// Camera uniforms shared by the vertex and fragment stages.
public struct View {
    /// World -> clip matrix (projection * view).
    clip_from_world: mat4x4<f32>,
    /// World -> view matrix; its first three rows are the camera's right, up
    /// and backward axes.
    view_from_world: mat4x4<f32>,
    /// xyz = world-space eye position; w is padding (write 1.0).
    camera_position: vec4<f32>,
    /// The region of the target this clip space is stretched across, in
    /// physical pixels: one pixel is `2 / viewport_size` on each axis. A
    /// shader that measures in pixels — a constant-width line — reads this;
    /// the viewport rather than the attachment, so a letterboxed view keeps
    /// its pixel sizes.
    viewport_size: vec2<f32>,
    /// Explicit tail padding, matching the Rust struct's own.
    pad0: vec2<f32>,
}
