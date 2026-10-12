//! Integration tests for the audio remote world: the PCM source contract,
//! and the world's job channel with and without an output device.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use unlit_ecs::World;
use unlit3dx::audio::Silent;
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

/// The source the device test counts fills with.
struct Counting(Arc<AtomicUsize>);

impl PcmSource for Counting {
    fn fill(&mut self, _world: &World, out: &mut [f32], _sample_rate: u32, _channels: u16) {
        self.0.fetch_add(1, Ordering::SeqCst);
        out.fill(0.25);
    }
}

/// A source owns its buffer: it may write every sample, and the default
/// source the mixer falls back to writes silence.
#[test]
fn pcm_sources_fill_their_buffers() {
    let mut out = vec![1.0f32; 128];
    Silent.fill(&World::new(), &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.0));

    let mut counting = Counting(Arc::new(AtomicUsize::new(0)));
    counting.fill(&World::new(), &mut out, 48_000, 2);
    assert!(out.iter().all(|sample| *sample == 0.25));
}

/// The audio world runs its jobs and, when the machine has an output
/// device, drives its source once per filled chunk.
#[test]
fn the_audio_world_runs_jobs_and_drives_its_source() {
    let mut host = World::new();
    let fills = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fills);
    let world = spawn_audio_world(
        &mut host,
        AudioConfig::default(),
        Some(Box::new(move |_world: &mut World, _rate, _channels| {
            Box::new(Counting(counted))
        })),
    );
    let remote = host.get::<RemoteWorld>(world).expect("the proxy exists");

    // A marker job and a probe that waits for it: jobs are answered
    // whether or not a device exists, so a device-less world runs
    // silently but stays alive.
    remote.send(Box::new(|world: &mut World| {
        world.spawn((Marker,));
    }));
    let marked = remote.request(Box::new(|world: &mut World| {
        world.query::<&Marker>().next().is_some()
    }));
    assert!(
        wait_for(|| marked.poll() == Some(true)),
        "the audio world never ran the marker job"
    );

    // With a device present the factory must have run and the mixer must
    // be filling; without one the world stays alive and silent instead.
    if has_output_device() {
        assert!(
            wait_for(|| fills.load(Ordering::SeqCst) > 0),
            "the output device exists but the source was never filled"
        );
    } else {
        assert_eq!(fills.load(Ordering::SeqCst), 0);
        assert!(!remote.thread().is_finished(), "the device-less world died");
    }
}

/// The audio world announces when it can sound: the inbox flips to a
/// published state once the stream and the source exist, or to `false`
/// when they never will. Either way it lands without any job being
/// sent, so a caller can tell silence from a still-loading factory.
#[test]
fn the_audio_world_publishes_its_readiness() {
    let mut host = World::new();
    let world = spawn_audio_world(
        &mut host,
        AudioConfig::default(),
        Some(Box::new(|_world: &mut World, _rate, _channels| {
            Box::new(Silent)
        })),
    );
    let inbox = host
        .get::<AudioReadyInbox>(world)
        .expect("the audio proxy carries its readiness inbox")
        .0
        .clone();
    assert!(
        wait_for(|| inbox.latest().is_some()),
        "the readiness inbox never published a state"
    );
    assert_eq!(
        inbox.latest().map(|state| state.0),
        Some(has_output_device()),
        "readiness must match whether an output device exists"
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

/// The mixer a broken soundfont leaves behind still fills its buffers:
/// silence, ready for the stream.
#[test]
fn a_failed_soundfont_leaves_a_silent_mixer() {
    let mut out = vec![1.0f32; 256];
    let mut source =
        synth_source_bytes(Arc::from(&b"not a soundfont"[..]), 4.0)(&mut World::new(), 48_000, 2);
    source.fill(&World::new(), &mut out, 48_000, 2);
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
