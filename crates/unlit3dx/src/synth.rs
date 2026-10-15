//! Soundfont-backed fill sources: synthesizers and MIDI sequencers,
//! mixed.
//!
//! The soundfont loads on the audio thread, where the [`SynthVoice`] and
//! one [`MidiPlayer`] per named track it feeds are spawned as components
//! of the audio world — each with the [`OnFill`] that renders it, so a
//! source plays by existing and a world with no sources plays nothing.
//! Installing them is ordinary work for that world: queue a job once the
//! output's format ([`AudioFormat`](crate::audio::AudioFormat)) has landed.
//! Gameplay never touches the
//! synthesizer: it
//! sends note jobs for live sound and transport jobs — play, pause,
//! rewind, stop — addressed to a track by name, and the audio thread
//! sums every source into the stream's interleaved frames.
//!
//! Tracks are independent: pausing one leaves the others sounding, and a
//! paused track renders nothing at all, so its sequencer clock stops and
//! the resume picks up exactly where it left off.

use std::sync::Arc;

pub use rustysynth_ext::{MidiFile, MidiMessage};
use rustysynth_ext::{
    MidiFileError, SoundFont, SoundFontError, Synthesizer, SynthesizerError, SynthesizerSettings,
};
use unlit_ecs::{Entity, World};

use crate::audio::{AudioChunk, FillOrder, OnFill, WorldFillExt};
use crate::job::Job;

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
/// [`play_track_job`].
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
/// [`play_track_job`].
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

/// How many frames the track's clock steps at a time, and the synthesizer's
/// block size: the two are one number because a block is exactly what the
/// clock advances by. Written into [`SynthesizerSettings`] rather than
/// assumed, so the walk and the render cannot drift apart.
const TRACK_CLOCK_FRAMES: usize = 64;

/// The synthesizer `font` plays through, and the sample rate it was built
/// at, in hertz: a caller that clocks its own events needs the rate the
/// synthesizer actually renders at, not the one it asked for.
///
/// Shared by [`SynthVoice`] and [`MidiPlayer`], which each own one.
fn synthesizer(font: &Arc<SoundFont>, sample_rate: u32) -> Result<(Synthesizer, u32), VoiceError> {
    let rate = i32::try_from(sample_rate).unwrap_or(48_000);
    let mut settings = SynthesizerSettings::new(rate);
    settings.block_size = TRACK_CLOCK_FRAMES;
    let synthesizer = Synthesizer::new(font, &settings)?;
    Ok((synthesizer, u32::try_from(rate).unwrap_or(48_000)))
}

/// A loaded soundfont and the synthesizer playing it, as one component.
///
/// Spawned with the [`OnFill`] that renders it, so the voice is a source
/// of the world's mix: note jobs press its keys and the audio thread
/// turns them into samples.
pub struct SynthVoice {
    /// The soundfont the synthesizer reads its instruments from.
    _font: Arc<SoundFont>,
    /// The synthesizer that turns note events into samples.
    pub synthesizer: Synthesizer,
    /// The left channel's block, reused across fills.
    left: Vec<f32>,
    /// The right channel's block, reused across fills.
    right: Vec<f32>,
}

impl SynthVoice {
    /// Spawns a voice playing `font`, and its fill source.
    ///
    /// # Errors
    ///
    /// Returns the synthesizer error when its settings are out of range.
    pub fn from_font(
        world: &mut World,
        font: &Arc<SoundFont>,
        sample_rate: u32,
    ) -> Result<Entity, VoiceError> {
        let (synthesizer, _rate) = synthesizer(font, sample_rate)?;
        let voice = Self {
            _font: font.clone(),
            synthesizer,
            left: Vec::new(),
            right: Vec::new(),
        };
        let fill = world.mount_on_fill(OnFill::new(FillOrder::default(), fill_voice));
        Ok(world.spawn((voice, fill)))
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
}

/// One MIDI track and its own synthesizer, as one component.
///
/// The player walks the sequence's own timed events — a list of
/// `(`seconds`, message)` pairs — and keeps the clock that walk needs:
/// `current_time` advances by exactly the samples each fill renders, so
/// playback stays sample-accurate no matter how the host paces its ticks.
/// Each player carries a name so transport jobs can address it
/// independently: a background loop and a one-shot sequence coexist on
/// the same audio world, each with its own [`OnFill`] source.
pub struct MidiPlayer {
    /// The name transport jobs address this track by.
    name: String,
    /// The synthesizer the track's messages drive.
    synthesizer: Synthesizer,
    /// The sample rate that synthesizer renders at, in hertz: the clock
    /// converts rendered samples back into seconds through it.
    sample_rate: u32,
    /// The file the track plays and whether it loops, kept so a rewind
    /// restarts the same sequence without its sender holding onto it.
    track: Option<(Arc<MidiFile>, bool)>,
    /// Where the track has read to, in seconds since its start.
    current_time: f64,
    /// The index of the next event to apply.
    message_index: usize,
    /// Where a loop restarts: the file's loop marker, or its start.
    loop_index: usize,
    /// Whether the track is paused; a paused track renders silence and
    /// its clock stands still, so the resume picks up exactly where the
    /// pause left off.
    paused: bool,
    /// How many one-shot plays this track has walked to their end, as
    /// [`MidiPlayer::ends`] reports them.
    ends: u64,
    /// Whether the play now under way has been counted in `ends`, so a
    /// track that finished is counted once rather than on every fill
    /// after.
    finished: bool,
    /// The left channel's block, reused across fills.
    left: Vec<f32>,
    /// The right channel's block, reused across fills.
    right: Vec<f32>,
}

impl MidiPlayer {
    /// The name transport jobs address this track by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How many one-shot plays this track has walked to their end.
    ///
    /// A looping play never counts: it has no end to reach. A reader that
    /// remembers the count it last saw sees every end, even when it polled
    /// nothing in between.
    #[must_use]
    pub fn ends(&self) -> u64 {
        self.ends
    }

