//! Integration tests for the audio remote world: the fill-source contract,
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

use unlit_ecs::World;
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

/// Install the synthesizer sources on a world: the mix's gain, one live
/// voice and one player per track name, all built at `sample_rate`.
///
/// A caller queues this as a job to the audio world once the output has
/// opened and its [`AudioFormat`] has landed; these tests drive worlds
/// themselves, so they call it directly.
fn install_synth(world: &mut World, bytes: &[u8], gain: f32, tracks: &[&str], sample_rate: u32) {
    for (index, one) in tracks.iter().enumerate() {
        assert!(
            !tracks[..index].contains(one),
            "the track name `{one}` is given twice"
        );
    }
    world.root_scope_mut().spawn((AudioGain(gain),));
    if let Ok(font) = load_soundfont(bytes) {
        let _ = SynthVoice::from_font(world, &font, sample_rate);
        for track in tracks {
            let _ = MidiPlayer::from_font(world, &font, sample_rate, track);
        }
    }
}

/// Spawn a source that counts the fills it takes and adds `level` to every
/// sample, the way a synthesizer adds its block to the chunk.
fn counting_source(world: &mut World, level: f32, counts: &Arc<AtomicUsize>) {
    let counted = Arc::clone(counts);
    world
        .root_scope_mut()
        .spawn((OnAudioFill::new([OnAudioFill::callback(
            AudioFillOrder::default(),
            move |_world, _entity, chunk| {
                counted.fetch_add(1, Ordering::SeqCst);
                for sample in chunk.out.iter_mut() {
                    *sample += level;
                }
            },
        )]),));
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

/// One entity may hold several sources in one component: the dispatcher mixes
/// every callback, in the order it was added.
#[test]
fn one_entity_may_hold_several_sources() {
    let mut world = World::new();
    let counts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&counts);
    world.root_scope_mut().spawn((OnAudioFill::new([
        OnAudioFill::callback(AudioFillOrder::default(), move |_world, _entity, chunk| {
            counted.fetch_add(1, Ordering::SeqCst);
            for sample in chunk.out.iter_mut() {
                *sample += 0.25;
            }
        }),
        OnAudioFill::callback(AudioFillOrder::default(), move |_world, _entity, chunk| {
            for sample in chunk.out.iter_mut() {
                *sample += 0.5;
            }
        }),
    ]),));
    let mut out = vec![0.0f32; 4];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| (sample - 0.75).abs() < 1e-6));
    assert_eq!(counts.load(Ordering::SeqCst), 1, "one dispatched fill");
}

