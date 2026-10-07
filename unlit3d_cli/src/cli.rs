//! The command line the binary parses.

use std::path::PathBuf;

use argh::FromArgs;

use crate::config::{AnimationRef, Config, DocumentConfig};

/// The program name argh prints in its usage.
const PROGRAM: &str = "unlit3d-cli";

/// The command-line arguments.
///
/// Every option overrides the same-named field of the JSON configuration.
#[derive(FromArgs, Debug, Clone, PartialEq)]
#[argh(
    help_triggers("-h", "--help"),
    description = "render glTF documents to an image"
)]
pub struct Args {
    /// a JSON configuration file. Command-line options override its fields.
    #[argh(option)]
    pub config: Option<PathBuf>,
    /// the output image path; its extension chooses the format (png, webp or jpeg).
    #[argh(option)]
    pub output: PathBuf,
    /// the output image size, as WxH.
    #[argh(option, from_str_fn(parse_size))]
    pub output_size: Option<(u32, u32)>,
    /// the render size the camera and viewport are based on, as WxH.
    #[argh(option, from_str_fn(parse_size))]
    pub render_size: Option<(u32, u32)>,
    /// the factors scaling the render size inside the output size, as S or SX,SY.
    #[argh(option, from_str_fn(parse_scale))]
    pub scale: Option<(f32, f32)>,
    /// the multisample count: 1, 2, 4 or 8.
    #[argh(option)]
    pub samples: Option<u32>,
    /// render without a depth buffer.
    #[argh(switch)]
    pub no_depth: bool,
    /// the clear colour, as R,G,B or R,G,B,A, each in 0..=1.
    #[argh(option, from_str_fn(parse_clear))]
    pub clear: Option<[f64; 4]>,
    /// the camera eye position, as X,Y,Z.
    #[argh(option, from_str_fn(parse_vec3))]
    pub eye: Option<[f32; 3]>,
    /// the camera target position, as X,Y,Z.
    #[argh(option, from_str_fn(parse_vec3))]
    pub target: Option<[f32; 3]>,
    /// the camera up vector, as X,Y,Z.
    #[argh(option, from_str_fn(parse_vec3))]
    pub up: Option<[f32; 3]>,
    /// the vertical field of view in degrees.
    #[argh(option)]
    pub fov_y: Option<f32>,
    /// the near plane distance.
    #[argh(option)]
    pub z_near: Option<f32>,
    /// a view (world-to-view) matrix, column-major, as 16 comma-separated
    /// numbers; give it together with --camera-projection-matrix.
    #[argh(option, from_str_fn(parse_matrix))]
    pub camera_view_matrix: Option<[f32; 16]>,
    /// a projection (view-to-clip) matrix, column-major, as 16 comma-separated
    /// numbers; give it together with --camera-view-matrix.
    #[argh(option, from_str_fn(parse_matrix))]
    pub camera_projection_matrix: Option<[f32; 16]>,
    /// a glTF document to render; repeatable. Replaces the configuration list.
    #[argh(option)]
    pub document: Vec<PathBuf>,
    /// the animation to play, by name or index; applies to every document.
    #[argh(option)]
    pub animation: Option<String>,
    /// the animation time in seconds.
    #[argh(option)]
    pub time: Option<f32>,
}

/// What parse decided the process should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Render with these arguments.
    Run(Box<Args>),
    /// Print this help text and exit successfully.
    Help(String),
}

