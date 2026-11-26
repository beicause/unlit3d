//! What a run hands its scene, and the pacing that drives the worlds the
//! scene hands back.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use unlit_ecs::{Entity, RemoteEntity, World};
use unlit_wgpu::context::RenderDevice;
use unlit3d::egui::EguiTarget;
use unlit3d::input::InputWriter;
use unlit3d::relay::Snapshot;

pub use super::frames::RunFrames;
use crate::audio::{AudioFormat, dispatch_audio_fill};
use crate::endpoint::WorldEndpoint;
use crate::thread::fixed_steps;
use crate::time::{AudioSampleTime, FrameDelta, SimulationDelta};
use crate::world::{StepOutcome, WorldError, step_stepped_world, with_stepped_world_mut};

use super::error::RunError;

/// Which run is building the scene.
///
/// The mode is not the world's pacing: that is the `WorldRunner` the scene is
/// spawned with, whose `SELF_DRIVEN` says whether the gameplay world runs a
/// thread of its own. An app reads the mode for what only a mode decides —
/// the audio setup differs between a live window, a capture and an
/// interactive run — and the runner for how the world is paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A window on the display.
    Windowed,
    /// A run without a window that only reads back the frames it captures.
    Headless,
    /// A run stepped one frame at a time by its caller.
    Interactive,
}

/// What a run hands its scene when it spawns it.
#[derive(Clone)]
pub struct ScenePorts<'a> {
    /// The render sub-world the scene draws through.
    pub render: &'a WorldEndpoint,
    /// The input writer of the world that holds the scene's window or driver.
    pub input: &'a InputWriter,
    /// The target the scene's panels lay themselves out against, published by
    /// the drawing side once it has drawn a frame.
    pub panel: &'a Snapshot<EguiTarget>,
    /// The display rate the gameplay world paces itself by when it runs a
    /// thread of its own, in millihertz.
    pub refresh_millihertz: Arc<AtomicU32>,
    /// Which run is asking.
    pub mode: Mode,
}

/// What a run hands its scene when the device is ready.
pub struct RenderPorts<'a> {
    /// The render sub-world the scene builds into.
    pub world: Entity,
    /// The device the frame draws with.
    pub device: &'a RenderDevice,
    /// The panel target to draw, or `None` for a run that draws no panels.
    pub panel: Option<Snapshot<EguiTarget>>,
}

/// The parts of the render world the run drives after the scene built them.
pub struct RenderSetup {
    /// The renderer whose frame the run draws.
    pub renderer: RemoteEntity,
    /// The pass the scene draws into.
    pub pass: RemoteEntity,
}

/// The worlds a scene was spawned into.
#[derive(Clone)]
pub struct SceneWorlds {
    /// The physics world.
    pub physics: WorldEndpoint,
    /// The audio world.
    pub audio: WorldEndpoint,
    /// The gameplay world, which is the scene's own.
    pub gameplay: WorldEndpoint,
}

/// The most audio one frame may be asked to fill, in seconds.
///
/// The audio debt is carried in seconds rather than steps, so the physics
/// catch-up limit says nothing about it. A host that stalled — a suspended
/// window, a debugger pause — would otherwise ask the world to fill a buffer
/// as long as the pause, which is neither wanted nor cheap; a quarter second
/// is well past any jitter a running host shows.
const MAX_AUDIO_CATCH_UP_SECONDS: f64 = 0.25;

/// The world's mix, pulled by the host a frame at a time.
///
/// Only a run that consumes the samples itself owns one of these: the headless
/// run, which captures the mix, and the interactive run, which records it. The
/// samples come out of the audio world's own [`dispatch_audio_fill`], so what
/// a caller receives is exactly what an output stream would: every source's
/// samples, the world's gain, and the clamp.
///
/// A windowed run takes none: its audio world opens an output device of its
/// own and fills the device's ring from its own tick, so there is no buffer for
/// the host to pull. That is why this type, rather than an `Option` on the
/// stepper, is what says whether a run pulls: a run that has no `AudioPull`
/// cannot silently pull nothing.
///
/// [`dispatch_audio_fill`]: crate::audio::dispatch_audio_fill
pub struct AudioPull {
    /// The interleaved samples the last pull filled, reused across frames so
    /// a run does not allocate one buffer per frame.
    samples: Vec<f32>,
    /// The audio time the last frame did not spend.
    debt: f64,
    /// How long one audio frame lasts.
    period: f64,
}

