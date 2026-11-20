//! The interactive run: the scene, stepped inline and driven by its caller.
//!
//! Every world is stepped on one thread and advanced one frame at a time, so
//! a caller reproduces exactly what it drove: a test that clicks buttons and
//! reads frames back, or an MCP host that does the same over a process
//! boundary. The GPU is injected, the way the headless run takes one.

use unlit_ecs::{Entity, World};
use unlit_wgpu::context::RenderDevice;
use unlit_wgpu::readback::try_readback_texture;
use unlit_wgpu::resources::ResHandle;
use unlit3d::egui::EguiTarget;
use unlit3d::frame::Frame;
use unlit3d::input::{InputEvent, InputFeed, InputWriter, MouseButton, MouseEvent, refresh_input};
use unlit3d::offscreen::{PersistentTarget, TargetConfig};
use unlit3d::relay::Snapshot;
use unlit3d::renderer::Renderer;

use super::App;
use super::capture::Capture;
use super::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, HEADLESS_HZ, RENDER_COMMAND_CAPACITY};
use super::scene::{Mode, RenderPorts, RunFrames, ScenePorts, SceneWorlds, Stepping};
use crate::audio::{AudioFormat, dispatch_audio_fill};
use crate::host::{drain_sub_world, spawn_sub_world, tick_sub_world};
use crate::record::{RecordedAudio, RecordingHandle, start_recording};
use crate::world::{Stepped, with_stepped_world_mut};

/// An interactive scene stepping inline: one frame per call, no pacing of
/// its own.
pub struct Interactive {
    worlds: SceneWorlds,
    /// The render sub-world the frames are drawn in.
    render: Entity,
    renderer: Entity,
    /// The texture the offscreen pass draws into. Its resource id is what a
    /// caller reads frames back through.
    pub target: ResHandle<wgpu::Texture>,
    /// The frames the run writes out, if any. It sits on the scene rather
    /// than on the loop that advances it, so every way of advancing the run
    /// — its own pacing, or a client stepping it — captures the same frames.
    pub capture: Capture,
    stepper: Stepping,
    frame: Frame,
    samples: Vec<f32>,
    recording: Option<RecordingHandle>,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Interactive {
    /// Step `frames` frames: advance every world and pull the mix the frames
    /// would have played.
    ///
    /// Nothing is drawn here, so a run that only reads the state back rather
    /// than the picture costs no draw calls. [`Interactive::draw`] draws the
    /// frame where the scene then stands, which is the same frame a step
    /// would have drawn: a draw does not advance the scene, and what it does
    /// write is only what a frame publishes for its readers, which the next
    /// frame replaces anyway.
    ///
    /// The render sub-world dispatches the frame here; the worlds the stepper
    /// drives dispatch theirs in their own ticks. The host world carries no
    /// scene of its own, so it takes no frame: what it would clock is the
    /// stepper's fixed rate, which a reader already knows.
    pub fn step(&mut self, host: &World, frames: u32) {
        for _ in 0..frames {
            // The host has no behaviour of its own to route input to, but it
            // holds a buffer so a caller can feed and read one, and a buffer
            // no one folds keeps every event it is fed. Folding it is the
            // whole of what this world needs on a tick.
            refresh_input(host);
            self.stepper.step(host, 1.0 / HEADLESS_HZ);
            // The stepper advanced the run's frame count as it stepped, so
            // the frame it just produced is the one that count names: the
            // tick, the capture and the frame a reader asks for all agree on
            // which frame this is.
            self.frame.index = self.stepper.frames().current().unwrap_or_default();
            let frames = self.stepper.audio_frames(1.0 / HEADLESS_HZ);
            if frames > 0 {
                self.samples.clear();
                self.samples.resize(frames * AUDIO_CHANNELS, 0.0);
                let filled = with_stepped_world_mut(host, self.worlds.audio.entity, |audio| {
                    dispatch_audio_fill(
                        audio,
                        &mut self.samples,
                        AUDIO_SAMPLE_RATE,
                        AUDIO_CHANNELS as u16,
                    );
                });
                assert!(filled.is_ok(), "the audio world is a stepped world");
            }
            {
                let mut nested = host.root_scope().query_one::<&mut World>(self.render);
                tick_sub_world(nested.get(), &mut self.frame);
            }
            // The frame is drawn exactly when the capture names it: a frame
            // drawn and not read is a wasted draw, and a frame the capture
            // names and the run does not draw is a picture missing from it.
            // The sub-world's borrow is released first, since drawing takes
            // it again.
            self.capture_frame(host, self.frame.index);
        }
    }

    /// Write out the frame `index` the run has just drawn, when the capture
    /// names it.
    ///
    /// A run writes each frame as it reaches it rather than holding them all
    /// to the end: a run with no length of its own keeps what it got to when
    /// the process is killed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn capture_frame(&self, host: &World, index: u32) {
        if !self.capture.wants(index) {
            return;
        }
        self.draw(host);
        // A frame that cannot be read back is one the capture has to name as
        // missing: taking the run down would lose the frames after it too.
        let (size, pixels) = match pollster::block_on(self.try_read_frame(host)) {
            Ok(frame) => ((frame.0, frame.1), frame.2),
            Err(reason) => {
                log::warn!("the run could not capture frame {index}: {reason}");
                return;
            }
        };
        match self.capture.write_frame(index, &pixels, size) {
            Ok(Some(path)) => log::info!("the run wrote {}", path.display()),
            Ok(None) => {}
            Err(message) => log::error!("{message}"),
        }
    }

    /// Nothing is captured on a target with no filesystem: the frames a run
    /// writes are the caller's to read back through
    /// [`Interactive::render_frame`] instead.
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    fn capture_frame(&self, _host: &World, _index: u32) {}

