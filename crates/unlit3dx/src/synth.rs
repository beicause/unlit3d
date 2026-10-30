//! Soundfont-backed fill sources: one [`SynthVoice`] per soundfont, mixed.
//!
//! A voice lives in the audio world with the [`OnAudioFill`] that renders it,
//! so it sounds by existing. Its synthesizer waits for the first fill, which
//! reports the rate the stream really runs at: nothing has to know that rate
//! before the device opens.
//!
//! Gameplay never touches the synthesizer. A live note is a [`midi_job`]; a
//! sequence is a [`MidiSequencer`] in the gameplay world, advanced once a
//! frame, which sends the same jobs at the moments its events come due. The
//! audio thread only turns messages into samples and sums every source into
//! the stream's interleaved frames.

use std::sync::Arc;

pub use rustysynth_ext::{MidiFile, MidiFileLoopType, MidiMessage, SoundFont, Synthesizer};
use rustysynth_ext::{MidiFileError, SoundFontError, SynthesizerError, SynthesizerSettings};
use unlit_ecs::{Bundle, DynamicBundle, Entity, World, bundle};

use crate::audio::{AudioChunk, AudioFillOrder, OnAudioFill};
use crate::job::{Job, JobSender};

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
/// [`MidiSequencer::new`].
///
/// The file carries no loop markers: a loop repeats it from the start. Use
/// [`load_midi_file_with_loop_type`] when the file marks one the way a
/// particular sequencer wrote it.
///
/// # Errors
///
/// Returns the MIDI error when the bytes are not a valid SMF.
pub fn load_midi_file(bytes: &[u8]) -> Result<Arc<MidiFile>, VoiceError> {
    load_midi_file_with_loop_type(bytes, MidiFileLoopType::LoopPoint(0))
}

/// Parses a standard MIDI file from `bytes`, reading its loop markers the way
/// `loop_type` describes.
///
/// A file's loop markers are controller messages that only mean "loop here"
/// to the tool that wrote them, so the reader has to be told which convention
/// to look for. [`MidiFileLoopType::LoopPoint`] takes the byte position of
/// the loop instead, and `LoopPoint(0)` marks nothing.
///
/// # Errors
///
/// Returns the MIDI error when the bytes are not a valid SMF.
pub fn load_midi_file_with_loop_type(
    bytes: &[u8],
    loop_type: MidiFileLoopType,
) -> Result<Arc<MidiFile>, VoiceError> {
    let mut reader = bytes;
    Ok(Arc::new(MidiFile::new_with_loop_type(
        &mut reader,
        loop_type,
    )?))
}

/// One MIDI message and when to send it: seconds since the sequence starts,
/// then the message.
pub type MidiEvent = (f64, MidiEventKind);

/// A MIDI message the synthesizer acts on.
///
/// These are the five commands it implements; anything else a file may carry
/// (aftertouch, channel pressure, system messages) is not part of making a
/// sound and has no variant here.
///
/// The channel is always the low nibble of the status byte, so `0` is the
/// first channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MidiEventKind {
    /// Press a key. `velocity` is 1 to 127; zero releases the key instead.
    NoteOn {
        /// The channel the note plays on.
        channel: u8,
        /// The key, 0 to 127, where 60 is middle C.
        key: u8,
        /// How hard the key was struck, 0 to 127.
        velocity: u8,
    },
    /// Release a key.
    NoteOff {
        /// The channel the note plays on.
        channel: u8,
        /// The key, 0 to 127.
        key: u8,
    },
    /// Set a controller, such as the volume (7), the expression (11) or the
    /// sustain pedal (64).
    ControlChange {
        /// The channel the controller belongs to.
        channel: u8,
        /// The controller number, 0 to 127.
        controller: u8,
        /// The controller value, 0 to 127.
        value: u8,
    },
    /// Switch the channel to another instrument of the soundfont.
    ProgramChange {
        /// The channel that changes instrument.
        channel: u8,
        /// The General MIDI program number, zero-based.
        program: u8,
    },
    /// Bend the channel's pitch, from `-8192` (fully down) through `0` (no
    /// bend) to `8191` (fully up).
    PitchBend {
        /// The channel that bends.
        channel: u8,
        /// The bend, clamped to `-8192..=8191`.
        value: i16,
    },
}

