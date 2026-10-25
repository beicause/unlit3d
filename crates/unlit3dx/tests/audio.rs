//! Integration tests for the audio threaded world: the fill-source contract,
//! and the world's job channel with and without an output device.

// The tests that need real samples read the workspace's own soundfont, which
// is not in git: a checkout without it has to say so out loud rather than
// report a green run that exercised nothing.
#![expect(
    clippy::print_stderr,
    reason = "naming the reason a test skipped is part of the test's output"
)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use unlit_ecs::{Entity, World, bundle};
use unlit3dx::prelude::*;

/// Waits until `probe` answers `true`, giving up after five seconds.
fn wait_for(probe: impl Fn() -> bool) -> bool {
    for _ in 0..5_000 {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// Install the mix's gain and the synthesizer voice on a world, answering
/// the entity that carries the voice when the soundfont parsed.
///
/// A caller queues this as a job to the audio world once it has a soundfont;
/// these tests drive worlds themselves, so they call it directly.
fn install_synth(world: &mut World, bytes: &[u8], gain: f32) -> Option<Entity> {
    world.root_scope_mut().spawn(bundle![GlobalAudioGain(gain)]);
    load_soundfont(bytes)
        .ok()
        .map(|font| SynthVoice::from_font(world, &font))
}

/// Spawn a source that counts the fills it takes and adds `level` to every
/// sample, the way a synthesizer adds its block to the chunk.
fn counting_source(world: &mut World, level: f32, counts: &Arc<AtomicUsize>) {
    let counted = Arc::clone(counts);
    world.root_scope_mut().spawn(bundle![OnAudioFill::new(
        AudioFillOrder::default(),
        move |_world, _entity, chunk| {
            counted.fetch_add(1, Ordering::SeqCst);
            for sample in chunk.out.iter_mut() {
                *sample += level;
            }
        },
    ),]);
}

/// A world with no sources fills with silence; a source adds to the chunk
/// the dispatcher hands it, and several sources mix.
#[test]
fn the_dispatcher_mixes_the_worlds_sources() {
    let mut out = vec![1.0f32; 128];
    dispatch_audio_fill(&World::new(), &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.0), "nothing to play");

    let mut world = World::new();
    let counts = Arc::new(AtomicUsize::new(0));
    counting_source(&mut world, 0.25, &counts);
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.25));
    assert_eq!(counts.load(Ordering::SeqCst), 1);

    counting_source(&mut world, 0.5, &counts);
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(
        out.iter().all(|sample| (sample - 0.75).abs() < 1e-6),
        "both sources must add up"
    );
}

/// The world's gain scales the mix once, after the sources summed, and an
/// overdriven mix clamps instead of wrapping.
#[test]
fn the_worlds_gain_scales_the_mix() {
    let mut world = World::new();
    world.root_scope_mut().spawn(bundle![GlobalAudioGain(2.0)]);
    let counts = Arc::new(AtomicUsize::new(0));
    counting_source(&mut world, 0.4, &counts);
    let mut out = vec![0.0f32; 8];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| (sample - 0.8).abs() < 1e-6));

    // A second source pushes the sum past full scale: the clamp lands on
    // the unit range, not on a wrapped sample.
    counting_source(&mut world, 0.4, &counts);
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 1.0));
}

/// A stated order runs after the default band, whatever order the two were
/// spawned in; nothing is asserted about the order within the band, which the
/// dispatcher promises none about.
#[test]
fn a_later_order_runs_after_the_default_band() {
    let mut world = World::new();
    let ran: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let late = Arc::clone(&ran);
    world.root_scope_mut().spawn(bundle![OnAudioFill::new(
        AudioFillOrder(1),
        move |_world, _entity, _chunk| {
            late.lock().unwrap().push("late");
        },
    ),]);
    for (name, level) in [("first", 0.1), ("second", 0.2)] {
        let ran = Arc::clone(&ran);
        world.root_scope_mut().spawn(bundle![OnAudioFill::new(
            AudioFillOrder::default(),
            move |_world, _entity, chunk| {
                ran.lock().unwrap().push(name);
                for sample in chunk.out.iter_mut() {
                    *sample += level;
                }
            },
        ),]);
    }
    let mut out = vec![0.0f32; 2];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    let ran = ran.lock().unwrap();
    assert_eq!(ran.len(), 3);
    assert_eq!(ran[2], "late", "the stated order runs last");
    assert!(ran[..2].contains(&"first") && ran[..2].contains(&"second"));
}

