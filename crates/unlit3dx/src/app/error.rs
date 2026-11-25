//! Why a run could not go on.
//!
//! A run has four entry points and they can all end in the same few ways: the
//! command line cannot be honoured, a scene world ended in failure, the GPU or
//! the render sub-world fell short, or a capture could not be written. Each way
//! is a variant here rather than a message, so a caller branches on what
//! happened and reads the underlying cause through [`std::error::Error::source`]
//! instead of matching on text.
//!
//! Only a cause an application invents for itself — the one its own
//! [`super::App::configure`] refuses its arguments with — is carried as text:
//! nothing here can enumerate reasons the application alone knows.

use std::net::AddrParseError;
use std::path::PathBuf;

use unlit_ecs::EntityAccessError;
use unlit_ecs::mcp::{ToolError, ToolErrorKind};

use super::capture::{CaptureError, CaptureWriteError};
use super::offscreen::OffscreenError;
use crate::world::WorldError;
use crate::world::{StepError, StepOutcome, WorldFailure};

/// Why the command line the run was given cannot be honoured.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The `--mcp-http-addr` switch does not parse.
    #[error("--mcp-http-addr: {source}")]
    HttpAddr {
        /// What the address parser reported.
        #[source]
        source: AddrParseError,
    },
    /// The application refused the arguments it was handed.
    #[error(transparent)]
    Application(#[from] AppConfigError),
}

/// Why an application could not apply the arguments it was given.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppConfigError {
    /// An argument named a file that could not be read.
    #[error("{} could not be read: {source}", path.display())]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// What the filesystem reported.
        #[source]
        source: std::io::Error,
    },
    /// The application refused its arguments for a reason of its own.
    #[error("{0}")]
    Rejected(String),
}

/// Why a run could not start, or stopped with a failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The command line cannot be honoured.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A run was asked to mix at a rate of zero.
    #[error("the audio sample rate must be positive")]
    AudioRate,
    /// The render sub-world is not reachable from the host.
    #[error("the render world is unavailable: {0}")]
    RenderWorld(#[source] EntityAccessError),
    /// The render world no longer holds a resource the scene setup named.
    #[error("the render world lost a resource the scene named: {0}")]
    Resolve(#[source] unlit_ecs::RemoteEntityError),
    /// The presentation path could not be built.
    #[error("failed to build the presentation path: {0}")]
    Setup(#[source] crate::present::PresenterError),
    /// The renderer could not be started.
    #[error(transparent)]
    Renderer(#[from] crate::gpu::GpuStartError),
    /// The offscreen target could not be built, drawn or read.
    #[error(transparent)]
    Offscreen(#[from] OffscreenError),
    /// A capture could not be read back or written.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// A world of the scene ended in failure, and this is what ended it.
    #[error("the scene failed: {0}")]
    Scene(#[source] WorldError),
    /// The render sub-world stopped, so there is no frame to draw.
    #[error("the render world cannot draw: {0}")]
    RenderWorldStop(StepOutcome),
    /// A world the run drives could not be reached or stepped.
    #[error("the world could not be driven: {0}")]
    Step(#[source] StepError),
    /// Worlds the run steps failed while their jobs were drained.
    #[error("{} world(s) failed: {}", .0.len(), crate::world::join_failures(.0))]
    Jobs(Vec<WorldFailure>),
    /// The MCP stdio transport could not be started.
    #[error("the MCP stdio server failed to start: {0}")]
    Stdio(#[source] std::io::Error),
    /// The MCP HTTP transport could not be bound.
    #[error("the MCP HTTP server failed to start: {0}")]
    Http(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The transport was asked for, but this build does not serve it.
    #[error("the HTTP transport needs the mcp-http feature")]
    NoHttpTransport,
    /// A client-paced run was given no transport to be driven through.
    #[error(
        "a client-paced host has no driver without a transport; name one with Transports, or let the host pace its own frames"
    )]
    NoDriver,
    /// The host paces its own frames, so nothing else may step it.
    #[error("the host paces its own frames and takes no stepping")]
    SelfPaced,
    /// The host is not an interactive scene, so it has nothing to step.
    #[error("the host is not an interactive headless scene")]
    NotInteractive,
    /// The run was asked to write a capture with nothing recording it.
    #[error("the audio world is already recording")]
    AlreadyRecording,
    /// A frame could not be taken, and the run cannot continue.
    #[error(transparent)]
    Turn(#[from] crate::present::TurnError),
    /// An earlier frame already failed, so nothing further is drawn.
    #[error("the windowed run has already failed")]
    AlreadyFailed,
    /// The window's event loop ended in failure.
    #[error("the event loop failed: {source}")]
    EventLoop {
        /// What the window backend reported.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl From<EntityAccessError> for RunError {
    fn from(error: EntityAccessError) -> Self {
        Self::RenderWorld(error)
    }
}

impl From<CaptureWriteError> for RunError {
    fn from(error: CaptureWriteError) -> Self {
        Self::Capture(CaptureError::Write(error))
    }
}

impl From<RunError> for ToolError {
    /// A tool call reports the same failure the run would have logged, told
    /// apart by whether the caller can do anything about it: a request the run
    /// cannot carry out is refused, and work that failed after it was accepted
    /// is a failure.
    fn from(error: RunError) -> Self {
        let kind = match error {
            RunError::AudioRate
            | RunError::NoHttpTransport
            | RunError::NoDriver
            | RunError::SelfPaced
            | RunError::NotInteractive
            | RunError::AlreadyRecording => ToolErrorKind::Rejected,
            _ => ToolErrorKind::Failed,
        };
        Self::new(kind, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use unlit_ecs::mcp::ToolErrorKind;

    use super::RunError;
    use crate::world::{StepError, WorldError, WorldStage};

    /// A scene failure keeps the world's own cause: the run reports what ended
    /// the world, not only that something did.
    #[test]
    fn a_scene_failure_keeps_the_cause_the_world_ended_on() {
        let error = RunError::Scene(WorldError::Panic {
            stage: WorldStage::Body,
            message: "the tick died".into(),
        });
        assert_eq!(
            error.to_string(),
            "the scene failed: the tick panicked: the tick died"
        );
        let source = error.source().expect("the world's failure is the cause");
        assert_eq!(
            source.to_string(),
            "the tick panicked: the tick died",
            "the cause reads as the world recorded it"
        );
    }

    /// A world that could not be stepped says which world and why, so a driver
    /// that gives up on one reports something a reader can act on.
    #[test]
    fn a_step_failure_names_the_world_it_could_not_drive() {
        let error = RunError::Step(StepError::NotStepped);
        assert_eq!(
            error.to_string(),
            "the world could not be driven: the world is threaded and drives itself"
        );
        assert!(error.source().is_some(), "the step error is the cause");
    }

    /// A tool call tells apart what the caller can correct from what it
    /// cannot, which is the reason the run's errors are not just messages.
    #[test]
    fn a_tool_call_branches_on_whether_the_caller_can_help() {
        use unlit_ecs::mcp::ToolError;

        let refused = ToolError::from(RunError::SelfPaced);
        assert_eq!(refused.kind(), ToolErrorKind::Rejected);
        assert_eq!(
            refused.message(),
            "the host paces its own frames and takes no stepping"
        );

        let failed = ToolError::from(RunError::Scene(WorldError::Panic {
            stage: WorldStage::Job,
            message: "the job died".into(),
        }));
        assert_eq!(failed.kind(), ToolErrorKind::Failed);
        assert_eq!(
            failed.message(),
            "the scene failed: the job panicked: the job died"
        );
    }
}
