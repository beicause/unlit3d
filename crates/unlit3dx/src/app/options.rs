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
    /// Serve the scene over MCP on standard input and output: in a windowed
    /// run the presenter draws into an offscreen target, and a headless run
    /// serves the interactive host instead of capturing.
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
    /// Run without a window: a capture the run steps itself, or a host a
    /// client drives through MCP. Without the switch the run opens a
    /// window.
    #[arg(long)]
    pub headless: bool,
    /// How many frames a headless run steps before it ends. A served run
    /// ignores the count: its client paces the frames.
    #[arg(long, value_name = "FRAMES", requires = "headless")]
    pub headless_frames: Option<u32>,
    /// The directory a headless run writes its capture under: the frames
    /// and the mix of the run. Without it the run writes nothing.
    #[arg(long, value_name = "DIR", requires = "headless")]
    pub headless_out: Option<PathBuf>,
}

impl RunOptions {
    /// The frame count a headless run steps when the command line names
    /// none: one frame, the same conservative default the headless
    /// configuration itself keeps.
    pub const DEFAULT_HEADLESS_FRAMES: u32 = 1;

    /// The address the MCP HTTP server listens on when the command line
    /// names none: the loopback, on a port the OS picks.
    pub const DEFAULT_MCP_HTTP_ADDR: &str = "127.0.0.1:0";

    /// The address the MCP HTTP server listens on, resolved to the default
    /// when the command line names none. An unparsable address stays an
    /// error until the run reads it, which happens before the window or
    /// the headless loop starts, and the message carries the
    /// `--mcp-http-addr` prefix so the failing switch stays findable.
    pub fn mcp_http_addr(&self) -> Result<std::net::SocketAddr, String> {
        self.mcp_http_addr
            .as_deref()
            .unwrap_or(Self::DEFAULT_MCP_HTTP_ADDR)
            .parse()
            .map_err(|error| format!("--mcp-http-addr: {error}"))
    }

    /// The address the MCP HTTP server listens on, when the command line
    /// asks for one: [None] when neither switch names it, and the parse
    /// error when the named one does not parse.
    pub fn mcp_http_bind(&self) -> Result<Option<std::net::SocketAddr>, String> {
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

    /// Whether the command line asks for a headless run.
    pub fn headless_run(&self) -> bool {
        self.headless
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
        assert!(!options.headless_run());
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
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--headless", "--headless-frames", "12"])
                .expect("the values parse");
        assert!(options.headless_run());
        assert_eq!(options.headless_frames, Some(12));
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--headless", "--headless-out=/tmp/capture"])
                .expect("the values parse");
        assert!(options.headless_run());
        assert_eq!(
            options.headless_out.as_deref(),
            Some(std::path::Path::new("/tmp/capture"))
        );
    }

    #[test]
    fn a_capture_switch_without_headless_is_refused() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--headless-frames", "12"]).is_err());
        assert!(RunOptions::try_parse_from(["unlit3dx", "--headless-out=/tmp/capture"]).is_err());
    }

    #[test]
    fn bare_options_ask_for_a_window() {
        let options = RunOptions::try_parse_from(["unlit3dx"]).expect("the empty line parses");
        assert!(!options.headless_run());
        assert_eq!(options.headless_frames, None);
        assert_eq!(options.headless_out, None);
    }

    #[test]
    fn what_neither_side_declares_is_refused() {
        assert!(RunOptions::try_parse_from(["unlit3dx", "--nonsense"]).is_err());
        assert!(RunOptions::try_parse_from(["unlit3dx", "--headless-frames"]).is_err());
    }
}
