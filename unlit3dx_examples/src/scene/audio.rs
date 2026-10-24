//! The scene's music: the synthesizer's voice, and the sequences played into it.
//!
//! The audio world is a world of its own, so the scene reaches it through
//! jobs. Installing the voice is one question: it builds the voice and answers
//! the entity that carries it, and every note the gameplay world plays is
//! addressed to that entity afterwards. The sequences themselves live in the
//! gameplay world, with the clocks that advance them, so a track's play state
//! is a component field rather than a job.

use std::sync::Arc;

use unlit_ecs::{Bundle, DynamicBundle, Entity, Scope, World, bundle};
use unlit3dx::prelude::*;

use super::Scene;
use super::music::{bgm_midi, sequence_midi};
use super::order::{ScenePhase, TICK_AUDIO, advancing};
use super::world_ended;

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
}

/// One track and the tick that advances it, spawned together.
///
/// The tick is the only thing that moves the sequence's clock, so a track
/// spawned without it would never play, and a tick without its track would
/// find nothing to advance. It is the one scene object whose tick reads the
/// frame, whose duration is what paces the sequence.
pub(super) struct PlayingTrack {
    /// The sequence the tick advances.
    track: Track,
    /// The worlds the track's messages are sent to.
    scene: Scene,
}

impl PlayingTrack {
    /// The sequence `midi` playing into `voice`, ready to spawn.
    #[must_use]
    fn new(midi: MidiSequencer, voice: Entity, scene: &Scene) -> Self {
        Self {
            track: Track { midi, voice },
            scene: scene.clone(),
        }
    }
}

impl Bundle for PlayingTrack {
    fn into_bundle(self) -> DynamicBundle {
        let Self { track, scene } = self;
        bundle![
            track,
            advancing(
                ScenePhase::Objects.at(TICK_AUDIO),
                move |track: &mut Track, _world, frame| {
                    track
                        .midi
                        .advance(f64::from(frame.delta_time), track.voice, &scene.audio);
                },
            ),
        ]
    }
}

/// The music, living on its own entity in the gameplay world.
///
/// It asks the audio world for the synthesizer's voice once, then spends that
/// voice on the two tracks the scene plays: the background loop, which starts
/// on its own, and the demo sequence, which waits for the panel.
pub struct SceneAudio {
    /// The soundfont the voice is built from.
    soundfont: Arc<[u8]>,
    /// The voice the install answered with, once it has: no answer until the
    /// question is asked.
    voice: Option<SnapshotReader<Option<Entity>>>,
    /// Whether the question has been asked, so it is not asked again.
    requested: bool,
    /// Whether the answer has landed, so the scene stops looking.
    installed: bool,
}

impl SceneAudio {
    /// The music state, waiting to ask the audio world for a synthesizer built
    /// from `soundfont`.
    #[must_use]
    pub fn new(soundfont: Arc<[u8]>) -> Self {
        Self {
            soundfont,
            voice: None,
            requested: false,
            installed: false,
        }
    }
}

/// The music and the tick that starts it, spawned together.
///
/// Starting the tracks is the answer to the audio world's install, so a
/// music state spawned without its tick would ask for a voice nobody waits
/// for.
pub(super) struct Music {
    /// The state the tick advances.
    audio: SceneAudio,
    /// The worlds the synthesizer is asked from and the tracks are put on.
    scene: Scene,
}

impl Music {
    /// The music for the worlds `scene` reaches, playing `soundfont`.
    #[must_use]
    pub(super) fn new(scene: &Scene, soundfont: Arc<[u8]>) -> Self {
        Self {
            audio: SceneAudio::new(soundfont),
            scene: scene.clone(),
        }
    }

    /// Ask for the synthesizer's voice once, and start the music on its answer.
    fn tick(audio: &mut SceneAudio, world: &World, scene: &Scene) {
        if audio.installed {
            return;
        }
        if !audio.requested {
            audio.requested = true;
            let soundfont = Arc::clone(&audio.soundfont);
            match scene
                .audio
                .ask(move |world: &mut World| install_synth(world, &soundfont))
            {
                Ok(voice) => audio.voice = Some(voice),
                Err(error) => {
                    audio.installed = true;
                    log::error!("the audio world refused the synthesizer install ({error:?})");
                    return;
                }
            }
        }
        let Some(reader) = audio.voice.as_ref() else {
            return;
        };
        let Some(voice) = reader.latest().map(|voice| *voice) else {
            // The world that owes the answer is gone: the install job died with
            // it, and no reader will ever see a voice.
            if world_ended(&scene.audio_state) {
                audio.installed = true;
                log::error!("the audio world ended before it installed the synthesizer");
            }
            return;
        };
        audio.installed = true;
        if let Some(voice) = voice {
            start_tracks(&world.root_scope(), scene, voice);
            log::info!("the background music is playing");
        }
    }
}

impl Bundle for Music {
    fn into_bundle(self) -> DynamicBundle {
        let Self { audio, scene } = self;
        bundle![
            audio,
            advancing(
                ScenePhase::Objects.at(TICK_AUDIO),
                move |audio, world, _frame| { Self::tick(audio, world, &scene) }
            ),
        ]
    }
}

/// Put the scene's two sequences on the gameplay world, playing into `voice`.
///
/// The background loop runs from the first frame it can; the one-shot
/// sequence is spawned paused, so the panel's Play is what starts it.
fn start_tracks(world: &Scope<'_>, scene: &Scene, voice: Entity) {
    let commands = world.queue();
    let mut background = MidiSequencer::new(bgm_midi());
    background.loop_play = true;
    let background = commands.spawn(PlayingTrack::new(background, voice, scene));
    let mut sequence = MidiSequencer::new(sequence_midi());
    sequence.paused = true;
    let sequence = commands.spawn(PlayingTrack::new(sequence, voice, scene));
    commands.spawn(bundle![SceneTracks {
        background,
        sequence,
    },]);
}
