// globals -- fixed frame-wide uniform layout.
//
// Must stay in sync with `crate::globals::Globals` (Rust side, which binds as
// `var<uniform>`); this module is validated by WESL/WGSL validation at
// compose time.

/// Frame-wide values a renderer advances once per frame.
public struct Globals {
    /// Seconds since the renderer started.
    time: f32,
    /// Seconds since the previous frame.
    delta_time: f32,
    /// Monotonic frame counter.
    frame_count: u32,
    /// Explicit WGSL tail padding (the struct size rounds up to 16).
    pad0: u32,
}
