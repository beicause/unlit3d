//! The scene's music: the synthesizer's voice, and the sequences played into it.
//!
//! The audio world is a world of its own, so the scene reaches it through
//! jobs. Installing the voice is one round trip: the job builds it and answers
//! the entity that carries it, and every note the gameplay world plays is
//! addressed to that entity afterwards. The sequences themselves live in the
//! gameplay world, with the clocks that advance them, so a track's play state
//! is a component field rather than a job.

use std::sync::Arc;

use unlit_ecs::{Entity, Scope, World, bundle};
use unlit3d::frame::Frame;
use unlit3dx::prelude::*;

use super::SceneWiring;
use super::music::{bgm_midi, sequence_midi};
use super::tick::{TICK_AUDIO, Tick, tick_behaviour};

/// How a scene's audio world gets its synthesizer and its output.
pub enum AudioSetup {
    /// No synthesizer: the world stays silent, and its control panel
    /// shows no tracks.
    Silent,
    /// Open an output device, and build the synthesizer from the embedded
    /// soundfont.
    Device,
    /// Open no device: the host pulls the fills with [`dispatch_audio_fill`],
    /// so the synthesizer renders at whatever rate those fills name.
    Pulled {
        /// The soundfont to build the synthesizer from.
        soundfont: Arc<[u8]>,
    },
}

/// Install the demo's synthesizer on an audio world, answering the entity
/// that carries it.
///
/// The voice builds its synthesizer on the first fill it renders, at the
/// rate that fill names, so it can be installed before the world has an
/// output at all.
pub fn install_synth(world: &mut World, soundfont: &[u8]) -> Option<Entity> {
    // Decoding an sf3's vorbis samples is the load's whole cost: a few
    // seconds in release, tens of them in an unoptimized debug build.
    log::info!("loading the embedded soundfont");
    match load_soundfont(soundfont) {
        Ok(font) => Some(SynthVoice::from_font(world, &font)),
        Err(error) => {
            log::error!("the soundfont failed to load ({error})");
            None
        }
    }
}

/// A job that installs the demo's synthesizer on an audio world, answering
/// the entity that carries the voice.
pub fn install_audio_job(soundfont: Arc<[u8]>) -> Job<Option<Entity>> {
    Box::new(move |world: &mut World| install_synth(world, &soundfont))
}

/// The tracks the control panel transports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneTracks {
    /// The looping background music.
    pub background: Entity,
    /// The one-shot sequence the panel starts.
    pub sequence: Entity,
}

/// One sequence playing into the audio world.
///
/// The gameplay world owns it and advances it once a frame, which sends the
/// messages that frame made due to `voice`. The voice is installed once and
/// addressed ever after, so a track keeps its handle for as long as it lives.
pub struct Track {
    /// The sequence, and the clock that advances it.
    pub midi: MidiSequencer,
    /// The voice in the audio world the messages go to.
    pub voice: Entity,
    /// The audio world's command channel.
    pub audio: JobSender,
}

impl Tick for Track {
    fn tick(&mut self, _world: &World, frame: &Frame) {
        self.midi
            .advance(f64::from(frame.delta_time), self.voice, &self.audio);
    }
}

/// The music, living on its own entity in the gameplay world.
///
/// It installs the synthesizer's voice once, then spends that voice on the
/// two tracks the scene plays: the background loop, which starts on its own,
/// and the demo sequence, which waits for the panel.
pub struct SceneAudio {
    /// The audio world's command channel.
    audio: JobSender,
    /// The soundfont the voice is built from.
    soundfont: Arc<[u8]>,
    /// The install round trip, until it answers.
    install: Option<Request<Option<Entity>>>,
    /// Whether the install has answered, so the round trip is not asked for
    /// again once it has.
    installed: bool,
}

impl SceneAudio {
    /// The music for the worlds `wiring` reaches, playing `soundfont`.
    #[must_use]
    pub fn new(wiring: &SceneWiring, soundfont: Arc<[u8]>) -> Self {
        Self {
            audio: wiring.audio_sender.clone(),
            soundfont,
            install: None,
            installed: false,
        }
    }
}

impl Tick for SceneAudio {
    fn tick(&mut self, world: &World, _frame: &Frame) {
        if self.installed {
            return;
        }
        if self.install.is_none() {
            self.install = Some(
                self.audio
                    .request(install_audio_job(self.soundfont.clone())),
            );
        }
        let Some(install) = self.install.take() else {
            return;
        };
        match install.poll() {
            Some(Some(voice)) => {
                self.installed = true;
                start_tracks(&world.root_scope(), &self.audio, voice);
                log::info!("the background music is playing");
            }
            Some(None) => self.installed = true,
            None if install.dropped() => {
                self.installed = true;
                log::error!("the audio world dropped the synthesizer install");
            }
            None => self.install = Some(install),
        }
    }
}

/// Put the scene's two sequences on the gameplay world, playing into `voice`.
///
/// The background loop runs from the first frame it can; the one-shot
/// sequence is spawned paused, so the panel's Play is what starts it.
fn start_tracks(world: &Scope<'_>, audio: &JobSender, voice: Entity) {
    let commands = world.queue();
    let mut background = MidiSequencer::new(bgm_midi());
    background.loop_play = true;
    let background = commands.spawn(bundle!(
        Track {
            midi: background,
            voice,
            audio: audio.clone(),
        },
        tick_behaviour::<Track>(TICK_AUDIO),
    ));
    let mut sequence = MidiSequencer::new(sequence_midi());
    sequence.paused = true;
    let sequence = commands.spawn(bundle!(
        Track {
            midi: sequence,
            voice,
            audio: audio.clone(),
        },
        tick_behaviour::<Track>(TICK_AUDIO),
    ));
    commands.spawn(bundle!(SceneTracks {
        background,
        sequence,
    }));
}
