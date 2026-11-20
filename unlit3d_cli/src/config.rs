//! The JSON configuration a render reads and the command line overrides.

use serde::Deserialize;
use unlit3d::components::Transform;

/// A whole render: what to draw, how to frame it, how big the image is, and
/// what lights it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The image size, the render size and the viewport scaling.
    pub output: OutputConfig,
    /// The camera the documents are seen through.
    pub camera: CameraConfig,
    /// The glTF documents to draw, in order.
    pub documents: Vec<DocumentConfig>,
    /// The lights the scene draws under.
    ///
    /// Omitting the section inherits every built-in light; stating it replaces
    /// the fields it names, so `"directional": []` draws no directional light.
    pub lights: LightsConfig,
}

/// The lights a scene draws under.
///
/// A configuration that omits the section, or omits a field of it, draws under
/// the corresponding part of [`LightsConfig::built_in`]; the section is a set of
/// per-field overrides, the same way the command line's options are. A field
/// that is stated replaces the built-in value of that field alone, so
/// `"directional": []` keeps the ambient term while dropping every directional
/// light, and `"ambient": [0.0, 0.0, 0.0]` drops the ambient term while keeping
/// them.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightsConfig {
    /// The ambient term as [red, green, blue] in linear light, added to every
    /// lit fragment. It carries no intensity of its own: the colour is the term,
    /// so `[0.0, 0.0, 0.0]` contributes nothing.
    #[serde(default = "built_in_ambient")]
    pub ambient: [f32; 3],
    /// One entry per directional light, in the order they are spawned.
    #[serde(default = "built_in_directional")]
    pub directional: Vec<DirectionalLightConfig>,
}

impl LightsConfig {
    /// The lights a configuration that says nothing about lighting draws under.
    ///
    /// One ambient term keeps a lit material off pure black, and a two-hue
    /// directional pair — an amber key raking from above, a blue fill facing the
    /// camera — gives a shape a readable form.
    ///
    /// These are the lights the reference glTF scene of [`unlit3d_examples`]
    /// draws under, kept in step with it by the two sharing the
    /// `snapshots/gltf` frames: the CLI's end-to-end test and the scene's
    /// snapshot test compare the same images, so either set drifting from the
    /// other fails a test. The scene states them in radians and adds them by
    /// hand, which is why the agreement is a test rather than a shared value.
    ///
    /// [`unlit3d_examples`]: https://github.com/beicause/unlit3d/tree/main/unlit3d_examples
    #[must_use]
    pub fn built_in() -> Self {
        Self {
            ambient: built_in_ambient(),
            directional: built_in_directional(),
        }
    }
}

impl Default for LightsConfig {
    fn default() -> Self {
        Self::built_in()
    }
}

/// The ambient term of the built-in light set.
fn built_in_ambient() -> [f32; 3] {
    [0.045, 0.045, 0.05]
}

/// The directional lights of the built-in light set.
fn built_in_directional() -> Vec<DirectionalLightConfig> {
    // The key light is tilted by 0.65 radians in the reference scene, which a
    // configuration states in degrees.
    let key = 0.65_f32.to_degrees();
    vec![
        DirectionalLightConfig {
            color: [1.0, 0.62, 0.15],
            intensity: 0.22,
            yaw: key,
            pitch: -key,
        },
        DirectionalLightConfig {
            color: [0.2, 0.45, 1.0],
            intensity: 0.11,
            yaw: 0.0,
            pitch: 0.0,
        },
    ]
}

/// A directional light: a colour, an intensity and the two angles that aim it.
///
/// A directional light shines along its transform's `-Z`, so its aim is a yaw
/// about the Y axis followed by a pitch about the X axis. A roll would turn the
/// light about its own axis and change nothing about where it points, so the
/// shape has no field for it.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DirectionalLightConfig {
    /// The light's linear colour, as [red, green, blue].
    pub color: [f32; 3],
    /// The light's dimensionless intensity.
    pub intensity: f32,
    /// The yaw about the Y axis, in degrees, applied before the pitch.
    pub yaw: f32,
    /// The pitch about the X axis, in degrees.
    pub pitch: f32,
}

impl Default for DirectionalLightConfig {
    fn default() -> Self {
        Self {
            color: [1.0, 1.0, 1.0],
            intensity: 1.0,
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

impl DirectionalLightConfig {
    /// The rotation this light shines with: a yaw about Y, then a pitch about X.
    #[must_use]
    pub fn rotation(&self) -> glam::Quat {
        glam::Quat::from_rotation_y(self.yaw.to_radians())
            * glam::Quat::from_rotation_x(self.pitch.to_radians())
    }

    /// The colour this light carries, as the vector a light component holds.
    #[must_use]
    pub fn color_vector(&self) -> glam::Vec4 {
        glam::Vec4::new(self.color[0], self.color[1], self.color[2], 0.0)
    }
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
    /// Where the document sits.
    ///
    /// The transform composes with the document's own node transforms, so a
    /// node that sits away from the document's origin keeps that offset: the
    /// placement moves the whole scene rather than each node independently.
    pub placement: Option<PlacementConfig>,
    /// The animation to sample, by index or by name.
    pub animation: Option<AnimationRef>,
    /// The animation time in seconds.
    pub time: f32,
}

/// Where a document sits: the transform composed onto the document's scene.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> Config {
        serde_json::from_str(json).expect("the configuration parses")
    }

    #[test]
    fn an_absent_lights_section_is_the_built_in_set() {
        assert_eq!(config("{}").lights, LightsConfig::built_in());
    }

    #[test]
    fn a_stated_light_field_replaces_only_that_field() {
        let lights = config(r#"{"lights": {"ambient": [0.5, 0.25, 0.0]}}"#).lights;
        assert_eq!(lights.ambient, [0.5, 0.25, 0.0]);
        assert_eq!(lights.directional, LightsConfig::built_in().directional);

        let lights = config(r#"{"lights": {"directional": []}}"#).lights;
        assert_eq!(lights.ambient, LightsConfig::built_in().ambient);
        assert!(lights.directional.is_empty());
    }

    #[test]
    fn unknown_light_fields_are_refused() {
        assert!(
            serde_json::from_str::<Config>(r#"{"lights": {"directional": [{"yaw": 1.0}]}}"#)
                .is_ok()
        );
        assert!(serde_json::from_str::<Config>(r#"{"lights": {"distances": [1.0]}}"#).is_err());
    }
}