    /// How many frames the run has advanced, which is also the number the
    /// next step draws as.
    #[must_use]
    pub fn frame(&self) -> u32 {
        self.stepper.frames().advanced()
    }

    /// The handles the scene's worlds were spawned under.
    #[must_use]
    pub fn worlds(&self) -> &SceneWorlds {
        &self.worlds
    }

    /// The entity the render sub-world is spawned under.
    ///
    /// The MCP tools name it as the world a client reads the run's picture
    /// through, so it stays part of what an interactive host offers.
    #[must_use]
    pub fn render_world(&self) -> Entity {
        self.render
    }

    /// Draw the frame where the scene stands and read its pixels back.
    ///
    /// The await is the readback: the pixels cannot exist before the copy
    /// lands. Drawing is what publishes the target the panels lay out
    /// against, so nothing is laid out until one frame has been drawn.
    ///
    /// # Panics
    ///
    /// Panics when the target cannot be read back. A caller that drives the
    /// run itself is holding the device and the format it bound the target
    /// with, so an unreadable target is a mistake in its own setup rather
    /// than a device that went away mid-run.
    pub async fn render_frame(&self, host: &World) -> Vec<u8> {
        self.draw(host);
        let (_, _, pixels) = self.read_frame(host).await;
        pixels
    }

    /// Read the target's pixels back without drawing first.
    ///
    /// The size comes from the texture, which is the size the target was
    /// bound at.
    async fn read_frame(&self, host: &World) -> (u32, u32, Vec<u8>) {
        self.try_read_frame(host)
            .await
            .unwrap_or_else(|error| panic!("the frame's target could not be read back: {error}"))
    }

    /// Read the target's pixels back, reporting why when it cannot.
    async fn try_read_frame(
        &self,
        host: &World,
    ) -> Result<(u32, u32, Vec<u8>), unlit_wgpu::readback::ReadbackError> {
        let texture = {
            let mut nested = host.root_scope().query_one::<&World>(self.render);
            let world = nested.get();
            let context = world
                .root_scope()
                .query_one::<&Renderer>(self.renderer)
                .get()
                .context();
            let mut graph = context
                .try_graph(&world.root_scope())
                .expect("the frame's resource graph exists");
            graph
                .get()
                .get(&self.target)
                .cloned()
                .expect("the target texture is a resource node")
        };
        let size = (texture.width(), texture.height());
        let pixels = try_readback_texture(&self.device, &self.queue, &texture).await?;
        Ok((size.0, size.1, pixels))
    }

    /// Draw the frame where the scene stands, into the offscreen texture.
    ///
    /// The render sub-world catches up on the jobs queued for it first, so
    /// the frame draws the scene as the steps left it rather than as the jobs
    /// found it.
    pub fn draw(&self, host: &World) {
        let mut nested = host.root_scope().query_one::<&mut World>(self.render);
        let world = nested.get();
        drain_sub_world(world);
        let _ = world
            .root_scope()
            .try_query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| renderer.get().render(&*world));
    }

    /// Click at `position`: move the pointer there, then press and release
    /// before the next frame, the sequence an input tool sends.
    pub fn click(&mut self, host: &World, position: [f32; 2]) {
        let button = |pressed| {
            InputEvent::Mouse(MouseEvent::Button {
                position: Some(position),
                button: MouseButton::Primary,
                pressed,
                modifiers: Default::default(),
            })
        };
        host.root_scope()
            .query::<&InputWriter>()
            .iter()
            .next()
            .expect("the interactive run carries an input writer")
            .push_all([
                InputEvent::Mouse(MouseEvent::Moved { position }),
                button(true),
                button(false),
            ]);
    }

    /// Start recording the mix the audio world fills.
    ///
    /// The recording lives on the audio world, so its fills are counted even
    /// though the world is stepped from here.
    pub fn start_recording(&mut self, host: &World, max_seconds: f32) {
        let mut handle = None;
        with_stepped_world_mut(host, self.worlds.audio.entity, |audio| {
            handle = Some(start_recording(audio, max_seconds));
        })
        .expect("the audio world is a stepped world");
        self.recording = handle;
    }

    /// Stop the recording and take the samples it captured.
    pub fn stop_recording(&mut self, host: &World) -> Option<Recording> {
        let recording = self.recording.take()?;
        let mut audio = None;
        with_stepped_world_mut(host, self.worlds.audio.entity, |world| {
            audio = recording.stop(world);
        })
        .expect("the audio world is a stepped world");
        audio.map(Recording::from)
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
pub fn interactive<A: App>(mut app: A, device: RenderDevice) -> (World, Interactive) {
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
    let target = {
        let mut nested = host.root_scope().query_one::<&mut World>(render.entity);
        let world = nested.get();
        let context = world
            .root_scope()
            .query_one::<&Renderer>(setup.renderer.entity)
            .get()
            .context();
        PersistentTarget::bind(
            world,
            context,
            setup.pass.entity,
            &device.device,
            A::SIZE,
            TargetConfig::default(),
        )
        .expect("the offscreen target binds")
    };
    // The count the run's length is read from. Nothing else advances it
    // here: the stepper is this run's only driver.
    let frames = RunFrames::default();
    let interactive = Interactive {
        stepper: Stepping::new(
            worlds.clone(),
            A::PHYSICS_HZ,
            Some(AUDIO_SAMPLE_RATE),
            frames,
        ),
        worlds,
        render: render.entity,
        renderer: setup.renderer.entity,
        target: target.texture,
        frame: Frame {
            delta_time: (1.0 / HEADLESS_HZ) as f32,
            index: 0,
        },
        samples: Vec::new(),
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
    interactive.draw(&host);
    (host, interactive)
}
