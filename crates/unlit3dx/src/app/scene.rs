//! What a run hands its scene, and the pacing that drives the worlds the
//! scene hands back.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

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
}

impl Stepping {
    /// A stepper for a scene whose physics world runs at `physics_hz`, and —
    /// when `sample_rate` is given — whose synthesizer runs at that rate.
    ///
    /// Gameplay has no rate here: its world ticks once per frame, with the
    /// frame's own duration.
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
    pub fn new(worlds: SceneWorlds, physics_hz: f64, sample_rate: Option<u32>) -> Self {
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
        }
    }

    /// The worlds this stepper drives.
    #[must_use]
    pub fn worlds(&self) -> &SceneWorlds {
        &self.worlds
    }

    /// Step the scene once for `delta_seconds`: gameplay ticks, physics steps
    /// as its rate comes due, and the audio world drains the note jobs those
    /// ticks sent.
    ///
    /// The render world is not stepped here: it is a sub-world the draw
    /// drains, not one with a rate of its own.
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