impl MidiEventKind {
    /// The message this kind stands for.
    #[must_use]
    pub fn into_message(self) -> MidiMessage {
        match self {
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => MidiMessage::Normal {
                status: 0x90 | (channel & 0x0F),
                data1: key,
                data2: velocity,
            },
            Self::NoteOff { channel, key } => MidiMessage::Normal {
                status: 0x80 | (channel & 0x0F),
                data1: key,
                data2: 0,
            },
            Self::ControlChange {
                channel,
                controller,
                value,
            } => MidiMessage::Normal {
                status: 0xB0 | (channel & 0x0F),
                data1: controller,
                data2: value,
            },
            Self::ProgramChange { channel, program } => MidiMessage::Normal {
                status: 0xC0 | (channel & 0x0F),
                data1: program,
                data2: 0,
            },
            Self::PitchBend { channel, value } => {
                let raw = (i32::from(value) + 8192).clamp(0, 16_383);
                MidiMessage::Normal {
                    status: 0xE0 | (channel & 0x0F),
                    data1: (raw & 0x7F) as u8,
                    data2: ((raw >> 7) & 0x7F) as u8,
                }
            }
        }
    }

    /// The kind a message stands for, or `None` for a message the synthesizer
    /// does not act on.
    ///
    /// A note-on with velocity zero stays a note-on: the synthesizer releases
    /// the key itself, and rewriting the message would make the two spellings
    /// of one release indistinguishable.
    #[must_use]
    pub fn from_message(message: MidiMessage) -> Option<Self> {
        let MidiMessage::Normal {
            status,
            data1,
            data2,
        } = message
        else {
            return None;
        };
        let channel = status & 0x0F;
        Some(match status & 0xF0 {
            0x90 => Self::NoteOn {
                channel,
                key: data1,
                velocity: data2,
            },
            0x80 => Self::NoteOff {
                channel,
                key: data1,
            },
            0xB0 => Self::ControlChange {
                channel,
                controller: data1,
                value: data2,
            },
            0xC0 => Self::ProgramChange {
                channel,
                program: data1,
            },
            0xE0 => Self::PitchBend {
                channel,
                value: ((i16::from(data2) << 7) | i16::from(data1)) - 8192,
            },
            _ => return None,
        })
    }
}

/// Builds a MIDI file from timed messages, ready for [`MidiSequencer::new`].
///
/// The events may come in any order: they are sorted by time, and events at
/// the same time keep the order they were given in.
///
/// # Errors
///
/// Returns the MIDI error when a time is not a number.
pub fn midi_file(events: impl IntoIterator<Item = MidiEvent>) -> Result<Arc<MidiFile>, VoiceError> {
    let mut events: Vec<MidiEvent> = events.into_iter().collect();
    events.sort_by(|one, other| one.0.total_cmp(&other.0));
    Ok(Arc::new(MidiFile::new_with_events(
        events
            .into_iter()
            .map(|(time, kind)| (time, kind.into_message())),
    )?))
}

/// The synthesizer's block size, and how finely a fill places a message inside
/// itself: a note lands on a block boundary, so this is the timing resolution
/// below one fill.
const BLOCK_FRAMES: usize = 64;

/// The synthesizer `font` plays through at `rate`, set up the way a voice
/// wants it: full scale out of the box, because the library's default of 0.5
/// is a -6 dB cut that every caller would otherwise have to undo.
fn synthesizer(font: &Arc<SoundFont>, rate: i32) -> Result<Synthesizer, SynthesizerError> {
    let mut settings = SynthesizerSettings::new(rate);
    settings.block_size = BLOCK_FRAMES;
    let mut synthesizer = Synthesizer::new(font, &settings)?;
    synthesizer.set_master_volume(1.0);
    Ok(synthesizer)
}

/// A loaded soundfont and the synthesizer playing it, as one object.
///
/// Spawning it also spawns the [`OnAudioFill`] that renders it, so the voice is
/// a source of the world's mix: [`midi_job`] presses its keys and the audio
/// thread turns them into samples. A voice on its own would never be asked for
/// samples, and a fill source without one would have nothing to render, so the
/// two are spawned together.
pub struct SynthVoice {
    /// The soundfont to build the synthesizer from, until it is built. Taken
    /// away when building fails, which is what makes a voice that cannot sound
    /// fail once instead of on every fill.
    font: Option<Arc<SoundFont>>,
    /// The synthesizer that turns messages into samples, built on the first
    /// fill from the rate that fill reports.
    pub synthesizer: Option<Synthesizer>,
    /// Messages waiting for their moment, in seconds from the next fill's
    /// start.
    queued: Vec<(f64, MidiMessage)>,
    /// The left channel's block, reused across fills.
    left: Vec<f32>,
    /// The right channel's block, reused across fills.
    right: Vec<f32>,
    /// Whether building gave up, so no queue is kept for it any more.
    gave_up: bool,
}

