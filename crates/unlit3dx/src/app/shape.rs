//! The shape of a run, read once from the command line.
//!
//! Who steps the scene's worlds, who advances the frames, what a frame draws
//! into and which transports reach a client are one combination, not five
//! switches each entry point reads for itself. The combination is frozen here
//! at parse time, so the windowed, headless, interactive and served entries
//! consume the same answer rather than deriving their own and drifting apart
//! as the switches gain conditions.
//!
//! A build that cannot serve a transport cannot be paced by one either, so the
//! refusal happens here: naming a transport this build does not serve is an
//! error rather than a switch that quietly changes nothing.

use std::net::SocketAddr;

use super::RunOptions;
use super::capture::Capture;
use super::error::RunError;
use super::pace::Pacing;

/// Who steps the scene's worlds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldOwnership {
    /// The run steps them, one turn per frame, from the thread that owns the
    /// world.
    Hosted,
    /// Each world runs on a thread of its own and steps itself.
    Threaded,
}

/// Who advances the frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrivingMode {
    /// The run's own loop advances them, as fast as it can or at the display's
    /// rate.
    Loop,
    /// A client advances them through the MCP tools, and the run only answers
    /// commands and notices when it should end.
    Client,
}

/// The transports a client reaches the run through.
///
/// Any combination is a run: the process's pipes, a TCP port, or both at once.
/// stdio and HTTP are two booleans rather than one choice because serving both
/// is a run someone actually wants — a local client on the pipes while a remote
/// one connects over the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportConfig {
    /// Serve the process's standard streams.
    pub stdio: bool,
    /// Serve streamable HTTP on this address. Port 0 leaves the port to the OS.
    pub http: Option<SocketAddr>,
}

impl TransportConfig {
    /// Whether any transport reaches a client.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.stdio || self.http.is_some()
    }
}

/// How a run is put together, frozen after the command line is parsed.
#[derive(Clone, Debug)]
pub struct RunShape {
    /// Who steps the scene's worlds.
    pub ownership: WorldOwnership,
    /// Who advances the frames.
    pub driving: DrivingMode,
    /// Whether frames are drawn into an offscreen target rather than onto a
    /// surface.
    ///
    /// A run without a window always draws offscreen. A windowed run does so
    /// when something has to read the frame back: an MCP client, whichever
    /// transport serves it, or a capture.
    pub offscreen: bool,
    /// The transports a client reaches the run through.
    pub transports: TransportConfig,
    /// The most frames the run advances before it ends, when it is bounded.
    pub frames: Option<u32>,
    /// The frames the run writes out, and where.
    pub capture: Option<Capture>,
}

impl RunShape {
    /// Read the shape a command line asks for.
    ///
    /// # Errors
    ///
    /// Fails when the line names a transport this build does not serve: the
    /// switch would otherwise change nothing, and a run that quietly ignores
    /// what it was told is worse than one that refuses.
    pub fn parse(options: &RunOptions) -> Result<Self, RunError> {
        let transports = TransportConfig {
            stdio: options.mcp_stdio,
            http: options.mcp_http_bind()?,
        };
        // Asking for a transport this build cannot serve changes nothing: the
        // process starts, reads its switches and serves no one. The refusal
        // names the feature that would serve it instead.
        if transports.stdio && !cfg!(feature = "mcp") {
            return Err(RunError::NoMcpServer);
        }
        if transports.http.is_some() && !cfg!(feature = "mcp-http") {
            return Err(RunError::NoHttpTransport);
        }
        let capture = options.capture();
        // `--interactive` alone does not hand the frames to a client: with no
        // transport nothing reaches one, so the switch is read as naming the
        // run's own pacing instead. The same holds on a build that cannot
        // serve a transport at all.
        let reachable = transports.any()
            && cfg!(all(
                feature = "mcp",
                not(any(target_arch = "wasm32", target_os = "android"))
            ));
        let driving = if options.interactive && reachable {
            DrivingMode::Client
        } else {
            DrivingMode::Loop
        };
        // A client-paced run can never step behind the client's back, so it
        // always takes the archived worlds. A browser has no threads to give.
        let hosted = options.headless
            || options.stepped
            || driving == DrivingMode::Client
            || cfg!(any(target_arch = "wasm32", target_os = "android"));
        Ok(Self {
            ownership: if hosted {
                WorldOwnership::Hosted
            } else {
                WorldOwnership::Threaded
            },
            driving,
            offscreen: options.headless || transports.any() || capture.is_some(),
            transports,
            frames: options.frame_budget(),
            capture,
        })
    }