    /// Spawns a player named `name` reading `font`, and its fill source.
    ///
    /// # Errors
    ///
    /// Returns the synthesizer error when its settings are out of range.
    pub fn from_font(
        world: &mut World,
        font: &Arc<SoundFont>,
        sample_rate: u32,
        name: &str,
    ) -> Result<Entity, VoiceError> {
        let (synthesizer, rate) = synthesizer(font, sample_rate)?;
        let player = Self {
            name: name.to_owned(),
            synthesizer,
            sample_rate: rate,
            track: None,
            current_time: 0.0,
            message_index: 0,
            loop_index: 0,
            paused: false,
            ends: 0,
            finished: false,
            left: Vec::new(),
            right: Vec::new(),
        };
        let fill = world.mount_on_fill(OnFill::new(FillOrder::default(), fill_track));
        Ok(world.spawn((player, fill)))
    }

    /// Puts the track back at its start: the clock, the event cursor and
    /// the synthesizer's sounding voices all reset, so a fresh sequence
    /// begins in silence rather than under whatever was held. A fresh
    /// play counts its own end again, whatever the last one did.
    fn start(&mut self, file: Arc<MidiFile>, loop_play: bool) {
        self.track = Some((file, loop_play));
        self.current_time = 0.0;
        self.message_index = 0;
        self.loop_index = 0;
        self.finished = false;
        self.synthesizer.reset();
    }

    /// Renders `frames` of the track into its own block, applying every
    /// event whose time the clock has passed along the way.
    ///
    /// The clock walks in [`TRACK_CLOCK_FRAMES`] steps, so an event lands
    /// within that granularity of the sample it belongs to rather than at
    /// the start of a whole fill chunk.
    fn advance(&mut self, frames: usize) {
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        let track = self.track.clone();
        let mut wrote = 0;
        while wrote < frames {
            let step = TRACK_CLOCK_FRAMES.min(frames - wrote);
            if let Some((file, loop_play)) = &track {
                self.process_events(file, *loop_play);
            }
            self.synthesizer.render(
                &mut self.left[wrote..wrote + step],
                &mut self.right[wrote..wrote + step],
            );
            self.current_time += step as f64 / f64::from(self.sample_rate);
            wrote += step;
        }
    }

    /// Puts the track down: no sequence, the clock back at zero, and every
    /// sounding voice released, so a stop leaves silence rather than tails.
    fn stop(&mut self) {
        self.track = None;
        self.paused = false;
        self.current_time = 0.0;
        self.message_index = 0;
        self.loop_index = 0;
        self.finished = false;
        self.synthesizer.reset();
    }

    /// Applies the events up to the clock, and loops the track when it runs
    /// out with looping on.
    ///
    /// A loop restarts at the file's `LoopStart` marker when it carries one
    /// and at its first event otherwise; the held voices are released
    /// first, so a restart is a seam and not a chord of everything the
    /// pass left sounding.
    fn process_events(&mut self, file: &MidiFile, loop_play: bool) {
        let (messages, times) = (file.get_messages(), file.get_times());
        while self.message_index < messages.len() {
            let time = times[self.message_index];
            if time > self.current_time {
                break;
            }
            match messages[self.message_index] {
                MidiMessage::Normal {
                    status,
                    data1,
                    data2,
                } => self.synthesizer.process_midi_message(
                    i32::from(status & 0x0F),
                    i32::from(status & 0xF0),
                    i32::from(data1),
                    i32::from(data2),
                ),
                MidiMessage::LoopStart if loop_play => self.loop_index = self.message_index,
                MidiMessage::LoopEnd if loop_play => {
                    self.restart_at(times[self.loop_index], self.loop_index);
                }
                _ => {}
            }
            self.message_index += 1;
        }
        let exhausted = self.message_index == messages.len();
        if loop_play && exhausted {
            let (time, index) = (times[self.loop_index], self.loop_index);
            self.restart_at(time, index);
        } else if exhausted && !self.finished {
            // A one-shot play that walked its whole file: counted once, so
            // a reader polling `ends` can tell that it ended.
            self.finished = true;
            self.ends += 1;
        }
    }