/// The audio world runs its jobs and, when the machine has an output
/// device, drives its source once per filled chunk.
#[test]
fn the_audio_world_runs_jobs_and_drives_its_source() {
    let mut host = World::new();
    let fills = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fills);
    let world = spawn_audio_world_with::<Threaded>(&mut host, AudioConfig::default());
    // The source arrives the way a caller's would: an ordinary job. Jobs
    // are answered whether or not a device exists, so a device-less world
    // runs silently but stays alive.
    world
        .sender
        .send(Box::new(move |world: &mut World| {
            counting_source(world, 0.25, &counted);
        }))
        .expect("the audio world takes its source");
    // A marker job and a probe that waits for it.
    let _ = world.sender.send(Box::new(|world: &mut World| {
        world.root_scope_mut().spawn(bundle![Marker]);
    }));
    let marked = Snapshot::new();
    let reader = marked.reader();
    let publish = marked.clone();
    world
        .sender
        .send(Box::new(move |world: &mut World| {
            publish.publish(world.root_scope().query::<&Marker>().next().is_some());
        }))
        .expect("the audio world accepts jobs");
    assert!(
        wait_for(|| reader.latest().is_some_and(|found| *found)),
        "the audio world never ran the marker job"
    );

    // With a device present the setup must have run and the source must
    // be filling; without one the world stays alive and silent instead.
    if has_output_device() {
        assert!(
            wait_for(|| fills.load(Ordering::SeqCst) > 0),
            "the output device exists but the source was never filled"
        );
    } else {
        assert_eq!(fills.load(Ordering::SeqCst), 0);
        assert!(!world.is_finished(), "the device-less world died");
    }
}

/// The audio world announces when its output is open, and says nothing
/// until it is: a subscriber's collector sees the marker the tick the
/// stream comes up, and a device-less world never gains one. A caller can
/// therefore tell a world with nowhere to play from a still-loading
/// soundfont by waiting on the subscription rather than guessing.
#[test]
fn the_audio_world_reports_its_output_opening() {
    let mut host = World::new();
    let world = spawn_audio_world_with::<Threaded>(&mut host, AudioConfig::default());
    let sender = world.sender.clone();
    // Both subscriptions are made the way a caller installs sources: the
    // format says which rate to build them at, the marker says the output
    // exists at all.
    let format = sender
        .watch(|world: &World| {
            world
                .root_scope()
                .query::<&AudioFormat>()
                .next()
                .map(|audio_format| *audio_format)
        })
        .expect("the audio world accepts a subscription");
    let opened = sender
        .watch(|world: &World| {
            world
                .root_scope()
                .query::<&AudioOutputOpen>()
                .next()
                .map(|_audio_output_open| ())
        })
        .expect("the audio world accepts a subscription");
    let installed = wait_for(|| format.latest().is_some());
    if !has_output_device() {
        assert!(!installed, "a device-less world published a format");
        assert!(
            !wait_for(|| opened.latest().is_some()),
            "a world without an output announced it is open"
        );
        return;
    }
    assert!(installed, "the output opened but no format was published");
    assert!(
        wait_for(|| opened.latest().is_some()),
        "the output opened but the marker never landed"
    );
}

/// Whether the default host has an output device to open.
fn has_output_device() -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};

    cpal::default_host()
        .default_output_device()
        .is_some_and(|device| {
            device
                .supported_output_configs()
                .map(|mut configs| configs.next().is_some())
                .unwrap_or(false)
        })
}

/// The marker the job test leaves in the audio world.
struct Marker;

/// A broken soundfont leaves the world without sources, so its fills are
/// silence rather than a panic on the first chunk.
#[test]
fn a_failed_soundfont_leaves_the_world_silent() {
    let mut world = World::new();
    assert!(install_synth(&mut world, b"not a soundfont", 4.0).is_none());
    assert!(
        world.root_scope().query::<&OnAudioFill>().next().is_none(),
        "a world whose soundfont failed carries no source"
    );
    let mut out = vec![1.0f32; 256];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.0));
}

/// A MIDI file sorts its events by time, so a caller's sequence does not
/// have to arrive in order.
#[test]
fn midi_file_sorts_its_events() {
    let events = [
        (
            1.0,
            MidiEventKind::NoteOff {
                channel: 0,
                key: 60,
            },
        ),
        (
            0.5,
            MidiEventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
            },
        ),
    ];
    let file = midi_file(events).expect("out of order events still build");
    let times = file.get_times();
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "the times are sorted"
    );
    assert!(file.get_length() >= 1.0);
}

