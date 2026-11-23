//! Frame progress shared by the application's runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// The number of frames a run has successfully advanced.
///
/// A run advances it once per frame it actually drew, so a capture and a
/// window pace their scenes against the same count. Clones share it, which is
/// how a reader outside the run tells how far the scene has come.
#[derive(Clone, Debug, Default)]
pub struct RunFrames(Arc<AtomicU32>);

impl RunFrames {
    /// How many frames the run has advanced.
    #[must_use]
    pub fn advanced(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// The frame the run stood on when it last advanced one, or [None] before
    /// its first frame.
    #[must_use]
    pub fn current(&self) -> Option<u32> {
        self.advanced().checked_sub(1)
    }

    /// Count one more frame as advanced.
    pub(crate) fn advance(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