impl Bundle for SynthVoice {
    fn into_bundle(self) -> DynamicBundle {
        bundle![
            self,
            OnAudioFill::new(AudioFillOrder::default(), fill_voice)
        ]
    }
}

impl SynthVoice {
    /// Spawns a voice playing `font`, and its fill source.
    pub fn from_font(world: &mut World, font: &Arc<SoundFont>) -> Entity {
        let voice = Self {
            font: Some(font.clone()),
            synthesizer: None,
            queued: Vec::new(),
            left: Vec::new(),
            right: Vec::new(),
            gave_up: false,
        };
        world.root_scope_mut().spawn(voice)
    }

    /// Parses `bytes` as a soundfont and spawns a voice playing it.
    ///
    /// # Errors
    ///
    /// Returns the soundfont error when the bytes are malformed.
    pub fn from_bytes(world: &mut World, bytes: &[u8]) -> Result<Entity, VoiceError> {
        Ok(Self::from_font(world, &load_soundfont(bytes)?))
    }

    /// Builds the synthesizer at `rate`, or gives the voice up for good.
    ///
    /// Returns whether the voice can render now.
    fn build(&mut self, rate: i32) -> bool {
        let Some(font) = self.font.clone() else {
            return false;
        };
        match synthesizer(&font, rate) {
            Ok(synthesizer) => {
                self.synthesizer = Some(synthesizer);
                true
            }
            Err(error) => {
                self.give_up(error);
                false
            }
        }
    }

    /// Stops trying to sound, naming `reason` once: a failure that would
    /// repeat on every fill belongs in the log once, not sixty times a second.
    fn give_up(&mut self, reason: impl std::fmt::Display) {
        if self.font.take().is_some() {
            log::error!("a synth voice cannot be built ({reason}); it stays silent");
        }
        self.synthesizer = None;
        self.queued.clear();
        self.gave_up = true;
    }
}

/// One sequence, its clock and where it loops.
///
/// The gameplay world owns it as a component and advances it once a frame, so
/// playback state is ordinary data in the world that asks for it: pausing is a
/// field. Nothing here touches the audio thread; [`MidiSequencer::advance`]
/// sends it [`midi_job`]s.
///
/// The voice a sequence plays into is shared with every other sequence in the
/// scene, so anything that cuts a pass short — a rewind, a pause, a loop back
/// to the marker — releases the notes this sequence pressed and leaves the
/// rest alone, instead of letting them ring into the next pass.
pub struct MidiSequencer {
    /// The sequence being played.
    pub file: Arc<MidiFile>,
    /// Seconds since the sequence started.
    pub time: f64,
    /// The index of the next event to send.
    pub index: usize,
    /// Where the sequence returns to once it reaches its end: seconds and event
    /// index, from the file's loop marker. `None` until a marker is reached,
    /// in which case the sequence repeats from its start.
    ///
    /// Written by hand, it loops a passage the file never marked; see
    /// [`MidiSequencer::advance`].
    pub loop_point: Option<(f64, usize)>,
    /// Whether reaching the end returns to the loop point instead of stopping.
    pub loop_play: bool,
    /// Whether the clock stands still.
    pub paused: bool,
    /// Whether the sequence reached its end and stayed there.
    pub finished: bool,
    /// The notes this sequence pressed and has not released, by channel and
    /// key. A shared voice is why the exact notes are remembered rather than
    /// silenced wholesale: an all-notes-off would cut another sequence's
    /// music too.
    held: Vec<(u8, u8)>,
    /// Whether the next advance has to release those notes before it plays
    /// anything, because a rewind cut a pass short since the last one.
    silent: bool,
}

impl MidiSequencer {
    /// Starts `file` from its beginning, playing once.
    pub fn new(file: Arc<MidiFile>) -> Self {
        Self {
            file,
            time: 0.0,
            index: 0,
            loop_point: None,
            loop_play: false,
            paused: false,
            finished: false,
            held: Vec::new(),
            silent: false,
        }
    }

    /// Puts the sequence back at its start, playing.
    ///
    /// `loop_point` is left alone, because a hand-written one has no marker to
    /// be restored from. The notes the pass left sounding are released on the
    /// next [`MidiSequencer::advance`]: a rewind has no sender to reach the
    /// voice with, and they must not ring into the new pass.
    pub fn rewind(&mut self) {
        self.time = 0.0;
        self.index = 0;
        self.finished = false;
        self.silent |= !self.held.is_empty();
    }

