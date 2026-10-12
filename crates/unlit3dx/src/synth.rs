//! A soundfont-backed [`PcmSource`]: synthesizers
//! and MIDI sequencers, mixed.
//!
//! The soundfont loads on the audio thread, where the [`SynthVoice`] and
//! [`MidiPlayer`] it feeds are spawned as components of the audio world.
//! Gameplay never touches the synthesizer: it sends note jobs and MIDI
//! files through the world's command channel, and the mixer renders
//! whatever sources exist into the stream's interleaved frames.

use std::sync::Arc;

pub use rustysynth_ext::MidiFile;
use rustysynth_ext::{
    MidiFileError, MidiFileSequencer, MidiMessage, SoundFont, SoundFontError, Synthesizer,
    SynthesizerError, SynthesizerSettings,
};
use unlit_ecs::{Entity, World};

use crate::audio::PcmSource;
use crate::job::Job;

/// Build a mixer backed by the soundfont in `bytes`, loaded when the audio
/// thread opens its output, with every sample scaled by `gain`.
///
/// The bytes come from the binary itself, typically an `include_bytes!` of
/// the soundfont, so no filesystem is involved and the same code runs on
/// wasm. The factory spawns both a [`SynthVoice`] for live notes and a
/// [`MidiPlayer`] for sequenced playback.
///
/// Soundfont presets leave a lot of headroom, so a mix of a few quiet
/// voices often sits far below full scale; `gain` makes it up. Samples
/// clamp to the unit range after the gain, so an overdriven mix distorts
/// rather than wraps. A soundfont that fails to load logs and leaves the
/// world silent rather than taking the stream down with it.
pub fn synth_source_bytes(bytes: Arc<[u8]>, gain: f32) -> crate::audio::PcmSourceFactory {
    Box::new(move |world: &mut World, sample_rate: u32, _channels: u16| {
        // Decoding an sf3's vorbis samples is the load's whole cost: a few
        // seconds in release, tens of them in an unoptimized debug build.
        log::info!("loading the embedded soundfont");
        match load_soundfont(&bytes) {
            Ok(font) => {
                if let Err(error) = SynthVoice::from_font(world, &font, sample_rate) {
                    log::error!("the synthesizer failed to start ({error})");
                }
                if let Err(error) = MidiPlayer::from_font(world, &font, sample_rate) {
                    log::error!("the midi player failed to start ({error})");
                }
            }
            Err(error) => log::error!("the soundfont failed to load ({error})"),
        }
        Box::new(SynthMixer::new(gain))
    })
}

/// Parses the soundfont in `bytes`.
///
/// # Errors
///
/// Returns the soundfont error when the bytes are malformed.
pub fn load_soundfont(bytes: &[u8]) -> Result<Arc<SoundFont>, VoiceError> {
    let mut reader = bytes;
    Ok(Arc::new(SoundFont::new(&mut reader)?))
}

/// Parses a standard MIDI file from `bytes`, ready for
/// [`play_midi_job`].
///
/// # Errors
///
/// Returns the MIDI error when the bytes are not a valid SMF.
pub fn load_midi_file(bytes: &[u8]) -> Result<Arc<MidiFile>, VoiceError> {
    let mut reader = bytes;
    Ok(Arc::new(MidiFile::new(&mut reader)?))
}

/// A timed MIDI message: seconds since the sequence starts, then the
/// status byte — with the channel in its low nibble — and the two data
/// bytes.
pub type MidiEvent = (f64, u8, u8, u8);

/// Builds a MIDI file from timed messages, ready for
/// [`play_midi_job`].
///
/// The times must be in non-decreasing order.
///
/// # Errors
///
/// Returns the MIDI error when the times are out of order.
pub fn midi_file(events: &[MidiEvent]) -> Result<Arc<MidiFile>, VoiceError> {
    Ok(Arc::new(MidiFile::new_with_events(events.iter().map(
        |(time, status, data1, data2)| {
            (
                *time,
                MidiMessage::Normal {
                    status: *status,
                    data1: *data1,
                    data2: *data2,
                },
            )
        },
    ))?))
}

