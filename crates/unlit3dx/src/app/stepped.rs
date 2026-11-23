//! The frame an inline-stepped run takes, shared by every entry that steps one.
//!
//! The headless and interactive runs advance a frame the same way: fold the
//! input, step the worlds, fill the mix the frame's duration asks for, draw the
//! render world, and count the frame. They differ only in what they keep of
//! the result, so the sequence lives here once.

use unlit_ecs::{Entity, World};
use unlit3d::frame::Frame;
use unlit3d::input::refresh_input;

use super::headless::{AUDIO_CHANNELS, FIXED_FRAME_SECONDS};
use super::offscreen;
use super::scene::{AudioPull, RunFrames, SceneWorlds, Stepping};
use crate::audio::AudioFormat;
use crate::host::tick_sub_world;
use crate::world::StepOutcome;

/// The worlds of one inline-stepped run, the frame counter they share, and the
/// render path that draws each frame.
pub(super) struct SteppedRun {
    /// Steps the worlds, carrying the physics remainder across frames.
    pub(super) stepper: Stepping,
    /// The frames the run has advanced.
    pub(super) frames: RunFrames,
    /// The frame the last advance took: its index, and the fixed step it was
    /// given.
    pub(super) frame: Frame,
    /// The mix's puller. It keeps the samples of the last advance, so a run
    /// that keeps the mix reads them back with [`AudioPull::samples`].
    pub(super) mix: AudioPull,
    pub(super) format: AudioFormat,
    /// The render sub-world the frames are drawn in, and the renderer drawing
    /// them.
    pub(super) render: Entity,
    pub(super) renderer: Entity,
}

impl SteppedRun {
    pub(super) fn new(
        worlds: SceneWorlds,
        physics_hz: f64,
        frames: RunFrames,
        render: Entity,
        renderer: Entity,
        sample_rate: u32,
    ) -> Self {
        Self {
            stepper: Stepping::new(worlds, physics_hz),
            frames,
            frame: Frame {
                delta_time: FIXED_FRAME_SECONDS as f32,
                index: 0,
            },
            mix: AudioPull::new(sample_rate),
            format: AudioFormat {
                sample_rate,
                channels: AUDIO_CHANNELS as u16,
            },
            render,
            renderer,
        }
    }

    /// Advance one frame: fold the host's input, step the worlds, fill the mix
    /// the frame asks for, then draw the render world when `draw` asks and
    /// count the frame.
    ///
    /// Drawing is the caller's to ask for because a draw is not what advances
    /// the scene: it reads what the steps left behind, and what it writes is
    /// only what a frame publishes for its readers, which the next frame
    /// replaces. A run that reads the state back rather than the picture
    /// therefore skips the draw entirely, which on a software rasterizer is the
    /// difference between a run that finishes and one that does not. A scene
    /// that accumulates across draws -- a ping-pong target, a temporal filter --
    /// needs every frame drawn, which is the caller's to ask for too.
    ///
    /// Anything but [`StepOutcome::Ran`] ends the frame where it stands and is
    /// returned as the outcome; the frame is not counted.
    pub(super) fn advance(&mut self, host: &World, draw: bool) -> Result<StepOutcome, String> {
        refresh_input(host);
        let outcome = self.stepper.step(host, FIXED_FRAME_SECONDS);
        if outcome != StepOutcome::Ran {
            return Ok(outcome);
        }
        self.frame.index = self.frames.advanced();
        // A recording observes the fills, so the frame has to fill even when
        // nothing reads the samples back: the notes the frame booked only become
        // recorded audio through a fill.
        self.mix.pull(
            host,
            self.stepper.worlds().audio.entity,
            FIXED_FRAME_SECONDS,
            self.format,
        )?;
        {
            let mut nested = host
                .root_scope()
                .try_query_one::<&mut World>(self.render)
                .map_err(|error| format!("the render world is unavailable: {error}"))?;
            let world = nested.get();
            let outcome = tick_sub_world(world, &mut self.frame);
            if outcome != StepOutcome::Ran {
                return Ok(outcome);
            }
            if draw {
                offscreen::render(world, self.renderer)?;
            }
        }
        self.frames.advance();
        Ok(StepOutcome::Ran)
    }
}
