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
    let mut sequencer =
        MidiSequencer::new(midi_file([note_at(0.0, 60), note_at(10.0, 62)]).expect("well formed"));
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
    let mut first =
        MidiSequencer::new(midi_file([note_at(0.0, 60), note_at(10.0, 62)]).expect("well formed"));
    let mut second =
        MidiSequencer::new(midi_file([note_at(0.0, 67), note_at(10.0, 69)]).expect("well formed"));
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
