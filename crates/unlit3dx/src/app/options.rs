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
        let options = RunOptions::try_parse_from(["unlit3dx", "--mcp", "--stepped"])
            .expect("the switches parse");
        assert!(options.mcp);
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
    fn a_capture_option_asks_for_the_headless_run() {
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--frames", "12"]).expect("the value parses");
        assert!(options.headless_run());
        assert_eq!(options.frames, Some(12));
        let options = RunOptions::try_parse_from(["unlit3dx", "--out=/tmp/capture"])
            .expect("the value parses");
        assert!(options.headless_run());
        assert_eq!(
            options.out.as_deref(),
            Some(std::path::Path::new("/tmp/capture"))
        );
        let options =
            RunOptions::try_parse_from(["unlit3dx", "--headless"]).expect("the switch parses");
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
