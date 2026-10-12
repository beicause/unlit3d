//! A soundfont-backed [`PcmSource`]: one or more synthesizers, mixed.
//!
//! The soundfont loads on the audio thread, where the [`SynthVoice`] it
//! becomes is spawned as a component of the audio world. Gameplay never
//! touches the synthesizer: it sends note jobs through the world's command
//! channel, and the mixer renders whatever voices exist into the stream's
//! interleaved frames.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustysynth_ext::{
    SoundFont, SoundFontError, Synthesizer, SynthesizerError, SynthesizerSettings,
};
use unlit_ecs::{Entity, World};

use crate::audio::PcmSource;
use crate::job::Job;

/// Build a mixer backed by the soundfont at the path, loaded when the audio
/// thread opens its output, at unit gain.
///
/// A soundfont that fails to load logs and leaves the world silent rather
/// than taking the stream down with it.
pub fn synth_source(path: impl Into<PathBuf>) -> crate::audio::PcmSourceFactory {
    synth_source_with_gain(path, 1.0)
}

/// Build a mixer backed by the soundfont at the path, loaded when the audio
/// thread opens its output, with every sample scaled by `gain`.
///
/// Soundfont presets leave a lot of headroom, so a mix of a few quiet voices
/// often sits far below full scale; `gain` makes it up. Samples clamp to the
/// unit range after the gain, so an overdriven mix distorts rather than
/// wraps. A soundfont that fails to load logs and leaves the world silent
/// rather than taking the stream down with it.
pub fn synth_source_with_gain(
    path: impl Into<PathBuf>,
    gain: f32,
) -> crate::audio::PcmSourceFactory {
    let path = path.into();
    Box::new(move |world: &mut World, sample_rate: u32, _channels: u16| {
        // Decoding an sf3's vorbis samples is the load's whole cost: a few
        // seconds in release, tens of them in an unoptimized debug build.
        log::info!("loading the soundfont at {}", path.display());
        match SynthVoice::spawn(world, &path, sample_rate) {
            Ok(_) => {
                log::info!("the soundfont at {} loaded", path.display());
                Box::new(SynthMixer::new(gain))
            }
            Err(error) => {
                log::error!(
                    "the soundfont at {} failed to load ({error})",
                    path.display()
                );
                Box::new(SynthMixer::new(gain))
            }
        }
    })
}

/// A loaded soundfont and the synthesizer playing it, as one component.
pub struct SynthVoice {
    /// The soundfont the synthesizer reads its instruments from.
    _font: Arc<SoundFont>,
    /// The synthesizer that turns note events into samples.
    pub synthesizer: Synthesizer,
}

impl SynthVoice {
    /// Load the soundfont at the path and spawn it as a component.
    ///
    /// # Errors
    ///
    /// Returns the soundfont error when the file is missing or malformed,
    /// and the synthesizer error when its settings are out of range.
    pub fn spawn(world: &mut World, path: &Path, sample_rate: u32) -> Result<Entity, VoiceError> {
        let mut file = File::open(path)?;
        let font = Arc::new(SoundFont::new(&mut file)?);
        let settings = SynthesizerSettings::new(i32::try_from(sample_rate).unwrap_or(48000));
        let synthesizer = Synthesizer::new(&font, &settings)?;
        Ok(world.spawn((Self {
            _font: font,
            synthesizer,
        },)))
    }
}

/// Why a [`SynthVoice`] could not be built.
#[derive(Debug)]
pub enum VoiceError {
    /// The soundfont file could not be read.
    Io(std::io::Error),
    /// The file is not a soundfont.
    SoundFont(SoundFontError),
    /// The synthesizer rejected its settings.
    Synthesizer(SynthesizerError),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "the soundfont file failed ({error})"),
            Self::SoundFont(error) => write!(f, "the soundfont failed ({error})"),
            Self::Synthesizer(error) => write!(f, "the synthesizer failed ({error})"),
        }
    }
}

impl std::error::Error for VoiceError {}

impl From<std::io::Error> for VoiceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SoundFontError> for VoiceError {
    fn from(error: SoundFontError) -> Self {
        Self::SoundFont(error)
    }
}

impl From<SynthesizerError> for VoiceError {
    fn from(error: SynthesizerError) -> Self {
        Self::Synthesizer(error)
    }
}

/// Renders every [`SynthVoice`] in the world into interleaved frames.
#[derive(Clone, Debug)]
struct SynthMixer {
    /// The left channel's scratch, reused across fills.
    left: Vec<f32>,
    /// The right channel's scratch, reused across fills.
    right: Vec<f32>,
    /// The factor every rendered sample scales by.
    gain: f32,
}

impl SynthMixer {
    /// A mixer whose samples scale by `gain`.
    fn new(gain: f32) -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            gain,
        }
    }
}

impl PcmSource for SynthMixer {
    fn fill(&mut self, world: &World, out: &mut [f32], _sample_rate: u32, channels: u16) {
        let channels = usize::from(channels.max(1));
        let frames = out.len() / channels;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        self.left.fill(0.0);
        self.right.fill(0.0);
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.render(&mut self.left, &mut self.right);
        }
        // Everything past the first two channels of a wider stream is silent.
        for (frame, out) in out.chunks_mut(channels).enumerate() {
            for (channel, sample) in out.iter_mut().enumerate() {
                *sample = (match channel {
                    0 => self.left[frame],
                    _ => self.right[frame],
                } * self.gain)
                    .clamp(-1.0, 1.0);
            }
        }
    }
}

/// A job that presses a key down on every synthesizer in the world.
pub fn note_on_job(channel: i32, key: i32, velocity: i32) -> Job<()> {
    Box::new(move |world| {
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.note_on(channel, key, velocity);
        }
    })
}

/// A job that releases a key on every synthesizer in the world.
pub fn note_off_job(channel: i32, key: i32) -> Job<()> {
    Box::new(move |world| {
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.note_off(channel, key);
        }
    })
}

/// A job that releases every key, immediately or with the envelope decayed.
pub fn notes_off_all_job(immediate: bool) -> Job<()> {
    Box::new(move |world| {
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.note_off_all(immediate);
        }
    })
}
