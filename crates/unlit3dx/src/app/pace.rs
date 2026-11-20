//! Who advances a run's frames, and what ends it.
//!
//! A run advances frames either from its own loop or at its client's
//! request, and either way it may have a length named for it. Both a
//! windowed run and a served run read the same value, so the two switches
//! behind it mean the same thing wherever they are named.

/// Who advances a run's frames, and the most it advances.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pacing {
    /// Leave the frames to the client: nothing in the run's own loop
    /// advances them, and the MCP stepping tool does instead.
    ///
    /// This is only ever true when a transport reaches the client. Pacing a
    /// run with nothing to unpark it would park it at its first frame for
    /// good, so a run with no transport reads the switch that asks for it
    /// as naming its own pacing instead.
    pub client: bool,
    /// The most frames the run advances, counting from the first. [None]
    /// leaves the run unbounded: it ends when its client asks it to, when
    /// its window closes, or when the process is killed.
    pub frames: Option<u32>,
}

impl Pacing {
    /// Whether `advanced` frames have spent the run's length.
    #[must_use]
    pub fn is_spent(&self, advanced: u32) -> bool {
        matches!(self.frames, Some(frames) if advanced >= frames)
    }
}

#[cfg(test)]
mod tests {
    use super::Pacing;

    #[test]
    fn an_unbounded_run_is_never_spent() {
        let pacing = Pacing {
            client: false,
            frames: None,
        };
        assert!(!pacing.is_spent(0));
        assert!(!pacing.is_spent(u32::MAX));
    }

    #[test]
    fn a_budget_is_spent_once_the_frames_are_advanced() {
        let pacing = Pacing {
            client: true,
            frames: Some(3),
        };
        assert!(!pacing.is_spent(0));
        assert!(!pacing.is_spent(2));
        assert!(pacing.is_spent(3));
        assert!(pacing.is_spent(4));
    }
}
