//! The interactive run: the scene, stepped inline and driven by its caller.
//!
//! Every world is stepped on one thread and advanced one frame at a time, so
//! a caller reproduces exactly what it drove: a test that clicks buttons and
//! reads frames back, or an MCP host that does the same over a process
//! boundary. The GPU is injected, the way the headless run takes one.

use unlit_ecs::{Entity, World, bundle};
use unlit_wgpu::context::RenderDevice;
use unlit_wgpu::resources::ResHandle;
use unlit3d::egui::EguiTarget;
use unlit3d::input::{InputEvent, InputFeed, InputWriter, MouseButton, MouseEvent};
use unlit3d::relay::Snapshot;

use super::App;
use super::capture::Capture;
use super::headless::{AUDIO_SAMPLE_RATE, RENDER_COMMAND_CAPACITY};
use super::offscreen;
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts, SceneWorlds};
use super::stepped::SteppedRun;
use crate::audio::AudioFormat;
use crate::host::{drain_sub_world, spawn_sub_world};
use crate::record::{RecordedAudio, RecordingHandle, start_recording};
use crate::world::{StepOutcome, Stepped, with_stepped_world_mut};

/// An interactive scene stepping inline: one frame per call, no pacing of
/// its own.
pub struct Interactive {
    /// The stepped worlds, the render path, and the frame counter.
    run: SteppedRun,
    input: InputWriter,
    /// The texture the offscreen pass draws into. Its resource id is what a
    /// caller reads frames back through.
    pub target: ResHandle<wgpu::Texture>,
    /// The frames the run writes out, if any. It sits on the scene rather
    /// than on the loop that advances it, so every way of advancing the run
    /// — its own pacing, or a client stepping it — captures the same frames.
    pub capture: Capture,
    recording: Option<RecordingHandle>,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Interactive {
    /// Step `frames` frames: advance every world and pull the mix the frames
    /// would have played.
    ///
    /// A frame is drawn exactly when the capture names it: a frame drawn and
    /// not read is a wasted draw, and a frame the capture names and the run
    /// does not draw is a picture missing from it. A caller that wants the
    /// picture without a capture draws it with [`Interactive::draw`], which
    /// draws where the scene then stands -- the same frame a step would have
    /// drawn, since a draw does not advance the scene.
    ///
    /// The render sub-world dispatches the frame here; the worlds the stepper
    /// drives dispatch theirs in their own ticks. The host world carries no
    /// scene of its own, so it takes no frame: what it would clock is the
    /// stepper's fixed rate, which a reader already knows.
    pub fn step(&mut self, host: &World, frames: u32) -> Result<StepOutcome, String> {
        for _ in 0..frames {
            let index = self.run.frames.advanced();
            let outcome = self.run.advance(host, self.capture.wants(index))?;
            if outcome != StepOutcome::Ran {
                return Ok(outcome);
            }
            self.capture_frame(host, self.run.frame.index)?;
        }
        Ok(StepOutcome::Ran)
    }

    /// Write out the frame `index` the run has just drawn, when the capture
    /// names it.
    ///
    /// A run writes each frame as it reaches it rather than holding them all
    /// to the end: a run with no length of its own keeps what it got to when
    /// the process is killed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn capture_frame(&self, host: &World, index: u32) -> Result<(), String> {
        if self.capture.wants(index) {
            let (width, height, pixels) = pollster::block_on(self.read_frame(host))?;
            if let Some(path) = self.capture.write_frame(index, &pixels, (width, height))? {
                log::info!("the run wrote {}", path.display());
            }
        }
        Ok(())
    }

    /// Nothing is captured on a target with no filesystem: the frames a run
    /// writes are the caller's to read back through
    /// [`Interactive::render_frame`] instead.
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    fn capture_frame(&self, _host: &World, _index: u32) -> Result<(), String> {
        Ok(())
    }

    /// How many frames the run has advanced, which is also the number the
    /// next step draws as.
    #[must_use]
    pub fn frame(&self) -> u32 {
        self.run.frames.advanced()
    }

    /// The handles the scene's worlds were spawned under.
    #[must_use]
    pub fn worlds(&self) -> &SceneWorlds {
        self.run.stepper.worlds()
    }

    /// The entity the render sub-world is spawned under.
    ///
    /// The MCP tools name it as the world a client reads the run's picture
    /// through, so it stays part of what an interactive host offers.
    #[must_use]
    pub fn render_world(&self) -> Entity {
        self.run.render
    }

    /// Draw the current scene without stepping, then read back its pixels.
    pub async fn render_frame(&self, host: &World) -> Result<Vec<u8>, String> {
        self.draw(host)?;
        let (_, _, pixels) = self.read_frame(host).await?;
        Ok(pixels)
    }

    /// Read the rendered frame without drawing or advancing it.
    async fn read_frame(&self, host: &World) -> Result<(u32, u32, Vec<u8>), String> {
        let texture = {
            let mut nested = host
                .root_scope()
                .try_query_one::<&World>(self.run.render)
                .map_err(|error| format!("the render world is unavailable: {error}"))?;
            offscreen::texture(nested.get(), self.run.renderer, &self.target)?
        };
        offscreen::read(&self.device, &self.queue, &texture).await
    }

