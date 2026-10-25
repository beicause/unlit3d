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
use super::tick::advancing;

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
            advancing(move |track: &mut Track, _world, frame| {
                track
                    .midi
                    .advance(f64::from(frame.delta_time), track.voice, &scene.audio);
            }),
        ]
    }
}

/// The music, living on its own entity in the gameplay world.
///
/// It asks the audio world for the synthesizer's voice once, when it is built,
/// then spends that voice on the two tracks the scene plays: the background
/// loop, which starts on its own, and the demo sequence, which waits for the
/// panel.
pub struct SceneAudio {
    /// The voice the audio world owes, and the answer once it comes.
    ///
    /// The install is one request, asked when the state is built: what it
    /// names is nothing but itself, so it is asked under no key at all.
    voice: Pending<(), Option<Entity>>,
    /// Where the answer lands, reused across ticks.
    settled: Vec<((), Settled<Option<Entity>>)>,
    /// Whether the answer has landed, so the scene stops looking.
    installed: bool,
}

impl SceneAudio {
    /// The music state, asking the audio world for a synthesizer built from
    /// `soundfont`.
    ///
    /// The install is asked here rather than on a tick because it is asked
    /// once: the soundfont goes into the job, and the state keeps nothing of
    /// it.
    #[must_use]
    pub fn new(scene: &Scene, soundfont: Arc<[u8]>) -> Self {
        let mut voice = Pending::new(scene.audio_state.clone(), None);
        // The wait has no ticks to run out of: the install is the one thing
        // the music asks for, so it waits until the world answers or ends.
        let installed = match voice.request(&scene.audio, (), move |world: &mut World| {
            install_synth(world, &soundfont)
        }) {
            Ok(()) => false,
            Err(error) => {
                log::error!("the audio world refused the synthesizer install ({error:?})");
                true
            }
        };
        Self {
            voice,
            settled: Vec::new(),
            installed,
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
            audio: SceneAudio::new(scene, soundfont),
            scene: scene.clone(),
        }
    }

    /// Start the music on the voice the install answered with.
    fn tick(audio: &mut SceneAudio, world: &World, scene: &Scene) {
        if audio.installed {
            return;
        }
        let mut settled = core::mem::take(&mut audio.settled);
        audio.voice.advance(&mut settled);
        for (_, settled) in settled.drain(..) {
            audio.installed = true;
            match settled {
                Settled::Answered(Some(voice)) => {
                    start_tracks(&world.root_scope(), scene, voice);
                    log::info!("the background music is playing");
                }
                // No voice: the soundfont failed to load, and the install said
                // so on its way.
                Settled::Answered(None) => {}
                // The install was asked once and never answered, so this is
                // the audio world ending with the job still in its queue.
                Settled::Expired => {
                    log::error!("the audio world ended before it installed the synthesizer");
                }
            }
        }
        audio.settled = settled;
    }
}

impl Bundle for Music {
    fn into_bundle(self) -> DynamicBundle {
        let Self { audio, scene } = self;
        bundle![
            audio,
            advancing(move |audio, world, _frame| Self::tick(audio, world, &scene)),
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
