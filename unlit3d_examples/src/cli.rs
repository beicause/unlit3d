//! The example's command line.
//!
//! The options are few and the example has no argument-parsing dependency, so
//! the parser is hand-written rather than pulling one in. Everything is
//! `--name value` or a bare flag; `--name=value` is accepted too.

use std::path::PathBuf;

use crate::scenes;

/// The lowest SSIMULACRA2 score that counts as matching by default.
///
/// The same threshold the snapshot tests use, restated here so the command line
/// does not have to reach into the test harness for it.
pub const DEFAULT_MIN_SCORE: f64 = 85.0;

/// The default snapshot directory: the asset submodule's `snapshots`.
///
/// Resolved through the crate's manifest rather than the working directory, so
/// `cargo run` finds it wherever it is invoked from.
pub fn default_snapshot_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wgpu_unlit_render_asset_files/snapshots"
    ))
}

/// What the command line asked the example to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Run with these options.
    Run(Args),
    /// `--help` was given: print the usage text.
    Help,
}

/// Every option the example accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    /// Render offscreen, read the frame back and exit, without opening a
    /// window.
    pub headless: bool,
    /// The scene to run, or `"all"` for every scene in a headless run.
    pub scene: String,
    /// Print the scene list and exit.
    pub list_scenes: bool,
    /// The render target size, in pixels; the scene's own size when `None`.
    pub size: Option<(u32, u32)>,
    /// How many frames to draw before capturing; the scene's own count when
    /// `None`.
    pub frames: Option<u32>,
    /// Write the captured frame to this path as a lossless WebP.
    pub output: Option<PathBuf>,
    /// Compare the captured frame against the snapshot at this path, instead
    /// of the scene's own snapshots.
    pub snapshot: Option<PathBuf>,
    /// Store snapshots instead of comparing against them.
    pub update: bool,
    /// Draw the scene without its UI, so the capture shows the 3D scene alone.
    pub no_ui: bool,
    /// The lowest SSIMULACRA2 score that counts as matching.
    pub min_score: f64,
    /// Where the scene's own snapshots resolve, by name.
    pub snapshot_dir: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            headless: false,
            scene: scenes::default().id.to_owned(),
            list_scenes: false,
            size: None,
            frames: None,
            output: None,
            snapshot: None,
            update: false,
            no_ui: false,
            min_score: DEFAULT_MIN_SCORE,
            snapshot_dir: default_snapshot_dir(),
        }
    }
}

/// The usage text `--help` prints.
pub fn usage() -> String {
    format!(
        "\
A windowed example with selectable scenes and an egui overlay — or, headlessly,
an offscreen capture that verifies each scene against its stored snapshots.

Usage: unlit3d_examples [OPTIONS]

Options:
      --headless          Render offscreen and exit without opening a window
      --scene <ID>        The scene to run, or `all` for every scene [default: {}]
      --list-scenes       Print the scene table and exit
      --size <WxH>        Render target size in pixels [default: the scene's own]
      --frames <N>        Frames to draw before capturing [default: the scene's own]
      --output <PATH>     Write the captured frame to PATH as a WebP
      --snapshot <PATH>   Compare the captured frame against the snapshot at PATH
      --update            Store the snapshots being compared instead
      --no-ui             Draw the scene without its UI overlay
      --min-score <S>     Lowest matching SSIMULACRA2 score [default: {}]
      --snapshot-dir <D>  Where the scene's own snapshots resolve [default: {}]
  -h, --help              Print this help

{}

The headless options need the `snapshot` feature, which is what reads frames
back and scores them:
    cargo run -p unlit3d_examples --features snapshot -- --headless --scene ecs_skinned
",
        scenes::default().id,
        DEFAULT_MIN_SCORE,
        default_snapshot_dir().display(),
        scenes::list_text(),
    )
}

/// Parse the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter().map(Into::into);

    while let Some(arg) = args.next() {
        // `--name=value` carries its value on the same argument.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };

        match name.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--headless" => parsed.headless = true,
            "--update" => parsed.update = true,
            "--no-ui" => parsed.no_ui = true,
            "--list-scenes" => parsed.list_scenes = true,
            "--scene" => parsed.scene = value(&name, inline, &mut args)?,
            "--size" => parsed.size = Some(parse_size(&value(&name, inline, &mut args)?)?),
            "--frames" => parsed.frames = Some(parse_count(&value(&name, inline, &mut args)?)?),
            "--min-score" => parsed.min_score = parse_score(&value(&name, inline, &mut args)?)?,
            "--output" => parsed.output = Some(PathBuf::from(value(&name, inline, &mut args)?)),
            "--snapshot" => parsed.snapshot = Some(PathBuf::from(value(&name, inline, &mut args)?)),
            "--snapshot-dir" => {
                parsed.snapshot_dir = PathBuf::from(value(&name, inline, &mut args)?);
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }

    // The capture options have no meaning in the windowed loop, so asking for
    // one there is a mistake worth reporting rather than silently ignoring.
    if !parsed.headless {
        let capture_option = if parsed.output.is_some() {
            Some("--output")
        } else if parsed.snapshot.is_some() {
            Some("--snapshot")
        } else if parsed.update {
            Some("--update")
        } else {
            None
        };
        if let Some(option) = capture_option {
            return Err(format!("`{option}` needs `--headless`"));
        }
        if parsed.scene == "all" {
            return Err("`--scene all` needs `--headless`".to_owned());
        }
    }

    // A scene the example does not know is a typo worth reporting, whichever
    // mode it was asked in. `all` is not a scene of its own.
    if parsed.scene != "all" && scenes::by_id(&parsed.scene).is_none() {
        return Err(format!(
            "unknown scene `{}`\n{}",
            parsed.scene,
            scenes::list_text()
        ));
    }

    // A raw capture against one path and a run over every scene cannot both
    // say what to compare.
    if parsed.scene == "all" && (parsed.snapshot.is_some() || parsed.output.is_some()) {
        return Err("`--scene all` cannot be combined with `--output` or `--snapshot`".to_owned());
    }

    Ok(Parsed::Run(parsed))
}

/// The value of the option `name`, from `--name=value` or the next argument.
fn value(
    name: &str,
    inline: Option<String>,
    args: &mut impl Iterator<Item = String>,
) -> Result<String, String> {
    match inline {
        Some(value) => Ok(value),
        None => args.next().ok_or_else(|| format!("`{name}` needs a value")),
    }
}

/// Parse a `WxH` size, both dimensions at least one pixel.
fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let invalid = || format!("`{text}` is not a `WxH` size");
    let (width, height) = text.split_once(['x', 'X']).ok_or_else(invalid)?;
    let width: u32 = width.trim().parse().map_err(|_| invalid())?;
    let height: u32 = height.trim().parse().map_err(|_| invalid())?;
    if width == 0 || height == 0 {
        return Err(format!("`{text}` has a zero dimension"));
    }
    Ok((width, height))
}

/// Parse a frame count of at least one.
fn parse_count(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(count) if count > 0 => Ok(count),
        _ => Err(format!("`{text}` is not a frame count of at least one")),
    }
}

