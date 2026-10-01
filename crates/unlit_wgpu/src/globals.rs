//! Uniforms the built-in pipelines bind.
//!
//! [`View`] and [`Globals`] are frame-wide and are mirrored by the
//! `view.wesl` / `globals.wesl` modules of the built-in WESL package;
//! [`MaterialCutoff`] is one material's own value, mirrored by
//! `material.wesl`. Every struct is bound as `var<uniform>`.

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
    pub clip_from_world: glam::Mat4,
    /// World-space eye position; `w` is padding by convention (write `1.0`).
    pub camera_position: glam::Vec4,
}

impl View {
    /// Build a view from a world-to-clip matrix and a world-space eye
    /// position.
    pub fn new(clip_from_world: glam::Mat4, camera_position: glam::Vec3) -> Self {
        Self {
            clip_from_world,
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

/// The alpha a material's fragments are cut off at.
///
/// Mirrors `material.wesl::MaterialCutoff`, and is bound by the
/// built-in unlit variant that carries
/// [`UnlitFlags::ALPHA_CUTOFF`](crate::pipeline::UnlitFlags::ALPHA_CUTOFF): a
/// fragment whose alpha is below the value is discarded, which is how a
/// material with a binary alpha — foliage, a grille, a cut-out label — draws
/// without blending and without depth sorting.
///
/// The value is a uniform rather than a part of the pipeline's variant so that
/// one pipeline serves every cutoff: a variant is part of the pipeline key, so
/// a cutoff baked into it would compile one pipeline per distinct value.
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
pub struct MaterialCutoff {
    /// The alpha a fragment has to reach to be drawn.
    pub cutoff: f32,
    /// Explicit tail padding: the WGSL struct size rounds up to 16 bytes.
    pub pad0: f32,
    /// Explicit tail padding, continued.
    pub pad1: f32,
    /// Explicit tail padding, continued.
    pub pad2: f32,
}

impl MaterialCutoff {
    /// The value one material is cut off at.
    pub fn new(cutoff: f32) -> Self {
        Self {
            cutoff,
            pad0: 0.0,
            pad1: 0.0,
            pad2: 0.0,
        }
    }
}

impl From<f32> for MaterialCutoff {
    fn from(cutoff: f32) -> Self {
        Self::new(cutoff)
    }
}