/// A loaded soundfont and the synthesizer playing it, as one component.
pub struct SynthVoice {
    /// The soundfont the synthesizer reads its instruments from.
    _font: Arc<SoundFont>,
    /// The synthesizer that turns note events into samples.
    pub synthesizer: Synthesizer,
}

impl SynthVoice {
    /// Spawns a voice playing `font`.
    ///
    /// # Errors
    ///
    /// Returns the synthesizer error when its settings are out of range.
    pub fn from_font(
        world: &mut World,
        font: &Arc<SoundFont>,
        sample_rate: u32,
    ) -> Result<Entity, VoiceError> {
        Ok(world.spawn((Self {
            _font: font.clone(),
            synthesizer: Self::synthesizer(font, sample_rate)?,
        },)))
    }

    /// Parses `bytes` as a soundfont and spawns a voice playing it.
    ///
    /// # Errors
    ///
    /// Returns the soundfont error when the bytes are malformed, and the
    /// synthesizer error when its settings are out of range.
    pub fn from_bytes(
        world: &mut World,
        bytes: &[u8],
        sample_rate: u32,
    ) -> Result<Entity, VoiceError> {
        Self::from_font(world, &load_soundfont(bytes)?, sample_rate)
    }

    /// The synthesizer `font` plays through, at `sample_rate`.
    fn synthesizer(font: &Arc<SoundFont>, sample_rate: u32) -> Result<Synthesizer, VoiceError> {
        let settings = SynthesizerSettings::new(i32::try_from(sample_rate).unwrap_or(48000));
        Ok(Synthesizer::new(font, &settings)?)
    }
}

/// A MIDI file sequencer and its own synthesizer, as one component.
///
/// The sequencer keeps its own clock, advanced by the samples its render
/// pulls, so playback stays sample-accurate no matter how the host paces
/// its ticks.
pub struct MidiPlayer {
    /// The sequencer walking the current file's events.
    sequencer: MidiFileSequencer,
}

impl MidiPlayer {
    /// Spawns a player reading `font`.
    ///
    /// # Errors
    ///
    /// Returns the synthesizer error when its settings are out of range.
    pub fn from_font(
        world: &mut World,
        font: &Arc<SoundFont>,
        sample_rate: u32,
    ) -> Result<Entity, VoiceError> {
        let sequencer = MidiFileSequencer::new(Self::synthesizer(font, sample_rate)?);
        Ok(world.spawn((Self { sequencer },)))
    }

    /// Parses `bytes` as a soundfont and spawns a player reading it.
    ///
    /// # Errors
    ///
    /// Returns the soundfont error when the bytes are malformed, and the
    /// synthesizer error when its settings are out of range.
    pub fn from_bytes(
        world: &mut World,
        bytes: &[u8],
        sample_rate: u32,
    ) -> Result<Entity, VoiceError> {
        Self::from_font(world, &load_soundfont(bytes)?, sample_rate)
    }

    /// The synthesizer `font` plays through, at `sample_rate`.
    fn synthesizer(font: &Arc<SoundFont>, sample_rate: u32) -> Result<Synthesizer, VoiceError> {
        let settings = SynthesizerSettings::new(i32::try_from(sample_rate).unwrap_or(48000));
        Ok(Synthesizer::new(font, &settings)?)
    }
}

/// Why a soundfont or a MIDI file could not be built.
#[derive(Debug)]
pub enum VoiceError {
    /// The bytes are not a soundfont.
    SoundFont(SoundFontError),
    /// The bytes are not a MIDI file, or its events are out of order.
    MidiFile(MidiFileError),
    /// The synthesizer rejected its settings.
    Synthesizer(SynthesizerError),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SoundFont(error) => write!(f, "the soundfont failed ({error})"),
            Self::MidiFile(error) => write!(f, "the midi file failed ({error})"),
            Self::Synthesizer(error) => write!(f, "the synthesizer failed ({error})"),
        }
    }
}