/// The world's gain scales the mix once, after the sources summed, and an
/// overdriven mix clamps instead of wrapping.
#[test]
fn the_worlds_gain_scales_the_mix() {
    let mut world = World::new();
    world.root_scope_mut().spawn((AudioGain(2.0),));
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

/// Equal orders fall back to entity order, so a mix built from several
/// sources stays reproducible.
#[test]
fn equal_orders_run_in_entity_order() {
    let mut world = World::new();
    let ran: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    for (name, level) in [("first", 0.1), ("second", 0.2)] {
        let ran = Arc::clone(&ran);
        world
            .root_scope_mut()
            .spawn((OnAudioFill::new([OnAudioFill::callback(
                AudioFillOrder::default(),
                move |_world, _entity, chunk| {
                    ran.lock().unwrap().push(name);
                    for sample in chunk.out.iter_mut() {
                        *sample += level;
                    }
                },
            )]),));
    }
    // A later order runs after every earlier one.
    let late = Arc::clone(&ran);
    world
        .root_scope_mut()
        .spawn((OnAudioFill::new([OnAudioFill::callback(
            AudioFillOrder(1),
            move |_world, _entity, _chunk| {
                late.lock().unwrap().push("late");
            },
        )]),));
    let mut out = vec![0.0f32; 2];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert_eq!(*ran.lock().unwrap(), vec!["first", "second", "late"]);
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
        .send(Box::new(move |world: &mut World| {
            counting_source(world, 0.25, &counted);
        }))
        .expect("the audio world takes its source");
    // A marker job and a probe that waits for it.
    let _ = world.send(Box::new(|world: &mut World| {
        world.root_scope_mut().spawn((Marker,));
    }));
    let marked = world.request(Box::new(|world: &mut World| {
        world.root_scope().query::<&Marker>().next().is_some()
    }));
    assert!(
        wait_for(|| marked.poll() == Some(true)),
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
    let sender = world.sender();
    // Both subscriptions are made the way a caller installs sources: the
    // format says which rate to build them at, the marker says the output
    // exists at all.
    let format = sender
        .subscribe(|world: &World| {
            world
                .root_scope()
                .query::<&AudioFormat>()
                .next()
                .map(|(_, f)| *f)
        })
        .expect("the audio world accepts a subscription");
    let opened = sender
        .subscribe(|world: &World| {
            world
                .root_scope()
                .query::<&AudioOutputOpen>()
                .next()
                .map(|_| ())
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
    install_synth(&mut world, b"not a soundfont", 4.0, &["a", "b"], 48_000);
    assert!(
        world.root_scope().query::<&OnAudioFill>().next().is_none(),
        "a world whose soundfont failed carries no sources"
    );
    let mut out = vec![1.0f32; 256];
    dispatch_audio_fill(&world, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.0));
}

/// A MIDI file refuses event times that run backwards.
#[test]
fn midi_file_rejects_out_of_order_times() {
    let events = [(1.0, 0x90, 60, 100), (0.5, 0x80, 60, 0)];
    assert!(matches!(midi_file(&events), Err(VoiceError::MidiFile(_))));
}

/// A MIDI file builds from sorted event times.
#[test]
fn midi_file_accepts_sorted_events() {
    let events = [(0.5, 0x90, 60, 100), (1.0, 0x80, 60, 0)];
    let file = midi_file(&events).expect("sorted events build");
    assert!(file.get_length() >= 1.0);
}

/// A soundfont parser rejects bytes that are not a soundfont.
#[test]
fn load_soundfont_rejects_garbage() {
    assert!(matches!(
        load_soundfont(&[0u8; 64]),
        Err(VoiceError::SoundFont(_))
    ));
}

/// Transport jobs address a track by name, so a job for a track the world
/// does not carry — one whose soundfont is still loading, a name typed
/// wrong — must be a no-op rather than a panic or a wrong track.
#[test]
fn transport_jobs_for_unknown_tracks_do_nothing() {
    let mut world = World::new();
    let file = midi_file(&[(0.0, 0x90, 60, 90)]).expect("a one note file");
    for name in ["background", "typo"] {
        let play = play_track_job(name, file.clone(), true);
        play(&mut world);
        let pause = pause_track_job(name, true);
        pause(&mut world);
        let rewind = rewind_track_job(name);
        rewind(&mut world);
        let stop = stop_track_job(name);
        stop(&mut world);
    }
    // A world whose soundfont never loaded still renders silence, ready
    // for the stream, rather than dying on the first fill.
    let mut broken = World::new();
    install_synth(
        &mut broken,
        b"not a soundfont",
        1.0,
        &["background"],
        48_000,
    );
    let mut out = vec![1.0f32; 64];
    dispatch_audio_fill(&broken, &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.0));
}

/// The transport's contract, on a track of one audio world: pausing stops
/// the sound and holds the sequencer's clock, resuming continues where the
/// pause left off, and a rewind puts the same file back at its start.
///
/// The clock is what makes the test worth having. A pause that only muted
/// the track would keep the sequence walking, so the resume would land
/// past the note; a rewind that did nothing would leave the track finished
/// and silent. The onset time of the first sound after each job says which
/// of the three the track is on.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_track_holds_its_clock_across_pause_and_rewind() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    install_synth(
        &mut world,
        &font,
        1.0,
        &["background", "sequence"],
        SAMPLE_RATE,
    );
    // One note, struck a second in and released a second later: nothing
    // before the first second sounds, and nothing after the second does.
    let file = midi_file(&[(1.0, 0x90, 60, 100), (2.0, 0x80, 60, 0)]).expect("a one note file");
    play_track_job("sequence", file, false)(&mut world);

    // The onset lands where the note says it should.
    let onset = fills_until_sound(&world, ONSET_FILLS * 2);
    assert!(
        (ONSET_FILLS - 8..ONSET_FILLS + 8).contains(&onset),
        "the note sounded after {onset} fills, expected about {ONSET_FILLS}"
    );
    // Pause for longer than the whole file lasts, then resume: the clock
    // stood still, so the note already struck is still sounding. A pause
    // that only muted the track would have walked past the file's end, and
    // the resume would be silence for good.
    pause_track_job("sequence", true)(&mut world);
    for _ in 0..ONSET_FILLS * 3 {
        assert!(
            !sounding(&world),
            "a paused track is still rendering samples"
        );
    }
    pause_track_job("sequence", false)(&mut world);
    assert!(
        sounding(&world),
        "the resume found nothing left to play: the pause ran the sequencer's clock"
    );

    // Rewind: the same file starts over, so the note is a second of playing
    // away again rather than still sounding where it was held.
    rewind_track_job("sequence")(&mut world);
    let after_rewind = fills_until_sound(&world, ONSET_FILLS * 3);
    assert!(
        (ONSET_FILLS - 8..ONSET_FILLS + 8).contains(&after_rewind),
        "after the rewind the note came after {after_rewind} fills, expected about {ONSET_FILLS}"
    );
}

