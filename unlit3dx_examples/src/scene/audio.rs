//! The scene's music: the synthesizer's sources, and the two tracks it plays.
//!
//! The audio world is a world of its own, so the scene reaches it through
//! jobs. Two things have to happen in order: the sources must be installed
//! once the world has an output to render into, and only then can a track be
//! put on. This object is what watches for both, and what starts the
//! background loop when the world can finally sound.

use unlit_ecs::World;
use unlit3dx::prelude::*;

use super::SceneWiring;
use super::music::bgm_midi;
use super::tick::Tick;

/// The factor every synthesizer sample scales by, making up the headroom
/// soundfont presets leave.
pub const SYNTH_GAIN: f32 = 2.0;

/// The track name the looping background music plays on.
pub const BGM_TRACK: &str = "background";

/// The track name the demo's MIDI sequence plays on.
pub const SEQUENCE_TRACK: &str = "sequence";

/// How a scene's audio world gets its synthesizer and its output.
pub enum AudioSetup {
    /// No synthesizer: the world stays silent, and its control panel
    /// shows no tracks.
    Silent,
    /// Open an output device and install the sources once it reports its
    /// rate.
    ///
    /// A self-driven world opens one of its own. A browser's stepped
    /// world opens the page's Web Audio context instead: the page drives
    /// it, so it needs no thread of its own, and the sources are built at
    /// the rate it reports.
    Device,
    /// Open no device: the host pulls the fills with [`dispatch_audio_fill`],
    /// so the sources are installed before the first frame, at
    /// `sample_rate`.
    Pulled {
        /// The soundfont to build the synthesizer from.
        soundfont: std::sync::Arc<[u8]>,
        /// The rate the sources render at.
        sample_rate: u32,
    },
}

/// Subscribe the music to the audio world's readiness.
///
/// The world can sound once its output is open *and* it carries the sources a
/// transport job addresses: a job that reaches it before the soundfont is
/// parsed finds nobody to address and is lost. The collector answers once, on
/// the first tick both hold, and nothing after it — the state never goes
/// back, and a mailbox that stopped changing stops waking its reader.
pub fn subscribe_audio(sender: &JobSender) -> SnapshotReader<()> {
    let mut published = false;
    sender
        .subscribe(move |world: &World| {
            if published
                || world
                    .root_view()
                    .query::<&AudioOutputOpen>()
                    .next()
                    .is_none()
                || world.root_view().query::<&OnAudioFill>().next().is_none()
            {
                return None;
            }
            published = true;
            Some(())
        })
        .expect("the audio world accepts a subscription")
}

/// Install the demo's audio sources on an audio world: the mix's gain, one
/// live voice for note jobs and one `MidiPlayer` per name in `tracks`,
/// each with the `OnAudioFill` that renders it.
///
/// The rate is not a choice but the output's: an audio thread spawns its
/// [`AudioFormat`] component when its stream opens, and a synthesizer built
/// before that would render at the wrong speed. Until someone installs
/// sources a world stays silent, and the music's readiness subscription does
/// not fire.
///
/// # Panics
///
/// Panics when two names in `tracks` are the same: a transport job could
/// not tell the two tracks apart.
pub fn install_synth(
    world: &mut World,
    soundfont: &[u8],
    gain: f32,
    tracks: &[&str],
    sample_rate: u32,
) {
    for (index, one) in tracks.iter().enumerate() {
        assert!(
            !tracks[..index].contains(one),
            "the track name `{one}` is given twice"
        );
    }
    world.root_view_mut().spawn((AudioGain(gain),));
    // Decoding an sf3's vorbis samples is the load's whole cost: a few
    // seconds in release, tens of them in an unoptimized debug build.
    log::info!("loading the embedded soundfont");
    match load_soundfont(soundfont) {
        Ok(font) => {
            if let Err(error) = SynthVoice::from_font(world, &font, sample_rate) {
                log::error!("the synthesizer failed to start ({error})");
            }
            for track in tracks {
                if let Err(error) = MidiPlayer::from_font(world, &font, sample_rate, track) {
                    log::error!("the midi track `{track}` failed to start ({error})");
                }
            }
        }
        Err(error) => log::error!("the soundfont failed to load ({error})"),
    }
}

/// A job that installs the demo's two tracks on an audio world that has
/// just reported its output format.
pub fn install_audio_job_with(soundfont: std::sync::Arc<[u8]>, sample_rate: u32) -> Job<()> {
    Box::new(move |world: &mut World| {
        install_synth(
            world,
            &soundfont,
            SYNTH_GAIN,
            &[BGM_TRACK, SEQUENCE_TRACK],
            sample_rate,
        );
    })
}

/// The music, living on its own entity in the gameplay world.
///
/// It installs the synthesizer's sources once the audio world reports an
/// output, and starts the background loop once the world says it can sound.
/// The panel drives the tracks afterwards; this object only gets them going.
pub struct SceneAudio {
    /// The audio world's command channel.
    audio: JobSender,
    /// The audio world's readiness mailbox: the music starts only once the
    /// world says it can sound, because a track job that reaches the world
    /// before its players exist finds nobody to address.
    ready: SnapshotReader<()>,
    /// The audio output's format, when the scene waits for a device to
    /// report one.
    format: Option<SnapshotReader<AudioFormat>>,
    /// Whether the sources have been queued.
    installed: bool,
    /// Whether the background music has been started.
    bgm_started: bool,
}

impl SceneAudio {
    /// The music for the worlds `wiring` reaches, installing its sources
    /// once `format` reports an output.
    #[must_use]
    pub fn new(wiring: &SceneWiring, format: Option<SnapshotReader<AudioFormat>>) -> Self {
        Self {
            audio: wiring.audio_sender.clone(),
            ready: wiring.audio_ready.clone(),
            format,
            installed: false,
            bgm_started: false,
        }
    }
}

impl Tick for SceneAudio {
    fn tick(&mut self, _world: &World) {
        // The sources first: a track job that reaches a world without them
        // finds nobody to address and is lost.
        if !self.installed
            && let Some(format) = &self.format
            && let Some(format) = format.latest()
        {
            // The readiness mailbox fires on the tick the sources appear, so
            // it cannot see them before this job has run: the world reports
            // ready a tick after the install, and the music starts then.
            self.installed = true;
            let install = install_audio_job_with(super::SOUNDFONT.into(), format.sample_rate);
            if let Err(error) = self.audio.send(install) {
                log::error!("the audio world refused its sources ({error:?})");
            }
        }
        // The audio world says when it can sound. Until its players exist
        // a track job finds nobody to address and is lost, so the music
        // starts on the first moment the world reports it is up. Only the
        // background loop autoplays; the one-shot sequence is started by
        // hand from the panel.
        if !self.bgm_started && self.ready.latest().is_some() {
            self.bgm_started = true;
            self.audio
                .send(play_track_job(BGM_TRACK, bgm_midi(), true))
                .ok();
            log::info!("the background music is playing");
        }
    }
}
