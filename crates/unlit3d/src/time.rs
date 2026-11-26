//! Time values carried by frame dispatch and animation playback.

/// Elapsed seconds of a dispatched frame. Zero advances no frame-driven clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct FrameDelta(f64);

impl FrameDelta {
    /// A frame without elapsed time.
    pub const ZERO: Self = Self(0.0);

    /// Non-finite or non-positive input produces a zero delta.
    #[must_use]
    pub const fn from_secs(seconds: f64) -> Self {
        if seconds.is_finite() && seconds > 0.0 {
            Self(seconds)
        } else {
            Self::ZERO
        }
    }

    /// The elapsed seconds, without narrowing.
    #[must_use]
    pub const fn as_secs(self) -> f64 {
        self.0
    }

    /// The elapsed seconds, saturating at the largest finite `f32`.
    #[must_use]
    pub fn as_secs_f32(self) -> f32 {
        self.0.min(f64::from(f32::MAX)) as f32
    }
}

/// A position on a clip's timeline, in seconds from its start.
///
/// This does not constrain the time to a particular clip's length. Playback
/// may wrap or clamp against that length; manual sampling holds the first or
/// last key outside a channel's range.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
#[repr(transparent)]
#[cfg_attr(feature = "reflect", derive(facet::Facet))]
#[cfg_attr(feature = "reflect", facet(transparent))]
pub struct AnimationTime(f32);

impl AnimationTime {
    /// The start of the clip.
    pub const START: Self = Self(0.0);

    /// Non-finite or non-positive input selects the start of the clip.
    #[must_use]
    pub const fn from_secs(seconds: f32) -> Self {
        if seconds.is_finite() && seconds > 0.0 {
            Self(seconds)
        } else {
            Self::START
        }
    }

    /// The clip position in seconds. This also validates values written by reflection.
    #[must_use]
    pub const fn as_secs(self) -> f32 {
        Self::from_secs(self.0).0
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationTime, FrameDelta};

    #[test]
    fn invalid_clock_values_do_not_advance_time() {
        for seconds in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, -0.0] {
            assert_eq!(FrameDelta::from_secs(f64::from(seconds)), FrameDelta::ZERO);
            assert_eq!(AnimationTime::from_secs(seconds), AnimationTime::START);
        }
    }

    #[test]
    fn narrowing_a_large_frame_delta_stays_finite() {
        assert_eq!(FrameDelta::from_secs(f64::MAX).as_secs_f32(), f32::MAX);
    }
}
