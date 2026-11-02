// camera -- the transforms one pass draws through.
//
// Must stay in sync with `crate::globals::Camera` (Rust side, which binds as
// `var<uniform>`); this module is validated by WESL/WGSL validation at compose
// time.
//
// The record is the camera alone. What a pass frames -- its viewport, and the
// pixel scale that follows from it -- is `view::View`, bound separately: the
// same camera drawn into two differently shaped rectangles is two views.

/// Camera uniforms shared by the vertex and fragment stages.
public struct Camera {
    /// World -> clip matrix (projection * view).
    clip_from_world: mat4x4<f32>,
    /// World -> view matrix; its first three rows are the camera's right, up
    /// and backward axes.
    view_from_world: mat4x4<f32>,
    /// xyz = world-space eye position; w is padding (write 1.0).
    camera_position: vec4<f32>,
}