/// Parse a finite SSIMULACRA2 score.
fn parse_score(text: &str) -> Result<f64, String> {
    match text.trim().parse::<f64>() {
        Ok(score) if score.is_finite() => Ok(score),
        _ => Err(format!("`{text}` is not a finite score")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a shell-like argument list.
    fn parse_args(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().copied())
    }

    /// The options of a successful parse that was not `--help`.
    fn run(args: &[&str]) -> Result<Args, String> {
        match parse_args(args)? {
            Parsed::Run(parsed) => Ok(parsed),
            Parsed::Help => panic!("expected options, got --help"),
        }
    }

    #[test]
    fn no_arguments_run_the_default_scene_windowed() {
        let args = run(&[]).expect("no arguments parse");
        assert!(!args.headless);
        assert_eq!(args.scene, scenes::default().id);
        assert!(!args.list_scenes);
        assert_eq!(args.size, None);
        assert_eq!(args.frames, None);
        assert_eq!(args.output, None);
        assert_eq!(args.snapshot, None);
        assert!(!args.update);
        assert!(!args.no_ui);
        assert_eq!(args.min_score, DEFAULT_MIN_SCORE);
    }

    #[test]
    fn the_capture_options_are_read() {
        let args = run(&[
            "--headless",
            "--scene",
            "ecs_skinned",
            "--size",
            "320x240",
            "--frames",
            "5",
            "--output",
            "frame.webp",
            "--snapshot",
            "snap.webp",
            "--update",
            "--min-score",
            "90.5",
            "--snapshot-dir",
            "snaps",
        ])
        .expect("the capture options parse");

        assert!(args.headless);
        assert_eq!(args.scene, "ecs_skinned");
        assert_eq!(args.size, Some((320, 240)));
        assert_eq!(args.frames, Some(5));
        assert_eq!(args.output.as_deref(), Some("frame.webp".as_ref()));
        assert_eq!(args.snapshot.as_deref(), Some("snap.webp".as_ref()));
        assert!(args.update);
        assert_eq!(args.min_score, 90.5);
        assert_eq!(args.snapshot_dir, PathBuf::from("snaps"));
    }

    #[test]
    fn an_equals_sign_carries_the_value() {
        let args = run(&["--headless", "--size=64x32", "--min-score=70"]).expect("`=` parses");
        assert_eq!(args.size, Some((64, 32)));
        assert_eq!(args.min_score, 70.0);
    }

    #[test]
    fn help_short_circuits_the_rest_of_the_line() {
        assert_eq!(parse_args(&["--headless", "--help"]), Ok(Parsed::Help));
        assert_eq!(parse_args(&["-h"]), Ok(Parsed::Help));
    }

    #[test]
    fn a_capture_option_without_headless_is_rejected() {
        assert_eq!(
            run(&["--snapshot", "snap.webp"]),
            Err("`--snapshot` needs `--headless`".to_owned())
        );
        assert_eq!(
            run(&["--output", "frame.webp"]),
            Err("`--output` needs `--headless`".to_owned())
        );
        assert_eq!(
            run(&["--update"]),
            Err("`--update` needs `--headless`".to_owned())
        );
    }

    #[test]
    fn an_unknown_scene_is_rejected() {
        assert!(run(&["--headless", "--scene", "nonsense"]).is_err());
        assert!(run(&["--scene", "nonsense"]).is_err());
    }

    #[test]
    fn running_every_scene_is_headless_only() {
        let args = run(&["--headless", "--scene", "all"]).expect("headless `all` parses");
        assert_eq!(args.scene, "all");
        assert_eq!(
            run(&["--scene", "all"]),
            Err("`--scene all` needs `--headless`".to_owned())
        );
        assert!(
            run(&["--headless", "--scene", "all", "--snapshot", "x.webp"]).is_err(),
            "a raw capture cannot be combined with every scene"
        );
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert!(run(&["--headless", "--size", "wide"]).is_err());
        assert!(run(&["--headless", "--size", "0x240"]).is_err());
        assert!(run(&["--headless", "--frames", "0"]).is_err());
        assert!(run(&["--headless", "--min-score", "nan"]).is_err());
        assert!(run(&["--headless", "--size"]).is_err());
        assert!(run(&["--nonsense"]).is_err());
    }
}
