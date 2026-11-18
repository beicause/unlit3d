//! The demo's two pieces of music, written out as MIDI events.
//!
//! The demo carries no MIDI asset: both tracks are short enough to state,
//! and stating them keeps the notes in the source that shapes them.
//!
//! Both tracks play into the demo's one synthesizer voice, so they carve
//! up its sixteen MIDI channels between them: the background loop takes
//! three, and the sequence takes the one after those. A channel sounds
//! with whatever instrument the last ProgramChange on it named, so two
//! files playing on one channel would overwrite each other's instruments.

use std::sync::Arc;

use unlit3dx::prelude::{MidiEvent, MidiEventKind, midi_file};
use unlit3dx::synth::MidiFile;

/// The General MIDI music box, as a zero-based program number: a bell
/// like plucked timbre that stays smooth where a piano turns brash.
pub const MUSIC_BOX_PROGRAM: u8 = 10;

/// The General MIDI electric piano, as a zero-based program number: soft
/// enough to sit under the sequence without competing with it.
pub const ELECTRIC_PIANO_PROGRAM: u8 = 4;

/// The channel the background loop's bass plays on.
const BGM_BASS_CHANNEL: u8 = 0;

/// The channel the background loop's arpeggio plays on.
const BGM_ARPEGGIO_CHANNEL: u8 = 1;

/// The channel the background loop's melody plays on.
const BGM_MELODY_CHANNEL: u8 = 2;

/// The channel the demo sequence plays on, the one after the background
/// loop's three: see the module docs for why the two files must not
/// share a channel.
const SEQUENCE_CHANNEL: u8 = 3;

/// The pentatonic scale the sequence walks, as MIDI keys.
///
/// The octave sits low on purpose: the soundfont's higher keys turn
/// brittle, and a lower register stays mellow.
pub const SCALE: [i32; 10] = [48, 50, 52, 55, 57, 60, 62, 64, 67, 69];

/// Push one note onto an event list: a press at `start`, a release
/// `hold` seconds later, both on `channel`.
fn note(events: &mut Vec<MidiEvent>, channel: u8, key: i32, start: f64, hold: f64, velocity: u8) {
    events.push((
        start,
        MidiEventKind::NoteOn {
            channel,
            key: key as u8,
            velocity,
        },
    ));
    events.push((
        start + hold,
        MidiEventKind::NoteOff {
            channel,
            key: key as u8,
        },
    ));
}

/// The demo's MIDI sequence: the scale walked up and back down, one note
/// every 0.28 seconds, each held for 0.25 of them, on the music box, on
/// its own channel so its ProgramChange cannot reach the background
/// loop's instruments.
pub fn sequence_midi() -> Arc<MidiFile> {
    let mut events = vec![(
        0.0,
        MidiEventKind::ProgramChange {
            channel: SEQUENCE_CHANNEL,
            program: MUSIC_BOX_PROGRAM,
        },
    )];
    for (index, &key) in SCALE.iter().chain(SCALE.iter().rev().skip(1)).enumerate() {
        let start = index as f64 * 0.28;
        note(&mut events, SEQUENCE_CHANNEL, key, start, 0.25, 96);
    }
    midi_file(events).expect("the demo sequence is well formed")
}

/// The bars of the background loop, as chords: three keys each, root
/// first. Four bars of a I-vi-IV-V turnround, twice, so the loop breathes
/// without changing key.
const BGM_CHORDS: [[i32; 3]; 8] = [
    [48, 55, 60],
    [45, 52, 57],
    [41, 53, 57],
    [43, 50, 59],
    [48, 55, 60],
    [45, 52, 57],
    [41, 53, 60],
    [43, 55, 62],
];

/// The melody over the loop, as scale degrees: an index into [`SCALE`],
/// or `None` for a rest. One entry per half bar.
const BGM_MELODY: [Option<usize>; 16] = [
    Some(5),
    None,
    Some(4),
    Some(3),
    Some(2),
    None,
    Some(3),
    Some(4),
    Some(5),
    None,
    Some(7),
    Some(6),
    Some(4),
    None,
    Some(2),
    Some(3),
];

/// How long one bar of the background loop lasts, in seconds.
const BGM_BAR_SECONDS: f64 = 2.0;

/// Which chord key sounds on each of a bar's eight eighths: up the chord
/// and back down again, so the accompaniment moves with the balls.
const BGM_ARPEGGIO: [usize; 8] = [0, 1, 2, 1, 0, 1, 2, 1];

