//! The example's command line.
//!
//! The flags are declared with [`argh`], which derives the parser and the
//! `--help` text from the [`Args`] struct. A value with a shape of its own — a
//! `WxH` size — is parsed by the function [`Args`] names through `from_str_fn`,
//! so the rules live next to the option they constrain.
//!
//! [`Args::validate`] holds the checks that span more than one option: here,
//! a scene the example does not know.
//!
//! The scenes' snapshots are not compared through this command line. That is a
//! test's job, and it lives in `tests/gpu_scenes.rs`, where the same bodies run
//! natively and in a browser.

use argh::FromArgs;

use crate::scenes;

/// The program name argh puts in its usage and error messages.
const PROGRAM: &str = "unlit3d_examples";

/// The scene `--scene` starts from.
fn default_scene() -> String {
    scenes::default().id.to_owned()
}

/// Every option the example accepts.
#[derive(FromArgs, Debug, Clone, PartialEq)]
#[argh(
    help_triggers("-h", "--help"),
    description = "a windowed example with selectable scenes and an egui overlay"
)]
pub struct Args {
    /// the scene to start from
    #[argh(option, default = "default_scene()")]
    pub scene: String,

    /// print the scene table and exit
    #[argh(switch)]
    pub list_scenes: bool,

    /// initial window size in pixels, as `WxH`; the scene's own when omitted
    #[argh(option, from_str_fn(parse_size))]
    pub size: Option<(u32, u32)>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            scene: default_scene(),
            list_scenes: false,
            size: None,
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
        // A scene the example does not know is a typo worth reporting.
        if scenes::by_id(&self.scene).is_none() {
            return Err(format!(
                "unknown scene `{}`\n{}",
                self.scene,
                scenes::list_text()
            ));
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
    fn no_arguments_start_the_default_scene_at_its_own_size() {
        let args = run(&[]).expect("no arguments parse");
        assert_eq!(args.scene, scenes::default().id);
        assert!(!args.list_scenes);
        assert_eq!(args.size, None);
    }

    #[test]
    fn the_window_options_are_read() {
        let args = run(&[
            "--scene",
            "ecs_skinned",
            "--size",
            "320x240",
            "--list-scenes",
        ])
        .expect("the options parse");
        assert_eq!(args.scene, "ecs_skinned");
        assert_eq!(args.size, Some((320, 240)));
        assert!(args.list_scenes);
    }

    #[test]
    fn help_is_reported_as_its_own_outcome() {
        let help = match parse(["--help"]).expect("`--help` parses") {
            Parsed::Help(text) => text,
            Parsed::Run(_) => panic!("`--help` must not run"),
        };
        assert!(help.contains("--scene"), "help names the options: {help}");
        assert!(parse(["-h"]).is_ok(), "`-h` is a help trigger too");
    }

    #[test]
    fn an_unknown_scene_is_rejected() {
        assert!(run(&["--scene", "nonsense"]).is_err());
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert!(run(&["--size", "wide"]).is_err());
        assert!(run(&["--size", "0x240"]).is_err());
        assert!(run(&["--size"]).is_err());
        assert!(run(&["--nonsense"]).is_err());
    }
}
