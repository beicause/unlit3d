//! The example's command line.
//!
//! The options are few and the example has no argument-parsing dependency, so
//! the parser is hand-written rather than pulling one in. Everything is
//! `--name value` or a bare flag; `--name=value` is accepted too.

use std::path::PathBuf;

/// The size the example renders at when the window or `--size` says otherwise.
pub const DEFAULT_SIZE: (u32, u32) = (960, 720);

/// How many frames the headless path draws before reading one back.
///
/// The second frame is the first egui has its font metrics for, so a capture
/// needs at least two to show laid-out text.
pub const DEFAULT_FRAMES: u32 = 2;

/// The lowest SSIMULACRA2 score that counts as matching by default.
///
/// The same threshold the snapshot tests use, restated here so the command line
/// does not have to reach into the test harness for it.
pub const DEFAULT_MIN_SCORE: f64 = 85.0;

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
    /// The render target size, in pixels.
    pub size: (u32, u32),
    /// How many frames to draw before capturing.
    pub frames: u32,
    /// Write the captured frame to this path as a lossless WebP.
    pub output: Option<PathBuf>,
    /// Compare the captured frame against the snapshot at this path.
    pub snapshot: Option<PathBuf>,
    /// Store the snapshot instead of comparing against it.
    pub update: bool,
    /// Draw the cube without the UI, so the capture shows the 3D scene alone.
    pub no_ui: bool,
    /// The lowest SSIMULACRA2 score that counts as matching.
    pub min_score: f64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            headless: false,
            size: DEFAULT_SIZE,
            frames: DEFAULT_FRAMES,
            output: None,
            snapshot: None,
            update: false,
            no_ui: false,
            min_score: DEFAULT_MIN_SCORE,
        }
    }
}

/// The usage text `--help` prints.
pub fn usage() -> String {
    format!(
        "\
A windowed unlit cube with an egui overlay.

Usage: unlit3d_examples [OPTIONS]

Options:
      --headless          Render offscreen and exit without opening a window
      --size <WxH>        Render target size in pixels [default: {}x{}]
      --frames <N>        Frames to draw before capturing [default: {}]
      --output <PATH>     Write the captured frame to PATH as a WebP
      --snapshot <PATH>   Compare the captured frame against the snapshot at PATH
      --update            Store PATH instead of comparing against it
      --no-ui             Draw the cube without the UI overlay
      --min-score <S>     Lowest matching SSIMULACRA2 score [default: {}]
  -h, --help              Print this help

The headless options need the `snapshot` feature, which is what reads frames
back and scores them:
    cargo run -p unlit3d_examples --features snapshot -- --headless --snapshot out.webp
",
        DEFAULT_SIZE.0, DEFAULT_SIZE.1, DEFAULT_FRAMES, DEFAULT_MIN_SCORE,
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
            "--size" => parsed.size = parse_size(&value(&name, inline, &mut args)?)?,
            "--frames" => parsed.frames = parse_count(&value(&name, inline, &mut args)?)?,
            "--min-score" => parsed.min_score = parse_score(&value(&name, inline, &mut args)?)?,
            "--output" => parsed.output = Some(PathBuf::from(value(&name, inline, &mut args)?)),
            "--snapshot" => parsed.snapshot = Some(PathBuf::from(value(&name, inline, &mut args)?)),
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
    }
    if parsed.update && parsed.snapshot.is_none() {
        return Err("`--update` needs `--snapshot <PATH>`".to_owned());
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
    fn no_arguments_windows_at_the_default_size() {
        let args = run(&[]).expect("no arguments parse");
        assert!(!args.headless);
        assert_eq!(args.size, DEFAULT_SIZE);
        assert_eq!(args.frames, DEFAULT_FRAMES);
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
        ])
        .expect("the capture options parse");

        assert!(args.headless);
        assert_eq!(args.size, (320, 240));
        assert_eq!(args.frames, 5);
        assert_eq!(args.output.as_deref(), Some("frame.webp".as_ref()));
        assert_eq!(args.snapshot.as_deref(), Some("snap.webp".as_ref()));
        assert!(args.update);
        assert_eq!(args.min_score, 90.5);
    }

    #[test]
    fn an_equals_sign_carries_the_value() {
        let args = run(&["--headless", "--size=64x32", "--min-score=70"]).expect("`=` parses");
        assert_eq!(args.size, (64, 32));
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
    }

    #[test]
    fn update_without_a_snapshot_is_rejected() {
        assert_eq!(
            run(&["--headless", "--update"]),
            Err("`--update` needs `--snapshot <PATH>`".to_owned())
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