    /// The pacing this shape gives the run's frames.
    ///
    /// [`Pacing`] is what the worlds carry, so the shape projects onto it in
    /// one place: an entry that read the switches again could disagree with
    /// the one that parsed them.
    #[must_use]
    pub fn pacing(&self) -> Pacing {
        Pacing {
            client: self.driving == DrivingMode::Client,
            frames: self.frames,
        }
    }
}

impl Default for RunShape {
    /// The shape of a plain window: its worlds on their own threads, its loop
    /// advancing the frames, its frames drawn onto a surface.
    ///
    /// A browser has no threads to give the worlds, so it takes the hosted
    /// shape whatever a caller asks for.
    fn default() -> Self {
        Self {
            ownership: if cfg!(any(target_arch = "wasm32", target_os = "android")) {
                WorldOwnership::Hosted
            } else {
                WorldOwnership::Threaded
            },
            driving: DrivingMode::Loop,
            offscreen: false,
            transports: TransportConfig::default(),
            frames: None,
            capture: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    #[cfg(feature = "mcp-http")]
    use super::TransportConfig;
    use super::{DrivingMode, RunShape, WorldOwnership};
    use crate::app::RunOptions;

    fn parse(args: &[&str]) -> RunShape {
        let options = RunOptions::try_parse_from(args).expect("the switches parse");
        RunShape::parse(&options).expect("the shape reads")
    }

    #[test]
    fn bare_switches_ask_for_a_window_paced_by_its_own_loop() {
        let shape = parse(&["unlit3dx"]);
        assert_eq!(shape.ownership, WorldOwnership::Threaded);
        assert_eq!(shape.driving, DrivingMode::Loop);
        assert!(!shape.offscreen);
        assert!(!shape.transports.any());
        assert_eq!(shape.frames, None);
        assert!(shape.capture.is_none());
        assert!(!shape.pacing().client);
    }

    #[test]
    fn a_stepped_run_owns_its_worlds_and_still_paces_itself() {
        let shape = parse(&["unlit3dx", "--stepped"]);
        assert_eq!(shape.ownership, WorldOwnership::Hosted);
        assert_eq!(shape.driving, DrivingMode::Loop);
    }

    /// A client-paced run is a transport-paced one, so the switch needs a
    /// build that serves a transport.
    #[cfg(feature = "mcp")]
    #[test]
    fn a_client_paced_run_owns_its_worlds() {
        let shape = parse(&["unlit3dx", "--interactive", "--mcp-stdio"]);
        assert_eq!(shape.ownership, WorldOwnership::Hosted);
        assert_eq!(shape.driving, DrivingMode::Client);
        assert!(shape.pacing().client);
        assert!(shape.offscreen, "the client reads the frame back");
    }

    #[test]
    fn the_client_pacing_needs_a_transport() {
        let shape = parse(&["unlit3dx", "--interactive"]);
        assert_eq!(
            shape.driving,
            DrivingMode::Loop,
            "a switch nothing reaches a client through names the run's own pacing"
        );
    }

    /// Both transports are switched on at once, so the run serves stdio and
    /// HTTP together rather than trading one for the other.
    #[cfg(feature = "mcp-http")]
    #[test]
    fn both_transports_serve_at_once() {
        let shape = parse(&["unlit3dx", "--mcp-stdio", "--mcp-http"]);
        assert_eq!(
            shape.transports,
            TransportConfig {
                stdio: true,
                http: Some("127.0.0.1:0".parse().expect("the loopback parses")),
            }
        );
    }

    #[test]
    fn a_headless_run_draws_offscreen_whatever_else_it_names() {
        let shape = parse(&["unlit3dx", "--headless"]);
        assert_eq!(shape.ownership, WorldOwnership::Hosted);
        assert!(shape.offscreen);
        assert_eq!(shape.driving, DrivingMode::Loop);
    }

    #[test]
    fn a_capture_asks_a_windowed_run_to_draw_offscreen() {
        let shape = parse(&[
            "unlit3dx",
            "--capture-frames",
            "0",
            "--capture-out",
            "/tmp/capture",
        ]);
        assert!(shape.offscreen, "the swap chain image cannot be read back");
        assert!(shape.capture.is_some());
        assert_eq!(shape.frames, Some(1), "a capture bounds the run it serves");
    }

    #[test]
    fn the_frame_budget_reaches_the_pacing() {
        let shape = parse(&["unlit3dx", "--max-frames", "5"]);
        assert_eq!(shape.frames, Some(5));
        assert_eq!(shape.pacing().frames, Some(5));
    }
}