    /// Sends every event that came due in the last `delta` seconds to `voice`,
    /// and loops or stops when the sequence runs out.
    ///
    /// A message's `delay` keeps its distance from the start of the frame, so
    /// events due within one frame still reach the synthesizer in order and at
    /// their own offsets. The audio thread can only place them within the fill
    /// it renders, so an event lands closer to its moment than one frame but
    /// not exactly on it.
    ///
    /// The voice is shared with every other sequence in the scene, so a note
    /// held across the end of a pass would ring into the next one, and a note
    /// held when the clock stops would ring through the pause. Reaching the
    /// end, looping, pausing and rewinding all release the notes this sequence
    /// pressed, and leave the other sequences' notes sounding.
    pub fn advance(&mut self, delta: f64, voice: Entity, audio: &JobSender) {
        if self.paused || self.finished {
            // A pause leaves the notes of the pass sounding on the shared
            // voice, so the first frame that finds the clock stopped releases
            // them; a finished sequence has already done so.
            self.release(voice, audio);
            return;
        }
        if self.silent {
            self.release(voice, audio);
        }
        self.time += delta;
        let mut wraps = 0;
        while let Some((time, message)) = self.due() {
            match message {
                MidiMessage::Normal { .. } => {
                    if let Some(kind) = MidiEventKind::from_message(message) {
                        self.pressed(kind);
                        let delay = (time - (self.time - delta)).max(0.0);
                        let _ = audio.send(midi_job(voice, kind, delay));
                    }
                }
                MidiMessage::LoopStart => {
                    self.loop_point = Some((time, self.index));
                }
                MidiMessage::LoopEnd if self.loop_play => {
                    self.release(voice, audio);
                    let (time, index) = self.loop_point.unwrap_or((0.0, 0));
                    self.time = time;
                    self.index = index;
                    // A loop body of no length would keep coming due at the
                    // same instant; one wrap per frame is all a real file
                    // asks for.
                    wraps += 1;
                    if wraps > 1 {
                        break;
                    }
                }
                _ => {}
            }
            self.index += 1;
        }
        if self.index == self.file.get_messages().len() {
            self.release(voice, audio);
            if !self.loop_play {
                self.finished = true;
                return;
            }
            let (time, index) = self.loop_point.unwrap_or((0.0, 0));
            self.time = time;
            self.index = index;
        }
    }

    /// The event at the cursor, once the clock has reached it.
    ///
    /// Read through a borrow that ends with the call, so the advance is free
    /// to change the sequence between two events.
    fn due(&self) -> Option<(f64, MidiMessage)> {
        let times = self.file.get_times();
        let messages = self.file.get_messages();
        if self.index >= messages.len() || times[self.index] > self.time {
            return None;
        }
        Some((times[self.index], messages[self.index]))
    }

    /// Records what `kind` does to the notes this sequence holds sounding.
    fn pressed(&mut self, kind: MidiEventKind) {
        match kind {
            MidiEventKind::NoteOn {
                channel,
                key,
                velocity,
            } if velocity > 0 => {
                if !self.held.contains(&(channel, key)) {
                    self.held.push((channel, key));
                }
            }
            MidiEventKind::NoteOn { channel, key, .. }
            | MidiEventKind::NoteOff { channel, key } => {
                self.held.retain(|held| *held != (channel, key));
            }
            MidiEventKind::ControlChange {
                channel,
                controller: 120 | 123,
                ..
            } => self.held.retain(|held| held.0 != channel),
            _ => {}
        }
    }

    /// Releases the notes this sequence pressed, and only those: the voice is
    /// shared, and an all-notes-off would cut another sequence's music.
    fn release(&mut self, voice: Entity, audio: &JobSender) {
        self.silent = false;
        for (channel, key) in std::mem::take(&mut self.held) {
            let kind = MidiEventKind::NoteOff { channel, key };
            let _ = audio.send(midi_job(voice, kind, 0.0));
        }
    }
}