    /// Jump the clock and the event cursor back to a loop point.
    fn restart_at(&mut self, time: f64, index: usize) {
        self.current_time = time;
        self.message_index = index;
        self.synthesizer.note_off_all(false);
    }

    /// Parses `bytes` as a soundfont and spawns a player named `name`
    /// reading it.
    ///
    /// # Errors
    ///
    /// Returns the soundfont error when the bytes are malformed, and the
    /// synthesizer error when its settings are out of range.
    pub fn from_bytes(
        world: &mut World,
        bytes: &[u8],
        sample_rate: u32,
        name: &str,
    ) -> Result<Entity, VoiceError> {
        Self::from_font(world, &load_soundfont(bytes)?, sample_rate, name)
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

/// Adds one voice's block to the chunk it was asked to fill.
fn fill_voice(world: &World, entity: Entity, chunk: &mut AudioChunk) {
    let Some(mut guard) = world.get_mut::<SynthVoice>(entity) else {
        return;
    };
    // Borrowed through the guard the fields are not disjoint; reborrowed as a
    // reference they are, so the render can fill the voice's own block.
    let voice = &mut *guard;
    let frames = chunk.frames();
    voice.left.resize(frames, 0.0);
    voice.right.resize(frames, 0.0);
    voice.synthesizer.render(&mut voice.left, &mut voice.right);
    add_stereo(&voice.left, &voice.right, chunk);
}

/// Adds one track's block to the chunk, unless the track is paused.
fn fill_track(world: &World, entity: Entity, chunk: &mut AudioChunk) {
    let Some(mut guard) = world.get_mut::<MidiPlayer>(entity) else {
        return;
    };
    // Reborrowed, as in [`fill_voice`], so the block and the clock are
    // separate borrows.
    let player = &mut *guard;
    // A paused track renders nothing at all: its clock stands still, so
    // the resume continues where the pause stopped.
    if player.paused {
        return;
    }
    let frames = chunk.frames();
    player.advance(frames);
    add_stereo(&player.left, &player.right, chunk);
}

/// Adds a stereo block onto the chunk's interleaved frames.
///
/// A stream wider than stereo gets the voice's two channels in its first
/// two and nothing in the rest, which keeps whatever other sources added.
fn add_stereo(left: &[f32], right: &[f32], chunk: &mut AudioChunk) {
    let channels = usize::from(chunk.channels.max(1));
    for (frame, samples) in chunk.out.chunks_mut(channels).enumerate() {
        if let Some(sample) = samples.first_mut() {
            *sample += left[frame];
        }
        if let Some(sample) = samples.get_mut(1) {
            *sample += right[frame];
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

/// A job that plays a MIDI file on the track named `name`, looping or
/// stopping at the end. Playing restarts the track's position at zero and
/// resumes it if it was paused.
pub fn play_track_job(name: &str, file: Arc<MidiFile>, loop_play: bool) -> Job<()> {
    let name = name.to_owned();
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            if player.name != name {
                continue;
            }
            player.paused = false;
            player.start(file.clone(), loop_play);
        }
    })
}

/// A job that pauses or resumes the track named `name`. A paused track
/// renders nothing and its sequencer clock stands still, so the resume
/// continues where the pause stopped rather than skipping ahead.
pub fn pause_track_job(name: &str, paused: bool) -> Job<()> {
    let name = name.to_owned();
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            if player.name == name {
                player.paused = paused;
            }
        }
    })
}

/// A job that returns the track named `name` to its start, keeping its
/// loop setting and its pause. A track with nothing loaded ignores it.
pub fn rewind_track_job(name: &str) -> Job<()> {
    let name = name.to_owned();
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            if player.name != name {
                continue;
            }
            let Some((file, loop_play)) = player.track.clone() else {
                continue;
            };
            player.start(file, loop_play);
        }
    })
}

/// A job that stops the track named `name`: its sequence is put down and
/// its sounding keys released, so only what someone starts again follows.
pub fn stop_track_job(name: &str) -> Job<()> {
    let name = name.to_owned();
    Box::new(move |world| {
        for (_, mut player) in world.query::<&mut MidiPlayer>() {
            if player.name == name {
                player.stop();
            }
        }
    })
}