    /// Drain the render world's queued jobs and draw without stepping.
    pub fn draw(&self, host: &World) -> Result<(), String> {
        let mut nested = host
            .root_scope()
            .try_query_one::<&mut World>(self.run.render)
            .map_err(|error| format!("the render world is unavailable: {error}"))?;
        let world = nested.get();
        let outcome = drain_sub_world(world);
        if outcome != StepOutcome::Ran {
            return Err(format!("the render world cannot draw: {outcome:?}"));
        }
        offscreen::render(world, self.run.renderer)
    }

    /// Click at `position`: move the pointer there, then press and release
    /// before the next frame, the sequence an input tool sends.
    pub fn click(&self, position: [f32; 2]) {
        let button = |pressed| {
            InputEvent::Mouse(MouseEvent::Button {
                position: Some(position),
                button: MouseButton::Primary,
                pressed,
                modifiers: Default::default(),
            })
        };
        self.input.push_all([
            InputEvent::Mouse(MouseEvent::Moved { position }),
            button(true),
            button(false),
        ]);
    }

    /// Start recording the mix the audio world fills.
    ///
    /// The recording lives on the audio world, so its fills are counted even
    /// though the world is stepped from here.
    pub fn start_recording(&mut self, host: &World, max_seconds: f32) -> Result<(), String> {
        if self.recording.is_some() {
            return Err("the audio world is already recording".to_owned());
        }
        self.recording = Some(
            with_stepped_world_mut(host, self.worlds().audio.entity, |audio| {
                start_recording(audio, max_seconds)
            })
            .map_err(|error| format!("recording could not start: {error}"))?,
        );
        Ok(())
    }

    /// Stop the recording and take the samples it captured.
    pub fn stop_recording(&mut self, host: &World) -> Result<Option<Recording>, String> {
        let Some(recording) = self.recording.take() else {
            return Ok(None);
        };
        with_stepped_world_mut(host, self.worlds().audio.entity, |world| {
            recording.stop(world)
        })
        .map(|audio| audio.map(Recording::from))
        .map_err(|error| format!("recording could not stop: {error}"))
    }
}

/// A finished recording: the format it captured at, and the samples it held.
pub struct Recording {
    /// The rate the mix was pulled at, and its channel count.
    pub format: AudioFormat,
    /// The interleaved samples, in the format's channel order.
    pub samples: Vec<f32>,
}

impl From<RecordedAudio> for Recording {
    fn from(audio: RecordedAudio) -> Self {
        Self {
            format: audio.format,
            samples: audio.samples,
        }
    }
}

/// Build an interactive scene: the scene's worlds, an offscreen render
/// target and the stepping interactive.
///
/// The host owns everything, the input writer included; the caller steps it
/// through the returned [`Interactive`] and draws nothing to any window. The
/// offscreen target draws at [`App::SIZE`].
pub fn interactive<A: App>(
    mut app: A,
    device: RenderDevice,
) -> Result<(World, Interactive), String> {
    let mut host = World::new();
    let input = InputWriter::new();
    host.root_scope_mut().spawn(InputFeed(input.clone()));
    let render = spawn_sub_world(&mut host, "render", RENDER_COMMAND_CAPACITY);
    let panel = Snapshot::<EguiTarget>::new();
    let worlds = app.spawn_scene::<Stepped>(
        &mut host,
        ScenePorts {
            render: &render,
            input: &input,
            panel: &panel,
            refresh_millihertz: Default::default(),
            mode: Mode::Interactive,
        },
    );
    let setup = app.setup_render(
        &mut host,
        RenderPorts {
            world: render.entity,
            device: &device,
            panel: Some(panel),
        },
    );
    let (renderer, target) = {
        let mut nested = host
            .root_scope()
            .try_query_one::<&World>(render.entity)
            .map_err(|error| format!("the render world is unavailable: {error}"))?;
        offscreen::bind(nested.get(), setup, &device, A::SIZE)?
    };
    // The MCP tools read the same frame count from the host.
    let frames = RunFrames::default();
    host.root_scope_mut().spawn(bundle![frames.clone()]);
    let interactive = Interactive {
        run: SteppedRun::new(
            worlds,
            A::PHYSICS_HZ,
            frames,
            render.entity,
            renderer,
            AUDIO_SAMPLE_RATE,
        ),
        input,
        target: target.texture,
        recording: None,
        capture: Capture::default(),
        device: device.device,
        queue: device.queue,
    };
    // One frame is drawn before the caller takes over: a drawn frame is what
    // publishes the target the panels lay out against, so a run that never
    // draws lays out no panels and has no buttons to click. That draw is a
    // layout pass, not a frame the run advanced: nothing was stepped for it,
    // so the run still stands on no frame and a capture of frame 0 is the
    // first step's.
    interactive.draw(&host)?;
    Ok((host, interactive))
}