/// Why a soundfont or a MIDI file could not be built.
#[derive(Debug)]
pub enum VoiceError {
    /// The bytes are not a soundfont.
    SoundFont(SoundFontError),
    /// The bytes are not a MIDI file.
    MidiFile(MidiFileError),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SoundFont(error) => write!(f, "the soundfont failed ({error})"),
            Self::MidiFile(error) => write!(f, "the midi file failed ({error})"),
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

/// A job that sends `kind` to `voice`, `delay` seconds from the next fill's
/// start. A negative delay is treated as zero.
///
/// A voice that is gone ignores the job, and so does one that gave up on
/// sounding: nothing would ever drain its queue.
pub fn midi_job(voice: Entity, kind: MidiEventKind, delay: f64) -> Job {
    Box::new(move |world| {
        let Ok(mut guard) = world.root_scope().query_one::<&mut SynthVoice>(voice) else {
            return;
        };
        let voice = &mut *guard;
        // A queue a voice that gave up can never drain only grows.
        if voice.gave_up {
            return;
        }
        voice.queued.push((delay.max(0.0), kind.into_message()));
    })
}

/// Builds the voice's synthesizer if it has none, adds the queue's messages at
/// their offsets, and renders the whole fill into the chunk.
fn fill_voice(world: &World, entity: Entity, chunk: &mut AudioChunk) {
    let Ok(mut guard) = world.root_scope().query_one::<&mut SynthVoice>(entity) else {
        return;
    };
    // Borrowed through the guard the fields are not disjoint; reborrowed as a
    // reference they are, so the render can fill the voice's own block.
    let voice = &mut *guard;
    let Ok(rate) = i32::try_from(chunk.sample_rate) else {
        voice.give_up(format!("the stream's {} Hz", chunk.sample_rate));
        return;
    };
    let stale = voice
        .synthesizer
        .as_ref()
        .is_some_and(|synthesizer| synthesizer.get_sample_rate() != rate);
    if stale {
        // A synthesizer renders at one rate; a stream that changed rate needs
        // a new one.
        voice.synthesizer = None;
    }
    if voice.synthesizer.is_none() && !voice.build(rate) {
        return;
    }
    let frames = chunk.frames();
    let seconds_per_frame = 1.0 / f64::from(chunk.sample_rate);
    let SynthVoice {
        synthesizer,
        queued,
        left,
        right,
        ..
    } = voice;
    let Some(synthesizer) = synthesizer.as_mut() else {
        return;
    };
    left.resize(frames, 0.0);
    right.resize(frames, 0.0);
    // Taken out so the messages can be walked while the fields are borrowed to
    // render; put back with its allocation, whatever is left over.
    let mut pending = std::mem::take(queued);
    pending.sort_by(|one, other| one.0.total_cmp(&other.0));
    let mut wrote = 0;
    let mut applied = 0;
    for (delay, message) in &pending {
        let offset = (*delay / seconds_per_frame).round();
        if offset >= frames as f64 {
            break;
        }
        let offset = offset as usize;
        if offset > wrote {
            synthesizer.render(&mut left[wrote..offset], &mut right[wrote..offset]);
            wrote = offset;
        }
        if let MidiMessage::Normal {
            status,
            data1,
            data2,
        } = *message
        {
            synthesizer.process_midi_message(
                i32::from(status & 0x0F),
                i32::from(status & 0xF0),
                i32::from(data1),
                i32::from(data2),
            );
        }
        applied += 1;
    }
    synthesizer.render(&mut left[wrote..frames], &mut right[wrote..frames]);
    pending.drain(..applied);
    let elapsed = frames as f64 * seconds_per_frame;
    for (delay, _) in &mut pending {
        *delay = (*delay - elapsed).max(0.0);
    }
    *queued = pending;
    add_stereo(left, right, chunk);
}

/// Adds a stereo block onto the chunk's interleaved frames.
///
/// A mono stream gets the two channels averaged, so neither is lost; a stream
/// wider than stereo gets them in its first two channels and nothing in the
/// rest, which keeps whatever other sources added.
fn add_stereo(left: &[f32], right: &[f32], chunk: &mut AudioChunk) {
    let channels = usize::from(chunk.channels.max(1));
    for (frame, samples) in chunk.out.chunks_mut(channels).enumerate() {
        if channels == 1 {
            if let Some(sample) = samples.first_mut() {
                *sample += (left[frame] + right[frame]) * 0.5;
            }
        } else {
            if let Some(sample) = samples.first_mut() {
                *sample += left[frame];
            }
            if let Some(sample) = samples.get_mut(1) {
                *sample += right[frame];
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A voice with nothing built into it: enough to receive messages, which
    /// is all a sequence ever does with a voice.
    fn voice(world: &mut World) -> Entity {
        let voice = SynthVoice {
            font: None,
            synthesizer: None,
            queued: Vec::new(),
            left: Vec::new(),
            right: Vec::new(),
            gave_up: false,
        };
        world.root_scope_mut().spawn(bundle![voice])
    }

    /// The sending half of a command channel whose jobs this test drains by
    /// hand, so a sequence's sends can be counted with no audio thread.
    fn channel() -> (JobSender, async_channel::Receiver<Job>) {
        let (sender, receiver) = async_channel::unbounded();
        (JobSender::new(sender), receiver)
    }

    /// Runs the jobs queued so far, answering how many there were.
    fn drain(world: &mut World, receiver: &async_channel::Receiver<Job>) -> usize {
        let mut ran = 0;
        while let Ok(job) = receiver.try_recv() {
            job(world);
            ran += 1;
        }
        ran
    }

    /// The messages a voice has been handed, in arrival order.
    fn queued(world: &World, voice: Entity) -> Vec<(f64, MidiMessage)> {
        world
            .root_scope()
            .query_one::<&SynthVoice>(voice)
            .map(|mut voice| {
                let voice = voice.get();
                voice.queued.clone()
            })
            .unwrap_or_default()
    }

    /// The kinds a voice has been handed, in arrival order.
    fn kinds(world: &World, voice: Entity) -> Vec<MidiEventKind> {
        queued(world, voice)
            .into_iter()
            .filter_map(|(_, message)| MidiEventKind::from_message(message))
            .collect()
    }

    /// A file built straight from messages, so a loop marker can be written
    /// where `midi_file` cannot put one.
    fn file(events: impl IntoIterator<Item = (f64, MidiMessage)>) -> Arc<MidiFile> {
        Arc::new(MidiFile::new_with_events(events).expect("the events are well formed"))
    }

    /// A note on at `0.0`.
    fn note(key: u8) -> MidiMessage {
        MidiEventKind::NoteOn {
            channel: 0,
            key,
            velocity: 100,
        }
        .into_message()
    }

    /// A note on at `time`.
    fn note_at(time: f64, key: u8) -> MidiEvent {
        (
            time,
            MidiEventKind::NoteOn {
                channel: 0,
                key,
                velocity: 100,
            },
        )
    }

    /// Every kind a synthesizer understands survives the trip into a message
    /// and back, including the ends of the pitch bend's range.
    #[test]
    fn midi_event_kind_round_trips_through_midi_message() {
        let kinds = [
            MidiEventKind::NoteOn {
                channel: 3,
                key: 60,
                velocity: 100,
            },
            MidiEventKind::NoteOff {
                channel: 3,
                key: 60,
            },
            MidiEventKind::ControlChange {
                channel: 1,
                controller: 7,
                value: 90,
            },
            MidiEventKind::ProgramChange {
                channel: 2,
                program: 10,
            },
            MidiEventKind::PitchBend {
                channel: 0,
                value: 0,
            },
            MidiEventKind::PitchBend {
                channel: 0,
                value: -8192,
            },
            MidiEventKind::PitchBend {
                channel: 0,
                value: 8191,
            },
        ];
        for kind in kinds {
            assert_eq!(
                MidiEventKind::from_message(kind.into_message()),
                Some(kind),
                "{kind:?} did not survive the round trip"
            );
        }
    }

    /// A note on of no velocity is still a note on: the synthesizer is what
    /// turns it into a release, and this crate must not fold the two together
    /// on the way in.
    #[test]
    fn from_message_keeps_velocity_zero_as_note_on() {
        let kind = MidiEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0,
        };
        assert_eq!(MidiEventKind::from_message(kind.into_message()), Some(kind));
    }

    /// A message the kind cannot express is refused rather than guessed at.
    #[test]
    fn from_message_refuses_a_message_it_cannot_name() {
        let system = MidiMessage::Normal {
            status: 0xF0,
            data1: 0,
            data2: 0,
        };
        assert_eq!(MidiEventKind::from_message(system), None);
        assert_eq!(MidiEventKind::from_message(MidiMessage::EndOfTrack), None);
    }

    /// Each event is sent once, on the advance that makes it due, and the
    /// ones that follow wait for their own.
    #[test]
    fn a_sequencer_sends_each_event_once() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(
            midi_file([
                note_at(0.5, 60),
                (
                    1.0,
                    MidiEventKind::NoteOff {
                        channel: 0,
                        key: 60,
                    },
                ),
            ])
            .expect("the sequence is well formed"),
        );

        sequencer.advance(0.25, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 0, "nothing is due yet");
        sequencer.advance(0.5, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 1, "the first note came due");
        sequencer.advance(0.01, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            0,
            "an event that already came due is not sent again"
        );
        sequencer.advance(1.0, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            1,
            "the release came due, and the pass had nothing left sounding"
        );
        assert!(sequencer.finished, "a one-shot sequence finishes");
        assert_eq!(queued(&world, source).len(), 2);
    }

    /// A paused sequence stands still: no message, no clock movement.
    #[test]
    fn a_paused_sequencer_sends_nothing() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(midi_file([note_at(0.5, 60)]).expect("well formed"));
        sequencer.paused = true;
        sequencer.advance(10.0, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 0);
        assert_eq!(sequencer.time, 0.0);
    }

    /// Pausing releases the notes the pass left sounding, so a long note does
    /// not ring through the pause, and the release happens once.
    #[test]
    fn a_paused_sequencer_lets_its_notes_go() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        // A second note far enough away that the pass is still running when
        // the first one has sounded.
        let mut sequencer = MidiSequencer::new(
            midi_file([note_at(0.0, 60), note_at(10.0, 62)]).expect("well formed"),
        );
        sequencer.advance(0.5, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 1, "the note started");
        sequencer.paused = true;
        sequencer.advance(0.5, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 1, "the note was let go");
        assert_eq!(
            kinds(&world, source),
            vec![
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
                MidiEventKind::NoteOff {
                    channel: 0,
                    key: 60
                },
            ],
            "the note is released by name, not by a channel-wide all-notes-off"
        );
        sequencer.advance(0.5, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            0,
            "a silence is sent once, not on every frame of the pause"
        );
    }

