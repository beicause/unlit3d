//! What a run hands its scene, and the pacing that drives the worlds the
//! scene hands back.

use std::sync::Arc;
#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use unlit_ecs::{Entity, RemoteEntity, World};
use unlit_wgpu::context::RenderDevice;
use unlit3d::egui::EguiTarget;
use unlit3d::input::InputWriter;
use unlit3d::relay::Snapshot;

use crate::handle::WorldHandle;
use crate::thread::fixed_steps;
use crate::world::step_stepped_world;

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
    /// A run without a window that draws only the frames it captures.
    Headless,
    /// A run stepped one frame at a time by its caller.
    Interactive,
}

/// What a run hands its scene when it spawns it.
#[derive(Clone)]
pub struct ScenePorts<'a> {
    /// The render sub-world the scene draws through.
    pub render: &'a WorldHandle,
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
    pub physics: WorldHandle,
    /// The audio world.
    pub audio: WorldHandle,
    /// The gameplay world, which is the scene's own.
    pub gameplay: WorldHandle,
}

/// The most audio one frame may be asked to fill, in seconds.
///
/// The audio debt is carried in sample frames rather than steps, so the
/// physics catch-up limit says nothing about it. A host that stalled — a
/// suspended window, a debugger pause — would otherwise ask the world to
/// fill a buffer as long as the pause, which is neither wanted nor cheap; a
/// quarter second is well past any jitter a running host shows.
const MAX_AUDIO_CATCH_UP_SECONDS: f64 = 0.25;

/// How many frames a run has advanced, shared between whoever advances them
/// and whoever reads the count.
///
/// A run builds one before it starts and hands a clone to each driver: a
/// stepped run's [`Stepping`] advances it once per step, and a threaded
/// window's draw advances it once per frame it can show. The frame budget, the
/// capture and the report of frames a capture never reached all read this one
/// number, so they name the frames the run actually advanced instead of each
/// counting its own way.
///
/// It is an atomic rather than a plain counter because a frame behaviour holds
/// only a shared reference to the world it runs in, and advancing the count is
/// what the drivers do while the readers hold their own clones.
#[derive(Clone, Debug, Default)]
pub struct RunFrames(Arc<AtomicU32>);

