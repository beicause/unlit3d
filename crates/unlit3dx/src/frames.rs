//! Frame progress shared by render drivers and application loops.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// The number of frames a run has successfully advanced.
///
/// This type lives below the application feature so a presenter can account
/// for successful draws without depending on the application skeleton.
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
