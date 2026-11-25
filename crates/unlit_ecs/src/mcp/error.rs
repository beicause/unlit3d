//! Why a tool call did not answer with an output.
//!
//! A tool failure reaches a client as structured content, so a client branches
//! on [`ToolErrorKind`] instead of matching a message it cannot rely on: an
//! argument that names nothing is something the caller can correct, a job that
//! ran and failed is not, and a job that panicked leaves the world in a state
//! that needs restarting.

/// What kind of failure a tool call met.
///
/// The kinds are the decisions a caller can make, not the places in the code
/// that raise them: whether to correct the arguments, to try again, or to give
/// up on the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum ToolErrorKind {
    /// The arguments name nothing the world can act on: an entity that is not
    /// there, a scope this world does not hold, a component it does not carry.
    /// Asking again with other arguments is what can help.
    #[error("the arguments do not name anything the world can act on")]
    Rejected,
    /// The tool ran against the world and the work itself failed: a texture
    /// that could not be read back, a file that could not be written. The
    /// world is unchanged by the failure and the same call may be retried.
    #[error("the tool ran and failed")]
    Failed,
    /// The job panicked. The world it ran in may be inconsistent, so this is
    /// the one kind that says the world itself is in doubt.
    #[error("the tool panicked and the world it ran in may be inconsistent")]
    Panicked,
    /// The command never reached a world: the queue refused it, the world had
    /// stopped, or the call was cancelled before the host took it.
    #[error("the command never reached the world")]
    Cancelled,
}

impl ToolErrorKind {
    /// The name this kind carries in structured content.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Panicked => "panicked",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A tool call that answered with a failure instead of an output.
///
/// The message is what a person reads; the kind is what a client branches on,
/// and it survives the transport as the `kind` field of the structured
/// content.
#[derive(Debug)]
pub struct ToolError {
    kind: ToolErrorKind,
    message: String,
}

impl ToolError {
    /// A failure of `kind` carrying `message`.
    #[must_use]
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The arguments named nothing the world can act on.
    #[must_use]
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Rejected, message)
    }

    /// The tool ran and the work itself failed.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Failed, message)
    }

    /// The job panicked, so the world it ran in may be inconsistent.
    #[must_use]
    pub fn panicked(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Panicked, message)
    }

    /// The command never reached a world.
    #[must_use]
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Cancelled, message)
    }

    /// What kind of failure this is.
    #[must_use]
    pub const fn kind(&self) -> ToolErrorKind {
        self.kind
    }

    /// The message a person reads.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The structured content this failure answers with.
    ///
    /// `kind` is the machine-readable half, spelled as
    /// [`ToolErrorKind::as_str`]; `message` is the half a person reads.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind.as_str(), "message": self.message })
    }
}

impl core::fmt::Display for ToolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for ToolError {}

/// A bare message is an argument refusal: it names what the world could not
/// act on. A failure that is not the caller's to correct, and a panic, say so
/// through their own constructors.
impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self::rejected(message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self::rejected(message)
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(error: serde_json::Error) -> Self {
        Self::failed(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ToolError, ToolErrorKind};

    #[test]
    fn a_failure_names_its_kind_in_the_content_a_client_reads() {
        let error = ToolError::rejected("no entity 7");
        assert_eq!(error.kind(), ToolErrorKind::Rejected);
        assert_eq!(
            error.to_value(),
            json!({ "kind": "rejected", "message": "no entity 7" })
        );
        assert_eq!(error.to_string(), "no entity 7");
    }

    #[test]
    fn the_kinds_are_told_apart_by_name() {
        assert_eq!(ToolErrorKind::Rejected.as_str(), "rejected");
        assert_eq!(ToolErrorKind::Failed.as_str(), "failed");
        assert_eq!(ToolErrorKind::Panicked.as_str(), "panicked");
        assert_eq!(ToolErrorKind::Cancelled.as_str(), "cancelled");
    }
}
