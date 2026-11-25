//! The switches a run reads, and the arguments an application adds to
//! the same command line.

use std::path::PathBuf;

use clap::Parser;

use super::error::ConfigError;

/// The switches a run owns.
///
/// The application's arguments, declared through [`App::Args`](super::App::Args), are
/// flattened into the same parser, so one command line and one `--help`
/// cover both sides.
#[derive(Clone, Debug, Parser)]
pub struct RunOptions {
    /// Serve the scene over MCP on standard input and output: in a windowed
    /// run the presenter draws into an offscreen target, and a headless run
    /// serves the interactive host.
    #[arg(long)]
    pub mcp_stdio: bool,
    /// Serve the scene over MCP on a TCP port as well, so network clients
    /// reach it while the run keeps its window (or its headless loop).
    /// Without `--mcp-http-addr` the server listens on the loopback on a
    /// port the OS picks.
    #[arg(long)]
    pub mcp_http: bool,
    /// The address the MCP HTTP server listens on. Naming it asks for the
    /// HTTP server too; port 0 leaves the port to the OS.
    #[arg(long, value_name = "ADDR")]
    pub mcp_http_addr: Option<String>,
    /// Drive the worlds from the frame loop rather than threads of their
    /// own. A browser always takes this path, whatever the switch says.
    #[arg(long)]
    pub stepped: bool,
    /// Run without a window: a scene the run steps itself, or a host a
    /// client drives through MCP. Without the switch the run opens a
    /// window.
    #[arg(long)]
    pub headless: bool,
    /// Leave the frames to the client: the run advances only when the
    /// client's tools advance them, so a client that never steps never sees
    /// a frame advance. Without the switch the run paces itself, and MCP
    /// rides along as an observer and an injector.
    ///
    /// Pacing needs a transport to reach the client. Naming this switch
    /// without `--mcp-stdio` or `--mcp-http` leaves it inert: the run
    /// announces that and paces its own frames.
    #[arg(long)]
    pub interactive: bool,
    /// The most frames a run advances before it ends, counting from the
    /// first.
    ///
    /// It counts frames the scene advanced, which is what a capture names
    /// and what a client's steps reach: a turn that draws nothing — a
    /// suspended app, or one whose presentation path is not built yet —
    /// spends none of it.
    ///
    /// Without it a run has no length of its own: a windowed run ends when
    /// its window closes, and a headless run only when its client asks it
    /// to exit or the process is interrupted.
    #[arg(long, value_name = "FRAMES", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_frames: Option<u32>,
    /// Which frames to write out, by index, comma separated: `0,10,20`.
    ///
    /// The indices are of the run's own frames, counting from the first
    /// one it advances, and they are written as they arrive — a run cut
    /// short keeps the frames it reached. Naming them bounds a run that
    /// names no `--max-frames` to the frame after the last one.
    #[arg(
        long,
        value_name = "FRAMES",
        value_delimiter = ',',
        requires = "capture_out"
    )]
    pub capture_frames: Vec<u32>,
    /// The directory a run writes its capture under: one PNG per frame it
    /// named, and the mix a headless or interactive run played. A windowed
    /// run writes the frames alone, since the mix it plays is filled by the
    /// audio device on the device's clock and shares no time base with the
    /// frames a client may step. Without it the run writes nothing.
    #[arg(long, value_name = "DIR", requires = "capture_frames")]
    pub capture_out: Option<PathBuf>,
}

impl RunOptions {
    /// The address the MCP HTTP server listens on when the command line
    /// names none: the loopback, on a port the OS picks.
    pub const DEFAULT_MCP_HTTP_ADDR: &str = "127.0.0.1:0";

    /// The address the MCP HTTP server listens on, resolved to the default
    /// when the command line names none. An unparsable address stays an
    /// error until the run reads it, which happens before the window or
    /// the headless loop starts, and the message carries the
    /// `--mcp-http-addr` prefix so the failing switch stays findable.
    pub fn mcp_http_addr(&self) -> Result<std::net::SocketAddr, ConfigError> {
        self.mcp_http_addr
            .as_deref()
            .unwrap_or(Self::DEFAULT_MCP_HTTP_ADDR)
            .parse()
            .map_err(|source| ConfigError::HttpAddr { source })
    }

    /// The address the MCP HTTP server listens on, when the command line
    /// asks for one: [None] when neither switch names it, and the parse
    /// error when the named one does not parse.
    pub fn mcp_http_bind(&self) -> Result<Option<std::net::SocketAddr>, ConfigError> {
        if !self.mcp_http_run() {
            return Ok(None);
        }
        self.mcp_http_addr().map(Some)
    }

    /// Whether the command line asks for the MCP HTTP server: its switch,
    /// or an address named for it.
    pub fn mcp_http_run(&self) -> bool {
        self.mcp_http || self.mcp_http_addr.is_some()
    }

    /// The most frames a run advances, when the command line bounds it.
    ///
    /// `--max-frames` names it outright. Failing that, a capture names the
    /// frames it wants, and the run has to reach the last of them, so the
    /// frame after it is the bound. Unbounded is [None].
    pub fn frame_budget(&self) -> Option<u32> {
        self.max_frames.or_else(|| {
            self.frames_to_capture()
                .last()
                .map(|last| last.saturating_add(1))
        })
    }

