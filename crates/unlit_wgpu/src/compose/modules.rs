//! Entry modules in the built-in WESL package.

/// The built-in mesh shader, also the mount point for caller replacements.
pub const LIT_MODULE: &str = "lit";
/// The built-in stroke shader, also the mount point for caller replacements.
pub const LINE_MODULE: &str = "line";
/// The built-in fullscreen vertex shader.
pub const FULLSCREEN_VERTEX_MODULE: &str = "fullscreen_vertex";
/// The built-in blit fragment shader.
pub const BLIT_FRAGMENT_MODULE: &str = "blit_fragment";
