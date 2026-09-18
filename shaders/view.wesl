// view -- fixed camera/view uniform layout.
//
// Must stay in sync with `crate::globals::View` (Rust side, which binds as
// `var<uniform>`); this module is validated by WESL/WGSL validation at
// compose time.

/// Camera uniforms shared by the vertex and fragment stages.
public struct View {
    /// World -> clip matrix (projection * view).
    clip_from_world: mat4x4<f32>,
    /// xyz = world-space eye position; w is padding (write 1.0).
    camera_position: vec4<f32>,
}