impl Args {
    /// Read the configuration file, if any, and apply the overrides.
    ///
    /// # Errors
    ///
    /// Fails when the configuration file cannot be read or parsed.
    pub fn config(&self) -> Result<Config, String> {
        let mut config = match &self.config {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
                serde_json::from_str(&text)
                    .map_err(|error| format!("cannot parse {}: {error}", path.display()))?
            }
            None => Config::default(),
        };
        self.apply(&mut config);
        Ok(config)
    }

    /// Apply every command-line override to config.
    fn apply(&self, config: &mut Config) {
        if let Some(size) = self.output_size {
            config.output.size = Some(size);
        }
        if let Some(size) = self.render_size {
            config.output.render_size = Some(size);
        }
        if let Some(scale) = self.scale {
            config.output.scale = scale;
        }
        if let Some(samples) = self.samples {
            config.output.samples = samples;
        }
        if self.no_depth {
            config.output.depth = false;
        }
        if let Some(clear) = self.clear {
            config.output.clear = clear;
        }
        if let Some(eye) = self.eye {
            config.camera.eye = eye;
        }
        if let Some(target) = self.target {
            config.camera.target = target;
        }
        if let Some(up) = self.up {
            config.camera.up = up;
        }
        if let Some(fov_y) = self.fov_y {
            config.camera.fov_y = fov_y;
        }
        if let Some(z_near) = self.z_near {
            config.camera.z_near = z_near;
        }
        let named_camera_field = self.eye.is_some()
            || self.target.is_some()
            || self.up.is_some()
            || self.fov_y.is_some()
            || self.z_near.is_some();
        if self.camera_view_matrix.is_some() || self.camera_projection_matrix.is_some() {
            config.camera.view = self.camera_view_matrix;
            config.camera.projection = self.camera_projection_matrix;
        } else if named_camera_field {
            config.camera.view = None;
            config.camera.projection = None;
        }
        let animation = self.animation.as_deref().map(parse_animation);
        if self.document.is_empty() {
            for document in &mut config.documents {
                if let Some(animation) = &animation {
                    document.animation = Some(animation.clone());
                }
                if let Some(time) = self.time {
                    document.time = time;
                }
            }
        } else {
            config.documents = self
                .document
                .iter()
                .map(|path| DocumentConfig {
                    path: path.to_string_lossy().into_owned(),
                    placement: None,
                    animation: animation.clone(),
                    time: self.time.unwrap_or(0.0),
                })
                .collect();
        }
    }
}

/// Parse the command line into the process's outcome.
///
/// # Errors
///
/// Returns the error text when the arguments are invalid.
pub fn parse<I>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match Args::from_args(&[PROGRAM], &refs) {
        Ok(parsed) => Ok(Parsed::Run(Box::new(parsed))),
        Err(exit) if exit.status.is_ok() => Ok(Parsed::Help(usage())),
        Err(exit) => Err(exit.output),
    }
}

/// The full usage text, as argh prints it.
#[must_use]
pub fn usage() -> String {
    Args::from_args(&[PROGRAM], &["--help"])
        .expect_err("--help exits early")
        .output
}

/// Read an animation reference: a plain integer is an index, anything else a name.
fn parse_animation(text: &str) -> AnimationRef {
    text.parse::<usize>()
        .map_or_else(|_| AnimationRef::Name(text.to_owned()), AnimationRef::Index)
}

/// Parse a WxH size.
fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let invalid = || format!("{text} is not a WxH size");
    let (width, height) = text.split_once(['x', 'X']).ok_or_else(invalid)?;
    let width = width.trim().parse::<u32>().map_err(|_| invalid())?;
    let height = height.trim().parse::<u32>().map_err(|_| invalid())?;
    if width == 0 || height == 0 {
        return Err(format!("{text} has a zero dimension"));
    }
    Ok((width, height))
}

/// Parse one or two comma-separated factors; one factor applies to both axes.
fn parse_scale(text: &str) -> Result<(f32, f32), String> {
    let values = parse_floats(text)?;
    match values.as_slice() {
        [scale] => Ok((*scale, *scale)),
        [x, y] => Ok((*x, *y)),
        values => Err(format!("{text} needs 1 or 2 numbers, got {}", values.len())),
    }
}

/// Parse three comma-separated numbers.
fn parse_vec3(text: &str) -> Result<[f32; 3], String> {
    let values = parse_floats(text)?;
    let count = values.len();
    values
        .try_into()
        .map_err(|_| format!("{text} needs 3 numbers, got {count}"))
}

