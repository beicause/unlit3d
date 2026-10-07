//! The JSON configuration a render reads and the command line overrides.

use serde::Deserialize;
use unlit3d::components::Transform;

/// A whole render: what to draw, how to frame it, and how big the image is.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The image size, the render size and the viewport scaling.
    pub output: OutputConfig,
    /// The camera the documents are seen through.
    pub camera: CameraConfig,
    /// The glTF documents to draw, in order.
    pub documents: Vec<DocumentConfig>,
}

/// The image size, the render size and the viewport scaling.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutputConfig {
    /// The output image size in pixels, as [width, height].
    ///
    /// Defaults to render_size when absent, so at least one of the two must be
    /// present.
    pub size: Option<(u32, u32)>,
    /// The render size the camera and viewport are based on, as [width, height].
    ///
    /// Defaults to size when absent. The viewport is this size scaled by scale
    /// and centred in the output.
    pub render_size: Option<(u32, u32)>,
    /// The factors scaling the render size into the output size, as
    /// [scale_x, scale_y]. Each must be positive.
    pub scale: (f32, f32),
    /// The multisample count of the render target: 1, 2, 4 or 8.
    pub samples: u32,
    /// Whether the render target has a depth buffer.
    pub depth: bool,
    /// The clear colour as [red, green, blue, alpha], each in 0..=1.
    pub clear: [f64; 4],
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            size: None,
            render_size: None,
            scale: (1.0, 1.0),
            samples: 1,
            depth: true,
            clear: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// The camera the documents are seen through.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CameraConfig {
    /// The eye position, as [x, y, z].
    pub eye: [f32; 3],
    /// The target position, as [x, y, z].
    pub target: [f32; 3],
    /// The up vector, as [x, y, z].
    pub up: [f32; 3],
    /// The vertical field of view in degrees.
    pub fov_y: f32,
    /// The near plane distance.
    pub z_near: f32,
    /// A view (world-to-view) matrix in column-major order.
    ///
    /// Set together with the projection matrix, these two override eye,
    /// target, up, fov_y and z_near when present. The camera keeps its view
    /// and projection apart, and a single combined matrix cannot be split back
    /// into them, so both must be given.
    pub view: Option<[f32; 16]>,
    /// A projection (view-to-clip) matrix in column-major order.
    ///
    /// The counterpart of the view matrix; both are needed together.
    pub projection: Option<[f32; 16]>,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            eye: [0.0, 1.2, 3.2],
            target: [0.0, 0.2, 0.0],
            up: [0.0, 1.0, 0.0],
            fov_y: 60.0,
            z_near: 0.1,
            view: None,
            projection: None,
        }
    }
}

/// One glTF document and how it enters the scene.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DocumentConfig {
    /// The path to the .gltf or .glb file.
    pub path: String,
    /// The transform every spawned node of the document is placed with.
    pub placement: Option<PlacementConfig>,
    /// The animation to sample, by index or by name.
    pub animation: Option<AnimationRef>,
    /// The animation time in seconds.
    pub time: f32,
}

/// A placement transform for a document's nodes.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PlacementConfig {
    /// The translation, as [x, y, z].
    pub translation: [f32; 3],
    /// The rotation quaternion, as [x, y, z, w].
    pub rotation: [f32; 4],
    /// The scale, as [x, y, z].
    pub scale: [f32; 3],
}

impl Default for PlacementConfig {
    fn default() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

impl PlacementConfig {
    /// The transform this placement describes.
    #[must_use]
    pub fn transform(&self) -> Transform {
        Transform {
            translation: glam::Vec3::from(self.translation),
            rotation: glam::Quat::from_xyzw(
                self.rotation[0],
                self.rotation[1],
                self.rotation[2],
                self.rotation[3],
            ),
            scale: glam::Vec3::from(self.scale),
        }
    }
}

/// Names an animation of a document, by index or by name.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum AnimationRef {
    /// The animation's index in the document.
    Index(usize),
    /// The animation's name.
    Name(String),
}
