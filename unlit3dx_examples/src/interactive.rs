//! The interactive run: the whole scene, stepped inline and driven by input.
//!
//! The windowed mode runs the same scene across threads, paced by the display.
//! This run keeps every world stepped on one thread and advances it one frame
//! at a time, so a caller reproduces exactly what it drove: the wasm test that
//! clicks buttons and reads frames back, or a native one that does the same
//! without a process boundary. The GPU is injected, the way the headless run
//! takes one, so a test asks for a headless device and a windowed caller
//! builds its own from the surface.

use std::sync::Arc;

use unlit_ecs::{Entity, World, bundle};
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::input::{InputEvent, InputWriter, dispatch_input};
use unlit3d::prelude::*;
use unlit3dx::prelude::*;

use crate::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};
use crate::scene::{
    AudioSetup, Ball, HEADLESS_HZ, PanelLayout, PanelSlots, SceneHandles, SceneStepper,
    SceneTracks, Track, WINDOW_SIZE, mount_scene_content, setup_render_world, spawn_scene,
};

/// An interactive scene stepping inline: one frame per call, no pacing of its own.
pub struct Interactive {
    worlds: SceneHandles,
    render: Entity,
    renderer: Entity,
    /// The view id the offscreen frames draw into, for a caller that reads
    /// them back through the render world's resource graph.
    pub target: u64,
    /// The entity the render sub-world is spawned under.
    pub render_world: Entity,
    /// The texture the offscreen pass draws into, for a caller that reads
    /// frames back.
    frame_texture: wgpu::Texture,
    stepper: SceneStepper,
    frame: Frame,
    samples: Vec<f32>,
    recording: Option<unlit3dx::record::RecordingHandle>,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Interactive {
    /// Step `frames` frames: dispatch the queued input, advance every world,
    /// pull the mix the frame would have played and draw the frame offscreen.
    pub fn step(&mut self, host: &World, frames: u32) {
        for _ in 0..frames {
            dispatch_input(host);
            self.stepper.step(host, &self.worlds, 1.0 / HEADLESS_HZ);
            let frames = self.stepper.audio_frames(1.0 / HEADLESS_HZ);
            if frames > 0 {
                self.samples.clear();
                self.samples.resize(frames * AUDIO_CHANNELS, 0.0);
                let filled = with_stepped_world(host, self.worlds.audio.entity, |audio| {
                    dispatch_audio_fill(
                        audio,
                        &mut self.samples,
                        AUDIO_SAMPLE_RATE,
                        AUDIO_CHANNELS as u16,
                    );
                });
                assert!(filled.is_some(), "the audio world is a stepped world");
            }
            let mut nested = host
                .root_scope()
                .query_one::<&mut World>(self.render)
                .expect("the render sub-world exists");
            drain_sub_world(&mut nested);
            dispatch_frame(&mut nested, &mut self.frame);
            let _ = nested
                .root_scope()
                .query_one::<&mut Renderer>(self.renderer)
                .map(|mut renderer| renderer.render(&nested));
            self.frame.index = self.frame.index.wrapping_add(1);
        }
    }

    /// The frame the next step draws.
    pub fn frame(&self) -> u32 {
        self.frame.index
    }

    /// The handles the scene's worlds were spawned under.
    pub fn worlds_handle(&self) -> &SceneHandles {
        &self.worlds
    }

    /// The panel buttons the last drawn frame laid out, keyed by label.
    ///
    /// The layout lives on the gameplay world, refreshed on every tick.
    pub fn panel_buttons(
        host: &World,
        worlds: &SceneHandles,
    ) -> Vec<(&'static str, unlit3d::ui::egui::Rect)> {
        with_stepped_world(host, worlds.gameplay.entity, |world| {
            world
                .root_scope()
                .query::<&PanelLayout>()
                .next()
                .map(|layout| {
                    layout
                        .buttons
                        .iter()
                        .map(|(label, rect)| (*label, *rect))
                        .collect()
                })
                .unwrap_or_default()
        })
        .expect("the gameplay world is a stepped world")
    }

    /// Draw the current frame again and read its pixels back.
    ///
    /// A caller that compares the frame against a baseline draws one more
    /// frame than it stepped: the pass draws wherever the scene stood when it
    /// ran, and the readback blocks until the copy lands.
    pub fn render_frame(&self, host: &World) -> Vec<u8> {
        let nested = host
            .root_scope()
            .query_one::<&mut World>(self.render)
            .expect("the render sub-world exists");
        let rendered = nested
            .root_scope()
            .query_one::<&mut Renderer>(self.renderer)
            .map(|mut renderer| renderer.render(&nested));
        let _ = rendered;
        unlit_wgpu::readback::readback_texture(&self.device, &self.queue, &self.frame_texture)
    }

    /// The samples the last step pulled from the audio world.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Click at `position`: move the pointer there, press and release,
    /// one frame apiece, the sequence a windowed click produces.
    pub fn click(&mut self, host: &World, position: [f32; 2]) {
        for pressed in [true, false] {
            let event = InputEvent::Mouse(unlit3d::input::MouseEvent::Button {
                position,
                button: unlit3d::input::MouseButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
            host.root_scope()
                .query::<&InputWriter>()
                .next()
                .expect("the interactive run carries an input writer")
                .push(event);
            self.step(host, 1);
        }
    }

    /// Start recording the mix the audio world fills.
    ///
    /// The recording lives on the audio world, so its fills are counted even
    /// though the world is stepped from here.
    pub fn start_recording(&mut self, host: &World, max_seconds: f32) {
        let mut handle = None;
        with_stepped_world_mut(host, self.worlds.audio.entity, |audio| {
            handle = Some(unlit3dx::record::start_recording(audio, max_seconds));
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
///
/// The recorder answers [`unlit3dx::record::RecordedAudio`]; this type carries
/// the same samples across the module boundary that hides the crate's internals.
pub struct Recording {
    /// The rate the mix was pulled at, and its channel count.
    pub format: AudioFormat,
    /// The interleaved samples, in the format's channel order.
    pub samples: Vec<f32>,
}

impl From<unlit3dx::record::RecordedAudio> for Recording {
    fn from(audio: unlit3dx::record::RecordedAudio) -> Self {
        Self {
            format: audio.format,
            samples: audio.samples,
        }
    }
}

/// The scene state a caller asserts on: the balls and the sequence.
#[derive(Clone, Copy)]
pub struct SceneStatus {
    /// How many balls the gameplay world holds.
    pub balls: usize,
    /// Whether the MIDI sequence is paused, and where it has got to.
    pub sequence: Option<(bool, f64, bool)>,
}

/// Report the scene state: the balls the gameplay world holds and where its
/// MIDI sequence stands.
pub fn scene_status(host: &World, worlds: &SceneHandles) -> SceneStatus {
    with_stepped_world(host, worlds.gameplay.entity, |world| {
        let scope = world.root_scope();
        let balls = scope.query::<&Ball>().count();
        let sequence = scope.query::<&SceneTracks>().next().and_then(|tracks| {
            scope
                .query_one::<&Track>(tracks.sequence)
                .ok()
                .map(|track| (track.midi.paused, track.midi.time, track.midi.finished))
        });
        SceneStatus { balls, sequence }
    })
    .expect("the gameplay world is a stepped world")
}

/// The settings one interactive run builds with.
pub struct InteractiveConfig {
    /// The GPU the scene renders with.
    pub device: wgpu::Device,
    /// The queue the scene's commands submit through.
    pub queue: wgpu::Queue,
    /// The limits and features the device was requested with.
    pub capabilities: DeviceCapabilities,
    /// The soundfont the synthesizer plays, as raw bytes; an empty slice
    /// silences the audio world the way the headless run's does.
    pub soundfont: Arc<[u8]>,
}

/// Build the interactive scene: an input writer on the host, the scene's
/// worlds, an offscreen render target and the stepping interactive.
///
/// The host owns everything; the caller steps it through the returned
/// [`Interactive`] and draws nothing to any window.
pub fn run(config: InteractiveConfig) -> (World, InputWriter, Interactive) {
    let mut host = World::new();
    let input = InputWriter::new();
    host.root_scope_mut()
        .spawn(bundle![input.clone(), input.subscribe()]);
    let render = spawn_sub_world(&mut host, "render", 64);
    let panels = PanelSlots::default();
    let audio_setup = if config.soundfont.is_empty() {
        AudioSetup::Silent
    } else {
        AudioSetup::Pulled {
            soundfont: config.soundfont,
            autoplay_background: false,
        }
    };
    let worlds = spawn_scene::<Stepped>(
        &mut host,
        render.clone(),
        audio_setup,
        panels.clone(),
        &input,
        false,
    );
    let (renderer, pass) = setup_render_world(
        &mut host,
        render.entity,
        &config.device,
        &config.queue,
        config.capabilities,
    );
    mount_scene_content(&render, pass, Some(panels));
    let target = {
        let nested = host
            .root_scope()
            .query_one::<&mut World>(render.entity)
            .expect("the render sub-world exists");
        let context = nested
            .root_scope()
            .query_one::<&Renderer>(renderer.0)
            .expect("the renderer is a resource entity")
            .context();
        bind_offscreen(&nested, context, pass.0, &config.device, WINDOW_SIZE)
            .expect("the offscreen target binds")
    };
    let interactive = Interactive {
        worlds,
        render: render.entity,
        renderer: renderer.0,
        render_world: render.entity,
        target: target.view.id().to_bits(),
        frame_texture: target.texture,
        stepper: SceneStepper::new(PHYSICS_HZ, Some(AUDIO_SAMPLE_RATE)),
        frame: Frame {
            delta_time: (1.0 / HEADLESS_HZ) as f32,
            index: 0,
            size: WINDOW_SIZE,
        },
        samples: Vec::new(),
        recording: None,
        device: config.device,
        queue: config.queue,
    };
    (host, input, interactive)
}