    /// A looping file with no marker of its own repeats from its start, which
    /// is time zero, rather than from whenever its first event happens to be.
    #[test]
    fn a_sequencer_without_a_loop_marker_returns_to_zero() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer =
            MidiSequencer::new(midi_file([note_at(2.0, 60)]).expect("the sequence is well formed"));
        sequencer.loop_play = true;
        sequencer.advance(3.0, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            2,
            "the note and its release at the seam"
        );
        assert_eq!(sequencer.time, 0.0, "the pass is measured from zero");
        assert_eq!(sequencer.index, 0);
        assert!(!sequencer.finished, "a looping sequence never finishes");
    }

    /// A loop body that spans time comes back to its own start and stops
    /// there: one wrap per frame is all a file asks for.
    #[test]
    fn a_loop_body_across_zero_does_not_spin() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(file([
            (0.0, MidiMessage::LoopStart),
            (1.0, note(60)),
            (2.0, MidiMessage::LoopEnd),
            (3.0, note(62)),
        ]));
        sequencer.loop_play = true;
        sequencer.advance(5.0, source, &audio);
        assert_eq!(sequencer.time, 0.0, "the pass restarted at the marker");
        assert_eq!(
            drain(&mut world, &receiver),
            2,
            "the note and the reset at the seam"
        );
    }

    /// A loop body of no length would come due at the same instant forever;
    /// the frame's single wrap is what keeps the world responsive.
    #[test]
    fn a_zero_length_loop_body_does_not_spin() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(file([
            (0.0, MidiMessage::LoopStart),
            (0.0, MidiMessage::LoopEnd),
        ]));
        sequencer.loop_play = true;
        sequencer.advance(1.0, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            0,
            "one wrap with nothing sounding, then the frame ends"
        );
    }

    /// A sequence that played out stays put however far its clock is moved.
    #[test]
    fn a_finished_sequencer_stops_advancing() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer =
            MidiSequencer::new(midi_file([note_at(0.5, 60)]).expect("the sequence is well formed"));
        sequencer.advance(1.0, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            2,
            "the note and its release at the end"
        );
        sequencer.advance(10.0, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 0);
        assert!(sequencer.finished);
    }

    /// A looping sequence never finishes, however many passes it makes.
    #[test]
    fn a_looping_sequencer_never_finishes() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer =
            MidiSequencer::new(midi_file([note_at(0.5, 60)]).expect("the sequence is well formed"));
        sequencer.loop_play = true;
        for _ in 0..3 {
            sequencer.advance(1.0, source, &audio);
        }
        let _ = drain(&mut world, &receiver);
        assert!(!sequencer.finished);
    }

    /// A loop point written by hand is honoured the way a marker's would be,
    /// so a passage of a file that never marked one can repeat.
    #[test]
    fn a_hand_written_loop_point_is_honoured() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(
            midi_file([note_at(0.0, 60), note_at(1.0, 62), note_at(2.0, 64)])
                .expect("the sequence is well formed"),
        );
        sequencer.loop_play = true;
        sequencer.loop_point = Some((1.0, 1));
        sequencer.advance(5.0, source, &audio);
        let _ = drain(&mut world, &receiver);
        assert_eq!(
            sequencer.time, 1.0,
            "the pass restarted where it was told to"
        );
        assert_eq!(sequencer.index, 1);
    }

    /// A job reaches the voice it names and no other.
    #[test]
    fn midi_job_addresses_only_its_voice() {
        let mut world = World::new();
        let first = voice(&mut world);
        let second = voice(&mut world);
        let kind = MidiEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100,
        };
        midi_job(first, kind, 0.0)(&mut world);
        assert_eq!(queued(&world, first).len(), 1);
        assert!(
            queued(&world, second).is_empty(),
            "the other voice was addressed"
        );
        // A voice that is not there is a no-op rather than a panic.
        midi_job(Entity::from_raw(9_999, 0), kind, 0.0)(&mut world);
    }

    /// A voice that gave up on sounding keeps no queue: nothing would drain it.
    #[test]
    fn a_voice_that_gave_up_keeps_no_queue() {
        let mut world = World::new();
        let source = voice(&mut world);
        let kind = MidiEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100,
        };
        world
            .root_scope()
            .query_one::<&mut SynthVoice>(source)
            .expect("the voice was spawned")
            .get()
            .give_up("no soundfont");
        midi_job(source, kind, 0.0)(&mut world);
        assert!(queued(&world, source).is_empty(), "a dead voice queued");
    }

    /// A mono stream keeps both of a voice's channels by averaging them.
    #[test]
    fn a_mono_stream_keeps_both_channels() {
        let mut out = [0.0f32; 1];
        let mut chunk = AudioChunk {
            out: &mut out,
            sample_rate: 48_000,
            channels: 1,
        };
        add_stereo(&[0.5], &[0.25], &mut chunk);
        assert!((out[0] - 0.375).abs() < 1e-6, "a channel was dropped");
    }

    /// Rewinding returns to the start and leaves the loop point alone: it is
    /// not something a rewind has an opinion about.
    #[test]
    fn rewind_keeps_the_loop_point() {
        let mut sequencer = MidiSequencer::new(
            midi_file([note_at(0.0, 60), note_at(1.0, 62)]).expect("the sequence is well formed"),
        );
        sequencer.loop_play = true;
        sequencer.loop_point = Some((1.0, 1));
        sequencer.time = 0.5;
        sequencer.index = 1;
        sequencer.rewind();
        assert_eq!(sequencer.time, 0.0);
        assert_eq!(sequencer.index, 0);
        assert_eq!(sequencer.loop_point, Some((1.0, 1)));
        assert!(!sequencer.finished);
    }

    /// A rewind cuts the pass short, so the advance after it releases the notes
    /// the pass left sounding before it plays anything of the new one.
    #[test]
    fn a_rewind_lets_the_old_pass_go() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut sequencer = MidiSequencer::new(
            midi_file([note_at(0.0, 60), note_at(1.0, 62)]).expect("the sequence is well formed"),
        );
        sequencer.advance(0.5, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 1, "the first note started");
        sequencer.rewind();
        sequencer.advance(0.5, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            2,
            "the old note was let go, then the first note started again"
        );
    }

    /// Sequences in a scene play into one voice, so cutting one pass short
    /// must leave the notes of the other sounding.
    #[test]
    fn a_rewind_leaves_the_other_sequences_notes_alone() {
        let mut world = World::new();
        let source = voice(&mut world);
        let (audio, receiver) = channel();
        let mut first = MidiSequencer::new(
            midi_file([note_at(0.0, 60), note_at(10.0, 62)]).expect("well formed"),
        );
        let mut second = MidiSequencer::new(
            midi_file([note_at(0.0, 67), note_at(10.0, 69)]).expect("well formed"),
        );
        first.advance(0.5, source, &audio);
        second.advance(0.5, source, &audio);
        assert_eq!(drain(&mut world, &receiver), 2, "both notes started");
        first.rewind();
        first.advance(0.5, source, &audio);
        assert_eq!(
            drain(&mut world, &receiver),
            2,
            "the first note was let go, then the first note started again"
        );
        assert_eq!(
            kinds(&world, source),
            vec![
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 67,
                    velocity: 100,
                },
                MidiEventKind::NoteOff {
                    channel: 0,
                    key: 60
                },
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
            ],
            "only the rewound sequence's note was released"
        );
        assert_eq!(
            second.held,
            vec![(0, 67)],
            "the other sequence still holds its note"
        );
    }
}