/// A time that is not a number has no place in a sequence, and the parser
/// says so rather than sorting it somewhere arbitrary.
#[test]
fn midi_file_rejects_a_nan_time() {
    let events = [
        (
            f64::NAN,
            MidiEventKind::NoteOff {
                channel: 0,
                key: 60,
            },
        ),
        (
            0.5,
            MidiEventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
            },
        ),
    ];
    assert!(matches!(midi_file(events), Err(VoiceError::MidiFile(_))));
}

/// A soundfont parser rejects bytes that are not a soundfont.
#[test]
fn load_soundfont_rejects_garbage() {
    assert!(matches!(
        load_soundfont(&[0u8; 64]),
        Err(VoiceError::SoundFont(_))
    ));
}

/// A type 0 file with one note and a loop marker written as controller 111,
/// which only the RPG Maker loop convention reads.
fn loop_marked_midi() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"MThd");
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes()); // format 0
    bytes.extend_from_slice(&1u16.to_be_bytes()); // one track
    bytes.extend_from_slice(&96u16.to_be_bytes()); // ticks per quarter
    let mut track = Vec::new();
    track.extend_from_slice(&[0x00, 0x90, 0x3C, 0x64]); // note on at 0
    track.extend_from_slice(&[0x60, 0xB0, 0x6F, 0x00]); // controller 111 a beat in
    track.extend_from_slice(&[0x60, 0x80, 0x3C, 0x00]); // note off a beat later
    track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]); // end of track
    bytes.extend_from_slice(b"MTrk");
    bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&track);
    bytes
}

/// The loop convention a caller names decides whether a file carries a loop
/// marker at all: the default reads none, and the RPG Maker one turns
/// controller 111 into a start.
#[test]
fn load_midi_file_with_loop_type_marks_the_loop() {
    let bytes = loop_marked_midi();
    let marked = load_midi_file_with_loop_type(&bytes, MidiFileLoopType::RpgMaker)
        .expect("the bytes are a midi file");
    let start = marked
        .get_messages()
        .iter()
        .position(|message| matches!(message, MidiMessage::LoopStart));
    let start = start.expect("the rpg maker convention marks a loop");
    let at = marked.get_times()[start];
    assert!((at - 0.5).abs() < 1e-6, "the marker landed at {at} seconds");

    let plain = load_midi_file(&bytes).expect("the bytes are a midi file");
    assert!(
        !plain
            .get_messages()
            .iter()
            .any(|message| matches!(message, MidiMessage::LoopStart | MidiMessage::LoopEnd)),
        "the default convention marks no loop"
    );
}

/// A voice has no synthesizer until the first fill names a sample rate,
/// because that is the first moment the rate is known.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_voice_builds_its_synthesizer_on_the_first_fill() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    let voice = install_synth(&mut world, &font, 1.0).expect("the asset soundfont parses");
    let built = |world: &World| {
        world
            .root_scope()
            .query_one::<&SynthVoice>(voice)
            .is_ok_and(|voice| voice.synthesizer.is_some())
    };
    assert!(
        !built(&world),
        "a voice built a synthesizer before any fill"
    );
    let mut out = vec![0.0f32; BLOCK * 2];
    dispatch_audio_fill(&world, &mut out, SAMPLE_RATE, 2);
    assert!(built(&world), "the first fill left the voice unbuilt");
}

/// A message sent before the voice has a synthesizer is not lost: the first
/// fill builds one and plays what was waiting.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_message_sent_before_the_first_fill_still_sounds() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    let voice = install_synth(&mut world, &font, 1.0).expect("the asset soundfont parses");
    midi_job(
        voice,
        MidiEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100,
        },
        0.0,
    )(&mut world);
    let heard = fills_until_sound(&world, ONSET_FILLS);
    assert!(
        heard < ONSET_FILLS,
        "the note never sounded: the queue was dropped with the first fill"
    );
}