    /// The frames the command line asks to write out, ascending and
    /// without repeats.
    ///
    /// A frame named twice is one picture, and the order the indices are
    /// named in is not the order they arrive.
    pub fn frames_to_capture(&self) -> Vec<u32> {
        let mut frames = self.capture_frames.clone();
        frames.sort_unstable();
        frames.dedup();
        frames
    }

    /// The capture the command line asks for, when it names one.
    pub fn capture(&self) -> Option<super::capture::Capture> {
        let into = self.capture_out.clone()?;
        Some(super::capture::Capture::new(
            self.frames_to_capture(),
            Some(into),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::RunOptions;
    use clap::Parser as _;

    #[test]
    fn the_switches_parse() {
        let options = RunOptions::try_parse_from(["unlit3dx", "--mcp-stdio", "--stepped"])
            .expect("the switches parse");
        assert!(options.mcp_stdio);
        assert!(options.stepped);
        assert!(!options.headless);
        assert!(!options.mcp_http_run());
    }

    #[test]
    fn the_mcp_http_switches_parse() {
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--mcp-http"]).expect("the switch parses");
        assert!(options.mcp_http_run());
        assert_eq!(
            options.mcp_http_addr().expect("the default parses"),
            "127.0.0.1:0".parse().expect("the loopback parses")
        );
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--mcp-http-addr", "192.168.1.10:7860"])
                .expect("the address parses");
        assert!(options.mcp_http_run());
        assert_eq!(
            options
                .mcp_http_addr()
                .expect("the named address parses")
                .to_string(),
            "192.168.1.10:7860"
        );
        let options = RunOptions::try_parse_from(["unlit3dx", "--mcp-http-addr", "not-an-address"])
            .expect("the line parses; the address is read later");
        assert!(options.mcp_http_addr().is_err());
    }

    #[test]
    fn the_headless_switches_parse() {
        let options = RunOptions::try_parse_from(["unlit3dx", "--headless", "--max-frames", "12"])
            .expect("the values parse");
        assert!(options.headless);
        assert_eq!(options.max_frames, Some(12));
        assert_eq!(options.frame_budget(), Some(12));
    }

    #[test]
    fn the_capture_switches_parse() {
        let options = RunOptions::try_parse_from([
            "unlit3dx",
            "--headless",
            "--capture-frames",
            "20,0,10",
            "--capture-out",
            "/tmp/capture",
        ])
        .expect("the values parse");
        assert_eq!(options.capture_frames, [20, 0, 10]);
        assert_eq!(options.frames_to_capture(), [0, 10, 20]);
        assert_eq!(
            options.capture_out.as_deref(),
            Some(std::path::Path::new("/tmp/capture"))
        );
    }

    #[test]
    fn a_capture_switch_needs_the_other_half() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--capture-frames", "12"]).is_err());
        assert!(RunOptions::try_parse_from(["unlit3dx", "--capture-out=/tmp/capture"]).is_err());
    }

    #[test]
    fn a_frame_count_of_zero_is_refused() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--max-frames", "0"]).is_err());
        assert!(RunOptions::try_parse_from(["unlit3dx", "--max-frames"]).is_err());
    }

    #[test]
    fn the_interactive_switch_parses() {
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--headless", "--interactive", "--mcp-stdio"])
                .expect("the switch parses");
        assert!(options.interactive);
        assert_eq!(options.max_frames, None);
        assert_eq!(options.frame_budget(), None);
    }

    #[test]
    fn the_frames_and_the_client_pacing_coexist() {
        let options = RunOptions::try_parse_from([
            "unlit3dx",
            "--headless",
            "--interactive",
            "--mcp-stdio",
            "--max-frames",
            "12",
        ])
        .expect("a client-paced run may still be bounded");
        assert_eq!(options.frame_budget(), Some(12));
    }

    #[test]
    fn a_capture_bounds_the_run_it_serves() {
        let options = RunOptions::try_parse_from([
            "unlit3dx",
            "--headless",
            "--capture-frames",
            "0,10,20",
            "--capture-out",
            "/tmp/capture",
        ])
        .expect("the values parse");
        assert_eq!(options.frame_budget(), Some(21));
    }

    #[test]
    fn a_named_frame_count_wins_over_the_capture() {
        let options = RunOptions::try_parse_from([
            "unlit3dx",
            "--headless",
            "--max-frames",
            "5",
            "--capture-frames",
            "0,10",
            "--capture-out",
            "/tmp/capture",
        ])
        .expect("the values parse");
        assert_eq!(options.frame_budget(), Some(5));
    }

    #[test]
    fn bare_options_ask_for_a_window() {
        let options = RunOptions::try_parse_from(["unlit3dx"]).expect("the empty line parses");
        assert!(!options.headless);
        assert_eq!(options.max_frames, None);
        assert_eq!(options.frame_budget(), None);
        assert!(options.capture().is_none());
    }

    #[test]
    fn what_neither_side_declares_is_refused() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--nonsense"]).is_err());
    }
}