/// Two tracks of one audio world are driven apart: pausing one leaves the
/// other playing, and stopping a name the world does not carry silences
/// nothing.
#[test]
fn the_other_track_keeps_playing() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    install_synth(
        &mut world,
        &font,
        1.0,
        &["background", "sequence"],
        SAMPLE_RATE,
    );
    let file = midi_file(&[(0.0, 0x90, 60, 100), (0.5, 0x80, 60, 0)]).expect("a one note file");
    play_track_job("background", file.clone(), true)(&mut world);
    play_track_job("sequence", file, false)(&mut world);
    assert!(sounding(&world), "both tracks sound");

    pause_track_job("sequence", true)(&mut world);
    stop_track_job("nobody")(&mut world);
    // The loop keeps playing: its file restarts on every pass, so the mix
    // stays alive while the paused track holds still.
    let mut quiet = 0;
    for _ in 0..ONSET_FILLS {
        if sounding(&world) {
            return;
        }
        quiet += 1;
    }
    panic!("pausing one track silenced the other after {quiet} fills");
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

/// How many one-shot plays `track` has walked to its end.
fn ends_of(world: &World, track: &str) -> u64 {
    world
        .root_scope()
        .query::<&MidiPlayer>()
        .find(|(_, player)| player.name() == track)
        .map_or(0, |(_, player)| player.ends())
}

/// A one-shot track counts each play to its end, and a looping one never
/// does. A reader that remembers the count it last saw sees every end,
/// so the count has to move exactly once per play rather than on every
/// fill that follows.
///
/// The soundfont is the workspace's own asset, so the test skips where the
/// file is absent.
#[test]
fn a_finished_track_is_counted_once_per_play() {
    let Some(font) = asset_soundfont() else {
        eprintln!("skipping: no soundfont under assets");
        return;
    };
    let mut world = World::new();
    install_synth(
        &mut world,
        &font,
        1.0,
        &["background", "sequence"],
        SAMPLE_RATE,
    );
    // Half a second of one note: short enough to end within a few fills.
    let file = midi_file(&[(0.0, 0x90, 60, 100), (0.5, 0x80, 60, 0)]).expect("a one note file");
    play_track_job("sequence", file.clone(), false)(&mut world);
    play_track_job("background", file, true)(&mut world);

    let mut seen = 0;
    for _ in 0..ONSET_FILLS * 2 {
        let _ = fill(&world);
        let ends = ends_of(&world, "sequence");
        assert!(ends >= seen, "the count fell from {seen} to {ends}");
        assert!(ends <= 1, "one play was counted {ends} times");
        seen = ends;
    }
    assert_eq!(seen, 1, "the sequence never reached its end");
    assert_eq!(ends_of(&world, "background"), 0, "a loop never ends");

    // A second play is a second end, not the first one repeated.
    let short = midi_file(&[(0.0, 0x90, 60, 100), (0.2, 0x80, 60, 0)]).expect("a short file");
    play_track_job("sequence", short, false)(&mut world);
    for _ in 0..ONSET_FILLS * 2 {
        let _ = fill(&world);
        if ends_of(&world, "sequence") == 2 {
            return;
        }
    }
    panic!("a second play was not counted");
}
