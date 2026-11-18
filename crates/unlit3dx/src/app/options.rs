//! The switches a run reads, and the arguments an application adds to
//! the same command line.

use std::path::PathBuf;

use clap::Parser;

/// The switches a run owns.
///
/// The application's arguments, declared through [`App::Args`](super::App::Args), are
/// flattened into the same parser, so one command line and one `--help`
/// cover both sides.
#[derive(Clone, Debug, Parser)]
pub struct RunOptions {
    /// Serve the scene over the Model Context Protocol: in a windowed run
    /// the presenter draws into an offscreen target, and a headless run
    /// serves the interactive host instead of capturing.
    #[arg(long)]
    pub mcp: bool,
    /// Drive the worlds from the frame loop rather than threads of their
    /// own. A browser always takes this path, whatever the switch says.
    #[arg(long)]
    pub stepped: bool,
    /// Run without a window. Naming `--frames` or `--out` asks for one
    /// too.
    #[arg(long)]
    pub headless: bool,
    /// How many frames a headless run steps before it ends.
    #[arg(long)]
    pub frames: Option<u32>,
    /// The directory a headless run writes its capture under.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
}

impl RunOptions {
    /// The frame count a headless run steps when the command line names
    /// none: one frame, the same conservative default the headless
    /// configuration itself keeps.
    pub const DEFAULT_FRAMES: u32 = 1;

    /// The directory a headless run writes under when the command line
    /// names none.
    pub const DEFAULT_OUT: &str = "headless";

    /// Whether the command line asks for a headless run: the switch, or
    /// either option only a headless run consumes.
    pub fn headless_run(&self) -> bool {
        self.headless || self.frames.is_some() || self.out.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::RunOptions;
    use clap::Parser as _;

    #[test]
    fn the_switches_parse() {
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--mcp", "--stepped"]).expect("the switches parse");
        assert!(options.mcp);
        assert!(options.stepped);
        assert!(!options.headless_run());
    }

    #[test]
    fn a_capture_option_asks_for_the_headless_run() {
        let options = RunOptions::try_parse_from(["unlit3dx", "--frames", "12"]).expect("the value parses");
        assert!(options.headless_run());
        assert_eq!(options.frames, Some(12));
        let options = RunOptions::try_parse_from(["unlit3dx", "--out=/tmp/capture"]).expect("the value parses");
        assert!(options.headless_run());
        assert_eq!(
            options.out.as_deref(),
            Some(std::path::Path::new("/tmp/capture"))
        );
        let options = RunOptions::try_parse_from(["unlit3dx", "--headless"]).expect("the switch parses");
        assert!(options.headless_run());
    }

    #[test]
    fn bare_options_ask_for_a_window() {
        let options = RunOptions::try_parse_from(["unlit3dx"]).expect("the empty line parses");
        assert!(!options.headless_run());
        assert_eq!(options.frames, None);
        assert_eq!(options.out, None);
    }

    #[test]
    fn what_neither_side_declares_is_refused() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--nonsense"]).is_err());
        assert!(RunOptions::try_parse_from(["unlit3dx", "--frames"]).is_err());
    }
}
