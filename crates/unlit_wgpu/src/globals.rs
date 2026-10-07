//! Uniforms the built-in pipelines bind.
//!
//! [`View`] and [`Globals`] are frame-wide and are mirrored by the
//! `view.wesl` / `globals.wesl` modules of the built-in WESL package. Both are
//! bound as `var<uniform>`.

/// Camera state for one view.
///
/// Mirrors `view.wesl::View`; the layout is checked at compile time against
/// the WGSL uniform address-space rules.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayoutCompat,
)]
pub struct View {
    /// World -> clip matrix (`projection * view`).
    ///
    /// The one matrix a vertex stage needs; it is stored rather than left for
    /// the shader to multiply so the hottest path pays a single matrix
    /// multiply per vertex.
    pub clip_from_world: glam::Mat4,
    /// World -> view matrix.
    ///
    /// A shader that lights, fogs or projects in view space reads this; the
    /// camera's right, up and backward axes are its first three rows.
    pub view_from_world: glam::Mat4,
    /// World-space eye position; `w` is padding by convention (write `1.0`).
    pub camera_position: glam::Vec4,
}

impl View {
    /// Build a view from a camera's view and projection transforms and its
    /// world-space eye position.
    ///
    /// The combined world-to-clip matrix is derived here, so a caller only
    /// hands over the two transforms it holds.
    pub fn new(
        view_from_world: glam::Mat4,
        clip_from_view: glam::Mat4,
        camera_position: glam::Vec3,
    ) -> Self {
        Self {
            clip_from_world: clip_from_view * view_from_world,
            view_from_world,
            camera_position: camera_position.extend(1.0),
        }
    }

    /// Build a view from a world-to-clip matrix alone.
    ///
    /// For a caller whose matrix is synthesised and has no separate view and
    /// projection — the UI's screen-space mapping is one — the view matrix is
    /// left at the identity, so `view_from_world` says nothing and only
    /// `clip_from_world` should be read.
    pub fn from_clip_from_world(clip_from_world: glam::Mat4, camera_position: glam::Vec3) -> Self {
        Self {
            clip_from_world,
            view_from_world: glam::Mat4::IDENTITY,
            camera_position: camera_position.extend(1.0),
        }
    }
}

/// Values the renderer advances once per frame.
///
/// Mirrors `globals.wesl::Globals`.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    zerocopy_derive::FromBytes,
    zerocopy_derive::Immutable,
    zerocopy_derive::IntoBytes,
    zerocopy_derive::KnownLayout,
    const_shader_layout::ShaderLayoutCompat,
)]
pub struct Globals {
    /// Seconds elapsed since the renderer started.
    pub time: f32,
    /// Seconds elapsed since the previous frame.
    pub delta_time: f32,
    /// Monotonic frame counter.
    pub frame_count: u32,
    /// Explicit tail padding: the WGSL struct size rounds up to 16 bytes.
    pub pad0: u32,
}

impl Globals {
    /// Advance the clock by `delta_time` and return the new value.
    pub fn advance(&mut self, delta_time: f32) {
        self.time += delta_time;
        self.delta_time = delta_time;
        self.frame_count += 1;
    }
}

impl Default for Globals {
    fn default() -> Self {
        Self {
            time: 0.0,
            delta_time: 0.0,
            frame_count: 0,
            pad0: 0,
        }
    }
}