/// The looping background music: a bass note per bar, the chord's keys
/// arpeggiated across its eight eighths, and a slow melody on top — the
/// bass and arpeggio electric piano, the melody the music box. Written
/// out as events rather than read from a file: the demo carries no MIDI
/// asset, and the loop is short enough to state.
pub fn bgm_midi() -> Arc<MidiFile> {
    let eighth = BGM_BAR_SECONDS / 8.0;
    // Three channels, one instrument each: the bass, the arpeggio and the
    // melody, so the loop reads as three parts instead of one chord wall.
    let mut events = vec![
        (
            0.0,
            MidiEventKind::ProgramChange {
                channel: BGM_BASS_CHANNEL,
                program: ELECTRIC_PIANO_PROGRAM,
            },
        ),
        (
            0.0,
            MidiEventKind::ProgramChange {
                channel: BGM_ARPEGGIO_CHANNEL,
                program: ELECTRIC_PIANO_PROGRAM,
            },
        ),
        (
            0.0,
            MidiEventKind::ProgramChange {
                channel: BGM_MELODY_CHANNEL,
                program: MUSIC_BOX_PROGRAM,
            },
        ),
    ];
    for (bar, chord) in BGM_CHORDS.iter().enumerate() {
        let start = bar as f64 * BGM_BAR_SECONDS;
        // The bass holds the bar: an octave below its root.
        note(
            &mut events,
            BGM_BASS_CHANNEL,
            chord[0] - 12,
            start,
            BGM_BAR_SECONDS,
            56,
        );
        // The chord walks up and back over the bar's eighths, so the
        // accompaniment moves without ever leaving the harmony.
        for (step, &finger) in BGM_ARPEGGIO.iter().enumerate() {
            let key = chord[finger] + 12;
            note(
                &mut events,
                BGM_ARPEGGIO_CHANNEL,
                key,
                start + step as f64 * eighth,
                eighth,
                44,
            );
        }
        // The melody: one note per half bar, resting where the chart says.
        for half in 0..2 {
            let Some(degree) = BGM_MELODY[bar * 2 + half] else {
                continue;
            };
            note(
                &mut events,
                BGM_MELODY_CHANNEL,
                SCALE[degree] + 12,
                start + half as f64 * (BGM_BAR_SECONDS / 2.0),
                BGM_BAR_SECONDS / 2.0 * 0.9,
                72,
            );
        }
    }
    midi_file(events).expect("the background loop is well formed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlit3dx::synth::MidiMessage;

    /// Count a file's key presses and releases: every sounding key must be
    /// let go again, or a looping track leaves a chord hanging forever.
    fn presses_and_releases(file: &MidiFile) -> (usize, usize) {
        let mut presses = 0;
        let mut releases = 0;
        for message in file.get_messages() {
            let MidiMessage::Normal { status, .. } = message else {
                continue;
            };
            match status & 0xF0 {
                0x90 => presses += 1,
                0x80 => releases += 1,
                _ => {}
            }
        }
        (presses, releases)
    }

    /// The demo's two tracks are well formed: sorted times, and every key
    /// press released again, so the loop's seam stays clean.
    #[test]
    fn the_midi_tracks_release_every_note() {
        for file in [bgm_midi(), sequence_midi()] {
            let (presses, releases) = presses_and_releases(&file);
            assert!(presses > 0, "a track with no notes is not music");
            assert_eq!(
                presses,
                releases,
                "the track leaves {} keys hanging",
                presses - releases
            );
            let times = file.get_times();
            for pair in times.windows(2) {
                assert!(
                    pair[0] <= pair[1],
                    "the event times run backwards at {}",
                    pair[1]
                );
            }
        }
    }

    /// The background loop is whole bars long: its length is a multiple of
    /// the bar, so a loop point lands on the downbeat rather than mid-bar.
    #[test]
    fn the_background_loop_spans_whole_bars() {
        let file = bgm_midi();
        let bars = file.get_length() / BGM_BAR_SECONDS;
        assert!(
            (bars - bars.round()).abs() < f64::EPSILON,
            "the loop is {bars} bars long"
        );
        assert!(bars >= 4.0, "a four-bar loop is the shortest that breathes");
    }

    /// The two tracks carve up the voice's sixteen channels: a channel
    /// sounds with the instrument its last ProgramChange named, and the
    /// sequencer releases a note by (channel, key), so two files on one
    /// channel overwrite each other's instruments and cut each other's notes.
    #[test]
    fn the_tracks_play_on_disjoint_channels() {
        // The channels a file plays on, read off its channel voice messages:
        // each carries its channel in the status byte's low four bits. Every
        // such message counts -- a note on or off, a controller, a program
        // change -- because any of them lands on the voice the channel drives.
        let channels = |file: &MidiFile| {
            let mut set = Vec::new();
            for message in file.get_messages() {
                let MidiMessage::Normal { status, .. } = message else {
                    continue;
                };
                if matches!(status & 0xF0, 0x80 | 0x90 | 0xA0 | 0xB0 | 0xC0 | 0xE0) {
                    set.push(status & 0x0F);
                }
            }
            set.sort_unstable();
            set.dedup();
            set
        };
        let bgm = channels(&bgm_midi());
        let sequence = channels(&sequence_midi());
        assert!(
            !bgm.is_empty() && !sequence.is_empty(),
            "a track sets no instrument"
        );
        for channel in &sequence {
            assert!(
                !bgm.contains(channel),
                "both tracks play on channel {channel}"
            );
        }
    }
}