impl std::error::Error for VoiceError {}

impl From<SoundFontError> for VoiceError {
    fn from(error: SoundFontError) -> Self {
        Self::SoundFont(error)
    }
}

impl From<MidiFileError> for VoiceError {
    fn from(error: MidiFileError) -> Self {
        Self::MidiFile(error)
    }
}

impl From<SynthesizerError> for VoiceError {
    fn from(error: SynthesizerError) -> Self {
        Self::Synthesizer(error)
    }
}

/// Renders every [`SynthVoice`] and [`MidiPlayer`] in the world into
/// interleaved frames.
#[derive(Clone, Debug)]
struct SynthMixer {
    /// The left channel's scratch, reused across fills.
    left: Vec<f32>,
    /// The right channel's scratch, reused across fills.
    right: Vec<f32>,
    /// The left channel's accumulator: every source overwrites the
    /// scratch, so the mix adds each render on top of it.
    mixed_left: Vec<f32>,
    /// The right channel's accumulator.
    mixed_right: Vec<f32>,
    /// The factor every rendered sample scales by.
    gain: f32,
}

impl SynthMixer {
    /// A mixer whose samples scale by `gain`.
    fn new(gain: f32) -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            mixed_left: Vec::new(),
            mixed_right: Vec::new(),
            gain,
        }
    }

    /// Adds the scratch's samples onto the accumulator.
    fn accumulate(&mut self) {
        for (mixed, sample) in self.mixed_left.iter_mut().zip(&self.left) {
            *mixed += sample;
        }
        for (mixed, sample) in self.mixed_right.iter_mut().zip(&self.right) {
            *mixed += sample;
        }
    }
}

impl PcmSource for SynthMixer {
    fn fill(&mut self, world: &World, out: &mut [f32], _sample_rate: u32, channels: u16) {
        let channels = usize::from(channels.max(1));
        let frames = out.len() / channels;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        self.mixed_left.clear();
        self.mixed_left.resize(frames, 0.0);
        self.mixed_right.clear();
        self.mixed_right.resize(frames, 0.0);
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.render(&mut self.left, &mut self.right);
            self.accumulate();
        }
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            player.sequencer.render(&mut self.left, &mut self.right);
            self.accumulate();
        }
        // Everything past the first two channels of a wider stream is
        // silent.
        for (frame, out) in out.chunks_mut(channels).enumerate() {
            for (channel, sample) in out.iter_mut().enumerate() {
                *sample = (match channel {
                    0 => self.mixed_left[frame],
                    _ => self.mixed_right[frame],
                } * self.gain)
                    .clamp(-1.0, 1.0);
            }
        }
    }
}

/// A job that switches every synthesizer's channel to another preset.
///
/// `program` is the General MIDI instrument number, zero-based: a music
/// box is 10, a vibraphone 12.
pub fn program_change_job(channel: i32, program: i32) -> Job<()> {
    Box::new(move |world| {
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice
                .synthesizer
                .process_midi_message(channel, 0xC0, program, 0);
        }
    })
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

/// A job that plays a MIDI file on every player in the world, looping or
/// stopping at the end.
pub fn play_midi_job(file: Arc<MidiFile>, loop_play: bool) -> Job<()> {
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            player.sequencer.play(&file, loop_play);
        }
    })
}

/// A job that stops every player's MIDI playback.
pub fn stop_midi_job() -> Job<()> {
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            player.sequencer.stop();
        }
    })
}

/// A job that stops the background audio dead: every sounding key is
/// released immediately, every player's sequence stops, and what plays
/// afterwards is only what someone starts again.
pub fn stop_audio_job() -> Job<()> {
    Box::new(move |world| {
        for (_, mut voice) in world.query::<&mut SynthVoice>() {
            voice.synthesizer.note_off_all(true);
        }
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            player.sequencer.stop();
        }
    })
}
