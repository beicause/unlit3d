//! The example's command line.
//!
//! The flags are declared with [`argh`], which derives the parser and the
//! `--help` text from the [`Args`] struct. The few values with a shape of their
//! own — a `WxH` size, a frame count, a score — are parsed by the functions
//! [`Args`] names through `from_str_fn`, so the rules live next to the options
//! they constrain.
//!
//! [`Args::validate`] holds the checks that span more than one option: a
//! capture option without `--headless`, a scene the example does not know, and
//! the combinations that cannot both say what to compare.

use std::path::PathBuf;

use argh::FromArgs;

use crate::scenes;

/// The program name argh puts in its usage and error messages.
const PROGRAM: &str = "unlit3d_examples";

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

/// The scene `--scene` starts from.
fn default_scene() -> String {
    scenes::default().id.to_owned()
}

/// Every option the example accepts.
#[derive(FromArgs, Debug, Clone, PartialEq)]
#[argh(
    help_triggers("-h", "--help"),
    description = "a windowed example with selectable scenes and an egui overlay, \
                   or a headless capture that verifies each scene against its snapshots"
)]
pub struct Args {
    /// render offscreen, read the frame back and exit, without opening a window
    #[argh(switch)]
    pub headless: bool,

    /// the scene to run, or `all` for every scene in a headless run
    #[argh(option, default = "default_scene()")]
    pub scene: String,

    /// print the scene table and exit
    #[argh(switch)]
    pub list_scenes: bool,

    /// render target size in pixels, as `WxH`; the scene's own when omitted
    #[argh(option, from_str_fn(parse_size))]
    pub size: Option<(u32, u32)>,

    /// frames to draw before capturing; the scene's own when omitted
    #[argh(option, from_str_fn(parse_count))]
    pub frames: Option<u32>,

    /// write the captured frame to this path as a lossless WebP
    #[argh(option)]
    pub output: Option<PathBuf>,

    /// compare the captured frame against the snapshot at this path, instead of
    /// the scene's own snapshots
    #[argh(option)]
    pub snapshot: Option<PathBuf>,

    /// store the snapshots being compared instead of comparing them
    #[argh(switch)]
    pub update: bool,

    /// draw the scene without its UI overlay
    #[argh(switch)]
    pub no_ui: bool,

    /// lowest SSIMULACRA2 score that counts as matching
    #[argh(option, from_str_fn(parse_score), default = "DEFAULT_MIN_SCORE")]
    pub min_score: f64,

    /// where the scene's own snapshots resolve, by name
    #[argh(option, default = "default_snapshot_dir()")]
    pub snapshot_dir: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            headless: false,
            scene: default_scene(),
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

/// What the command line asked the example to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Run with these options.
    Run(Args),
    /// `--help` was given: print `String`, which argh generated from [`Args`].
    Help(String),
}

/// Parse the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();

    match Args::from_args(&[PROGRAM], &refs) {
        Ok(parsed) => {
            parsed.validate()?;
            Ok(Parsed::Run(parsed))
        }
        // argh reports `--help` as an early exit whose status is `Ok`, and a
        // failed parse as one whose status is `Err`; only the message is
        // useful either way.
        Err(exit) if exit.status.is_ok() => Ok(Parsed::Help(usage())),
        Err(exit) => Err(exit.output),
    }
}

/// The `--help` text: argh's option list followed by the scene table.
///
/// argh derives the option text from [`Args`] but knows nothing about the
/// scenes, so the table is appended here rather than restated by hand.
pub fn usage() -> String {
    let options = Args::from_args(&[PROGRAM], &["--help"])
        .expect_err("`--help` exits early")
        .output;
    format!("{options}\n{}", scenes::list_text())
}

impl Args {
    /// The checks that span more than one option.
    ///
    /// argh has already parsed each option on its own; this is the part only
    /// the combination can decide.
    fn validate(&self) -> Result<(), String> {
        // The capture options have no meaning in the windowed loop, so asking
        // for one there is a mistake worth reporting rather than silently
        // ignoring.
        if !self.headless {
            let capture_option = if self.output.is_some() {
                Some("--output")
            } else if self.snapshot.is_some() {
                Some("--snapshot")
            } else if self.update {
                Some("--update")
            } else {
                None
            };
            if let Some(option) = capture_option {
                return Err(format!("`{option}` needs `--headless`"));
            }
            if self.scene == "all" {
                return Err("`--scene all` needs `--headless`".to_owned());
            }
        }

        // A scene the example does not know is a typo worth reporting,
        // whichever mode it was asked in. `all` is not a scene of its own.
        if self.scene != "all" && scenes::by_id(&self.scene).is_none() {
            return Err(format!(
                "unknown scene `{}`\n{}",
                self.scene,
                scenes::list_text()
            ));
        }

        // A raw capture against one path and a run over every scene cannot both
        // say what to compare.
        if self.scene == "all" && (self.snapshot.is_some() || self.output.is_some()) {
            return Err(
                "`--scene all` cannot be combined with `--output` or `--snapshot`".to_owned(),
            );
        }

        Ok(())
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

    /// The options of a successful parse that was not `--help`.
    fn run(args: &[&str]) -> Result<Args, String> {
        match parse(args.iter().copied())? {
            Parsed::Run(parsed) => Ok(parsed),
            Parsed::Help(_) => panic!("expected options, got --help"),
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
        assert_eq!(args.snapshot_dir, default_snapshot_dir());
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
    fn help_is_reported_as_its_own_outcome() {
        let help = match parse(["--help"]).expect("`--help` parses") {
            Parsed::Help(text) => text,
            Parsed::Run(_) => panic!("`--help` must not run"),
        };
        assert!(
            help.contains("--headless"),
            "help names the options: {help}"
        );
        assert!(parse(["-h"]).is_ok(), "`-h` is a help trigger too");
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