/// Parse sixteen comma-separated numbers.
fn parse_matrix(text: &str) -> Result<[f32; 16], String> {
    let values = parse_floats(text)?;
    let count = values.len();
    values
        .try_into()
        .map_err(|_| format!("{text} needs 16 numbers, got {count}"))
}

/// Parse a clear colour: three or four comma-separated numbers.
fn parse_clear(text: &str) -> Result<[f64; 4], String> {
    let values: Vec<f64> = text
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("{text} is not a comma-separated colour"))
        })
        .collect::<Result<_, _>>()?;
    match values.len() {
        3 => Ok([values[0], values[1], values[2], 1.0]),
        4 => Ok([values[0], values[1], values[2], values[3]]),
        count => Err(format!("{text} needs 3 or 4 numbers, got {count}")),
    }
}

/// Parse a comma-separated list of numbers.
fn parse_floats(text: &str) -> Result<Vec<f32>, String> {
    text.split(',')
        .map(|value| {
            value
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{text} is not a comma-separated number list"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(arguments: &[&str]) -> Args {
        Args::from_args(&[PROGRAM], arguments).expect("the arguments parse")
    }

    #[test]
    fn the_output_path_is_required() {
        assert!(Args::from_args(&[PROGRAM], &[]).is_err());
        assert_eq!(
            args(&["--output", "out.png"]).output,
            PathBuf::from("out.png")
        );
    }

    #[test]
    fn sizes_are_parsed_and_rejected() {
        assert_eq!(parse_size("256x192"), Ok((256, 192)));
        assert_eq!(parse_size("256X192"), Ok((256, 192)));
        assert!(parse_size("256").is_err());
        assert!(parse_size("0x192").is_err());
    }

    #[test]
    fn colours_accept_three_or_four_components() {
        assert_eq!(parse_clear("1,0.5,0"), Ok([1.0, 0.5, 0.0, 1.0]));
        assert_eq!(parse_clear("1,0.5,0,0.25"), Ok([1.0, 0.5, 0.0, 0.25]));
        assert!(parse_clear("1,0.5").is_err());
    }

    #[test]
    fn scale_accepts_one_or_two_factors() {
        assert_eq!(parse_scale("2"), Ok((2.0, 2.0)));
        assert_eq!(parse_scale("0.75,0.5"), Ok((0.75, 0.5)));
        assert!(parse_scale("1,2,3").is_err());
    }

    #[test]
    fn animation_references_distinguish_names_from_indices() {
        assert_eq!(parse_animation("2"), AnimationRef::Index(2));
        assert_eq!(
            parse_animation("Walk"),
            AnimationRef::Name("Walk".to_owned())
        );
    }

    #[test]
    fn command_line_options_override_the_configuration() {
        let arguments = args(&[
            "--output",
            "out.png",
            "--output-size",
            "512x384",
            "--scale",
            "2,3",
            "--eye",
            "1,2,3",
        ]);
        let mut config = Config::default();
        config.camera.view = Some([0.0; 16]);
        config.camera.projection = Some([0.0; 16]);
        arguments.apply(&mut config);
        assert_eq!(config.output.size, Some((512, 384)));
        assert_eq!(config.output.scale, (2.0, 3.0));
        assert_eq!(config.camera.eye, [1.0, 2.0, 3.0]);
        assert_eq!(config.camera.view, None);
        assert_eq!(config.camera.projection, None);
    }

    #[test]
    fn documents_replace_the_configuration_list() {
        let arguments = args(&[
            "--output",
            "out.png",
            "--document",
            "a.glb",
            "--time",
            "1.5",
        ]);
        let mut config = Config::default();
        config.documents.push(DocumentConfig {
            path: "ignored.glb".to_owned(),
            ..DocumentConfig::default()
        });
        arguments.apply(&mut config);
        assert_eq!(config.documents.len(), 1);
        assert_eq!(config.documents[0].path, "a.glb");
        assert_eq!(config.documents[0].time, 1.5);
    }
}
