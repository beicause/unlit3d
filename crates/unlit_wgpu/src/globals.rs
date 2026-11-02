//! Uniforms the built-in pipelines bind.
//!
//! Three records, each advanced by whoever owns the value it carries:
//! `Camera` by the camera a pass draws through, `Globals` by the renderer once a
//! frame, and `View` by the pass that states the viewport. They are mirrored by
//! the `camera.wesl`, `globals.wesl` and `view.wesl` modules of the built-in WESL
//! package, and all three are bound as `var<uniform>`.

/// Camera state: the transforms a pass draws through, and the eye they imply.
///
/// Mirrors `camera.wesl::Camera`; the layout is checked at compile time against
/// the WGSL uniform address-space rules.
///
/// A pass binds this for the camera it draws through, so two passes through two
/// cameras bind two records. What the pass frames — its viewport, and the pixel
/// scale that follows from it — is [`View`] rather than part of this record.
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
pub struct Camera {
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

impl Camera {
    /// Build a camera from its view and projection transforms and its
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

    /// Build a camera from a world-to-clip matrix alone.
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

/// What one pass frames: the region of the target its clip space covers.
///
/// Mirrors `view.wesl::View`.
///
/// This is the pass's value rather than the camera's: the same camera drawn into
/// two differently shaped rectangles is two views, and the passes of one frame
/// may frame differently. A shader that measures in pixels — a constant-width
/// line, a pixel-sized billboard, a screen-space projection — needs it to turn
/// a pixel count into clip space.
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
    /// The region of the target this view's clip space is stretched across, in
    /// physical pixels.
    ///
    /// One pixel is `2 / viewport_size` on each axis. It is the viewport rather
    /// than the attachment's size because a letterboxed view stretches its clip
    /// space across the smaller rectangle, so a width measured against the
    /// attachment would shrink along with the picture.
    ///
    /// Zero for a caller that never set it, which no pixel measurement can
    /// divide by; such a caller does not read it.
    pub viewport_size: glam::Vec2,
    /// Explicit tail padding: the WGSL struct size rounds up to 16 bytes, and a
    /// record with implicit padding could not be written out as bytes.
    pub pad0: glam::Vec2,
}

impl View {
    /// The view of a pass drawing through a `width` x `height` viewport, in
    /// physical pixels.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            viewport_size: glam::Vec2::new(width, height),
            pad0: glam::Vec2::ZERO,
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