/// A delayed message waits its delay out and lands in the fill its offset
/// falls in, not on the next one.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_delayed_message_lands_after_its_offset() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    let voice = install_synth(&mut world, &font, 1.0).expect("the asset soundfont parses");
    // One second of delay is longer than the fills the first probe runs for,
    // so a note heard there would mean the delay was ignored.
    midi_job(
        voice,
        MidiEventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100,
        },
        1.0,
    )(&mut world);
    let early = fills_until_sound(&world, ONSET_FILLS);
    assert_eq!(early, ONSET_FILLS, "the note sounded before its delay");
    let heard = fills_until_sound(&world, ONSET_FILLS);
    assert!(heard < ONSET_FILLS, "the delayed note never sounded");
}

/// The whole chain, from a loop marker in a file to samples on the stream:
/// a looping sequence repeats its pass and never finishes.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_loop_marker_repeats_the_pass() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    let voice = install_synth(&mut world, &font, 1.0).expect("the asset soundfont parses");
    let (sender, receiver) = async_channel::unbounded();
    let audio = JobSender::new(sender);
    // A beat of sound between two loop markers: every pass strikes the note
    // again, and nothing after the end marker is ever reached.
    let file = MidiFile::new_with_events(vec![
        (0.0, MidiMessage::LoopStart),
        (0.05, note_on(60)),
        (0.2, note_off(60)),
        (0.5, MidiMessage::LoopEnd),
    ])
    .expect("the events are well formed");
    let mut sequencer = MidiSequencer::new(Arc::new(file));
    sequencer.loop_play = true;
    // Small advances, one fill at a time, the way a frame at 60 Hz drives it:
    // the wrap lands inside a frame rather than on its boundary.
    let step = BLOCK as f64 / f64::from(SAMPLE_RATE);
    for pass in 1..=3 {
        let mut peak = 0.0f32;
        // Half a second of frames is a whole pass.
        for _ in 0..ONSET_FILLS {
            sequencer.advance(step, voice, &audio);
            pump(&mut world, &receiver);
            peak = peak.max(
                fill(&world)
                    .iter()
                    .fold(0.0f32, |one, sample| one.max(sample.abs())),
            );
        }
        assert!(peak > 0.05, "pass {pass} made no sound of its own");
        assert!(!sequencer.finished, "a looping file never finishes");
    }
    // The cursor never left the loop body: whatever step the passes ended on,
    // the clock is still inside the half second between the two markers.
    assert_eq!(
        sequencer.loop_point,
        Some((0.0, 0)),
        "the start marker is the loop point"
    );
    assert!(
        sequencer.time < 0.5,
        "the clock ran past the end marker: {}",
        sequencer.time
    );
}

/// A note on at middle C.
fn note_on(key: u8) -> MidiMessage {
    MidiEventKind::NoteOn {
        channel: 0,
        key,
        velocity: 100,
    }
    .into_message()
}

/// A note off at middle C.
fn note_off(key: u8) -> MidiMessage {
    MidiEventKind::NoteOff { channel: 0, key }.into_message()
}

/// Runs the jobs a sequence has queued so far against its world.
fn pump(world: &mut World, receiver: &async_channel::Receiver<Job>) {
    while let Ok(job) = receiver.try_recv() {
        job(world);
    }
}

/// The chunk the tests render, in frames per channel: the same size the
/// audio thread fills with.
const BLOCK: usize = 256;

/// The sample rate the tests render at, in hertz.
const SAMPLE_RATE: u32 = 48_000;

/// How many fills one second of audio lasts.
const ONSET_FILLS: usize = SAMPLE_RATE as usize / BLOCK;

/// Render one chunk and answer whether any sample moved.
fn sounding(world: &World) -> bool {
    fill(world).iter().any(|sample| *sample != 0.0)
}

/// Render one chunk of interleaved frames.
fn fill(world: &World) -> Vec<f32> {
    let mut out = vec![0.0f32; BLOCK * 2];
    dispatch_audio_fill(world, &mut out, SAMPLE_RATE, 2);
    out
}

/// How many fills until a sample moves, or `limit` when none does.
fn fills_until_sound(world: &World, limit: usize) -> usize {
    for index in 0..limit {
        if sounding(world) {
            return index;
        }
    }
    limit
}

/// The soundfont the workspace keeps under `assets`, when it is there: the
/// same file the demo embeds, so these tests skip rather than fail where
/// the asset is absent.
fn asset_soundfont() -> Option<Arc<[u8]>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/FluidR3Mono_GM.sf3");
    std::fs::read(path)
        .ok()
        .map(|bytes| Arc::from(bytes.as_slice()))
}
