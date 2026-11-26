//! Clock domains used by world drivers, physics, requests, and the audio mixer.

use std::time::Duration;

pub use unlit3d::time::FrameDelta;

/// The exact 60 Hz step used by runs without a display.
pub const FIXED_FRAME_DELTA: FrameDelta = FrameDelta::from_secs(1.0 / 60.0);

/// The default rate of physics steps, independent of the frame rate, in hertz.
pub const DEFAULT_PHYSICS_HZ: f64 = 60.0;

/// A duration of simulation work. Zero accrues no simulation debt.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct SimulationDelta(f32);

impl SimulationDelta {
    /// A tick with no new simulation time.
    pub const ZERO: Self = Self(0.0);

    /// Non-finite or non-positive input produces a zero delta.
    #[must_use]
    pub const fn from_secs(seconds: f32) -> Self {
        if seconds.is_finite() && seconds > 0.0 {
            Self(seconds)
        } else {
            Self::ZERO
        }
    }

    /// The simulation duration in seconds.
    #[must_use]
    pub const fn as_secs(self) -> f32 {
        self.0
    }
}

/// Time advanced by completed physics steps, in seconds since initialization.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct SimulationTime(pub(crate) f64);

impl SimulationTime {
    /// The simulation clock in seconds. Ticks without a physics step do not advance it.
    #[must_use]
    pub const fn as_secs(self) -> f64 {
        self.0
    }
}

/// A position or length in sample frames, independent of the channel count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioSampleTime(pub usize);

impl AudioSampleTime {
    /// The nearest sample frame at `sample_rate`. Non-finite or non-positive
    /// input gives frame zero. Positions beyond `usize::MAX` saturate.
    #[must_use]
    pub fn from_secs(seconds: f64, sample_rate: u32) -> Self {
        Self((FrameDelta::from_secs(seconds).as_secs() * f64::from(sample_rate)).round() as usize)
    }

    /// The duration in seconds, or `None` when the rate is zero.
    #[must_use]
    pub fn as_secs(self, sample_rate: u32) -> Option<f64> {
        (sample_rate != 0).then(|| self.0 as f64 / f64::from(sample_rate))
    }
}

/// A request's monotonic wall-clock wait, independent of simulation pauses.
/// Zero permits only answers or world termination already visible at the poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallTimeout(pub Duration);

#[cfg(test)]
mod tests {
    use super::{AudioSampleTime, SimulationDelta};

    #[test]
    fn sample_time_rounds_at_the_output_rate() {
        let position = AudioSampleTime::from_secs(0.25, 48_000);
        assert_eq!(position, AudioSampleTime(12_000));
        assert_eq!(position.as_secs(48_000), Some(0.25));
        assert_eq!(position.as_secs(0), None);
        assert_eq!(
            AudioSampleTime::from_secs(f64::NAN, 48_000),
            AudioSampleTime(0)
        );
    }

    #[test]
    fn invalid_simulation_delta_adds_no_time() {
        for seconds in [-1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(SimulationDelta::from_secs(seconds), SimulationDelta::ZERO);
        }
    }
}