impl RunFrames {
    /// How many frames the run has advanced.
    #[must_use]
    pub fn advanced(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// The frame the run stood on when it last advanced one, or [None]
    /// before its first frame.
    #[must_use]
    pub fn current(&self) -> Option<u32> {
        self.advanced().checked_sub(1)
    }

    /// Count one more frame as advanced.
    pub(crate) fn advance(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// The pacing a host uses to step a scene's worlds frame by frame.
///
/// The frame's own duration is what drives the worlds, and none of their
/// rates is an integer multiple of the display's, so each frame carries the
/// fraction of a step it did not spend into the next one. Counting seconds
/// rather than frames is what keeps the simulation at its rates on a display
/// that redraws at anything but 60 Hz; a fixed-rate run passes an exact step
/// instead of a measured one, which is what makes it reproducible.
pub struct Stepping {
    /// The worlds this stepper drives.
    worlds: SceneWorlds,
    /// The physics time the last frame did not spend.
    physics_debt: f32,
    /// The audio time the last frame did not spend.
    audio_debt: f64,
    /// How long one physics step lasts.
    physics_period: f32,
    /// How long one audio frame lasts, or none when the host pulls no audio.
    sample_period: Option<f64>,
    /// The run's frame count, advanced once per [`Self::step`].
    frames: RunFrames,
}

impl Stepping {
    /// A stepper for a scene whose physics world runs at `physics_hz`, and —
    /// when `sample_rate` is given — whose synthesizer runs at that rate.
    ///
    /// Gameplay has no rate here: its world ticks once per frame, with the
    /// frame's own duration.
    ///
    /// `frames` is the run's shared frame count: the stepper advances it once
    /// per step, so a host that reads it knows how many frames the run has
    /// advanced rather than how many turns its loop has taken.
    ///
    /// The rate is turned into a period once, so a rate that is not a positive
    /// finite number has no period to carry: it would divide the frame's
    /// duration into an infinity or a NaN of steps. `sample_rate` is trusted
    /// as given, being an integer the caller cannot make non-positive without
    /// saying so.
    ///
    /// # Panics
    ///
    /// Panics when `physics_hz` is not a positive finite number.
    #[must_use]
    pub fn new(
        worlds: SceneWorlds,
        physics_hz: f64,
        sample_rate: Option<u32>,
        frames: RunFrames,
    ) -> Self {
        assert!(
            physics_hz.is_finite() && physics_hz > 0.0,
            "a physics rate must be a positive finite number, not {physics_hz}"
        );
        Self {
            worlds,
            physics_debt: 0.0,
            audio_debt: 0.0,
            physics_period: 1.0 / physics_hz as f32,
            sample_period: sample_rate.map(|rate| 1.0 / f64::from(rate)),
            frames,
        }
    }

    /// The worlds this stepper drives.
    #[must_use]
    pub fn worlds(&self) -> &SceneWorlds {
        &self.worlds
    }

    /// The run's frame count, which this stepper advances.
    #[must_use]
    pub fn frames(&self) -> &RunFrames {
        &self.frames
    }

    /// Step the scene once for `delta_seconds`: gameplay ticks, physics steps
    /// as its rate comes due, and the audio world drains the note jobs those
    /// ticks sent.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
    ///
    /// The count the stepper shares advances here, once every tick has run, so
    /// the frame this step produced is the one it is named by.
    pub fn step(&mut self, host: &World, delta_seconds: f64) {
        // Gameplay runs once per host frame with its measured duration: its
        // requests reach the other worlds before they step, and a slow frame
        // never causes a burst of catch-up ticks.
        step_stepped_world(host, self.worlds.gameplay.entity, delta_seconds as f32)
            .expect("the stepped world starts");
        let steps = fixed_steps(
            &mut self.physics_debt,
            delta_seconds as f32,
            self.physics_period,
        );
        for _ in 0..steps {
            step_stepped_world(host, self.worlds.physics.entity, self.physics_period)
                .expect("the stepped world starts");
        }
        step_stepped_world(host, self.worlds.audio.entity, delta_seconds as f32)
            .expect("the stepped world starts");
        self.frames.advance();
    }

    /// The audio frames this frame's duration asks for, carrying the fraction
    /// it did not spend into the next frame.
    ///
    /// Zero when the stepper was built without a sample rate.
    pub fn audio_frames(&mut self, delta_seconds: f64) -> usize {
        let Some(period) = self.sample_period else {
            return 0;
        };
        // Debt past the catch-up limit is discarded rather than owed, the way
        // `fixed_steps` discards it: a host that stalled for a long while
        // would otherwise ask the world to fill a buffer as long as the pause,
        // and the audio that pause skipped is gone either way. A negative
        // interval — a clock that went backwards — owes nothing.
        self.audio_debt =
            (self.audio_debt + delta_seconds.max(0.0)).min(MAX_AUDIO_CATCH_UP_SECONDS);
        let frames = (self.audio_debt / period) as usize;
        self.audio_debt -= frames as f64 * period;
        frames
    }
}

/// The stepper a client-paced host parks in its world.
///
/// Spawning one is what declares a host client-paced: the `step_frames` tool
/// advances the scene through the stepper it finds here, and reports the host
/// as pacing its own frames when it finds none. The worlds stay stepped
/// worlds either way, because a world on a thread of its own would step
/// behind the client's back.
///
/// A self-paced host keeps its stepper inside the frame behaviour that drives
/// it, which no tool can reach — that is what stops a client from advancing
/// the scene a second way beside the host's own loop.
///
/// Only a build with the tools has a client to hand the state to, so the type
/// exists only there.
#[cfg(all(
    feature = "mcp",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp")))]
pub struct ClientPacing(pub Arc<Mutex<Stepping>>);

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