impl AudioPull {
    /// A puller for an audio world whose synthesizer runs at `sample_rate`.
    ///
    /// # Panics
    ///
    /// Panics if `sample_rate` is zero, which names no period at all.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        assert!(sample_rate > 0, "an audio sample rate must be positive");
        Self {
            samples: Vec::new(),
            debt: 0.0,
            period: 1.0 / f64::from(sample_rate),
        }
    }

    /// The samples the last pull filled.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Fill the mix `delta` asks for and hand back the samples.
    ///
    /// The frames come out of `host`'s audio world at `world`, which must be
    /// the stepped world this run drives. A frame whose duration is too short
    /// to fill a whole sample frame fills nothing and carries the fraction
    /// into the next one, the way the physics steps do.
    ///
    /// # Errors
    ///
    /// Returns an error when the audio world is not available to fill.
    pub fn pull(
        &mut self,
        host: &World,
        world: Entity,
        delta: FrameDelta,
        format: AudioFormat,
    ) -> Result<&[f32], RunError> {
        // Debt past the catch-up limit is discarded rather than owed, the way
        // `fixed_steps` discards it: the audio a long stall skipped is gone
        // either way, and filling a buffer as long as the stall would only
        // delay the run further. A negative interval — a clock that went
        // backwards — owes nothing.
        self.debt = (self.debt + delta.as_secs()).min(MAX_AUDIO_CATCH_UP_SECONDS);
        let frames = AudioSampleTime((self.debt / self.period) as usize).0;
        self.debt -= frames as f64 * self.period;
        if frames == 0 {
            self.samples.clear();
            return Ok(&self.samples);
        }
        self.samples.clear();
        self.samples
            .resize(frames * usize::from(format.channels), 0.0);
        with_stepped_world_mut(host, world, |audio| {
            dispatch_audio_fill(
                audio,
                &mut self.samples,
                format.sample_rate,
                format.channels,
            );
        })
        .map_err(RunError::Step)?;
        Ok(&self.samples)
    }
}

/// The frame's own duration drives the worlds, and none of their rates is an
/// integer multiple of the display's, so each frame carries the fraction of a
/// step it did not spend into the next one. Counting seconds rather than
/// frames is what keeps the simulation at its rates on a display that
/// redraws at anything but 60 Hz; a fixed-rate run passes an exact step
/// instead of a measured one, which is what makes it reproducible.
///
/// The mix is not among the worlds this drives at a rate of its own: a run
/// that consumes it pulls it with [`AudioPull`], and a windowed run leaves
/// filling to the audio world's own device.
pub struct Stepping {
    /// The worlds this stepper drives.
    worlds: SceneWorlds,
    /// The physics time the last frame did not spend.
    physics_debt: SimulationDelta,
    /// How long one physics step lasts.
    physics_period: SimulationDelta,
}

impl Stepping {
    /// A stepper for a scene whose physics world runs at `physics_hz`.
    ///
    /// Gameplay has no rate here: its world ticks once per frame, with the
    /// frame's own duration. Neither does the mix: a run that consumes the
    /// samples pulls them with [`AudioPull`], at the rate it names there.
    ///
    /// # Panics
    ///
    /// Panics if `physics_hz` gives no positive, finite period.
    #[must_use]
    pub fn new(worlds: SceneWorlds, physics_hz: f64) -> Self {
        let physics_period = SimulationDelta::from_secs((1.0 / physics_hz) as f32);
        assert!(
            physics_period != SimulationDelta::ZERO,
            "a physics rate must be a positive finite number, not {physics_hz}"
        );
        Self {
            worlds,
            physics_debt: SimulationDelta::ZERO,
            physics_period,
        }
    }

    /// The worlds this stepper drives.
    #[must_use]
    pub fn worlds(&self) -> &SceneWorlds {
        &self.worlds
    }

    /// Step the scene once for `delta`: gameplay ticks, physics steps
    /// as its rate comes due, and the audio world drains the note jobs those
    /// ticks sent.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
    /// The caller accounts for the frame after drawing it.
    pub fn step(&mut self, host: &World, delta: FrameDelta) -> StepOutcome {
        // Gameplay runs once per host frame with its measured duration: its
        // requests reach the other worlds before they step, and a slow frame
        // never causes a burst of catch-up ticks.
        let outcome = match step_stepped_world(host, self.worlds.gameplay.entity(), delta) {
            Ok(outcome) => outcome,
            Err(source) => return StepOutcome::Failed(WorldError::Step { source }),
        };
        if outcome != StepOutcome::Ran {
            return outcome;
        }
        let steps = fixed_steps(
            &mut self.physics_debt,
            SimulationDelta::from_secs(delta.as_secs_f32()),
            self.physics_period,
        );
        for _ in 0..steps {
            let outcome = match step_stepped_world(
                host,
                self.worlds.physics.entity(),
                FrameDelta::from_secs(f64::from(self.physics_period.as_secs())),
            ) {
                Ok(outcome) => outcome,
                Err(source) => return StepOutcome::Failed(WorldError::Step { source }),
            };
            if outcome != StepOutcome::Ran {
                return outcome;
            }
        }
        let outcome = match step_stepped_world(host, self.worlds.audio.entity(), delta) {
            Ok(outcome) => outcome,
            Err(source) => return StepOutcome::Failed(WorldError::Step { source }),
        };
        if outcome != StepOutcome::Ran {
            return outcome;
        }
        StepOutcome::Ran
    }
}

#[cfg(test)]
mod tests {
    use super::RunFrames;

    #[test]
    fn a_frame_count_starts_at_none_and_names_the_frame_it_reaches() {
        let frames = RunFrames::default();
        assert_eq!(frames.advanced(), 0);
        assert_eq!(frames.current(), None);
        frames.advance();
        assert_eq!(frames.advanced(), 1);
        assert_eq!(frames.current(), Some(0));
        frames.advance();
        assert_eq!(frames.current(), Some(1));
    }

    #[test]
    fn a_clone_reads_the_count_its_original_advanced() {
        let frames = RunFrames::default();
        let cloned = frames.clone();
        frames.advance();
        frames.advance();
        assert_eq!(cloned.advanced(), 2);
        assert_eq!(frames.advanced(), 2);
    }
}
