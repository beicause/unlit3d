//! The audio remote world: a callback-driven output stream fed by a ring.
//!
//! The audio thread owns the stream and a lock-free ring buffer; the
//! operating system's callback pops whatever is newest and silences the
//! rest, so a late simulation degrades to silence instead of stuttering.
//! What sound plays is decided by a [`PcmSource`] the caller's factory
//! builds on the audio thread, where it can reach the world's entities.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use unlit_ecs::{Entity, World};

use crate::thread::FixedClock;
use crate::world::spawn_remote_world;

/// How many frames the filler generates per pass, the unit the pacing and
/// the ring's headroom are measured in.
const FILL_FRAMES: usize = 256;

/// How the audio world is set up before its first fill.
#[derive(Clone, Debug)]
pub struct AudioConfig {
    /// The ring buffer's length, in frames per channel: the total latency
    /// the filler can smooth over.
    pub buffer_frames: usize,
    /// The sample rate to ask the output device for, in hertz.
    pub preferred_sample_rate: u32,
    /// The command channel's capacity, in queued jobs.
    pub command_capacity: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            buffer_frames: 4096,
            preferred_sample_rate: 48000,
            command_capacity: 256,
        }
    }
}

/// A source of audio samples, run on the audio thread once per filled chunk.
///
/// The buffer is interleaved frames: one set of samples per channel. The
/// default impl leaves the buffer silent, which is what the stream plays
/// when no source exists yet.
pub trait PcmSource: Send {
    /// Fill the buffer with the next chunk of interleaved audio.
    fn fill(&mut self, world: &World, out: &mut [f32], sample_rate: u32, channels: u16) {
        let _ = (world, out, sample_rate, channels);
    }
}

/// Builds the world's [`PcmSource`] on the audio thread.
///
/// The thread hands over its world, its output's sample rate and channel
/// count, and takes the mixer back: this is the one moment a caller's
/// setup may touch entities on the audio world.
pub type PcmSourceFactory = Box<dyn FnOnce(&mut World, u32, u16) -> Box<dyn PcmSource> + Send>;

/// Everything the open output stream needs to keep running.
struct OpenOutput {
    /// The stream, kept alive by being held.
    _stream: cpal::Stream,
    /// The producer end of the ring the callback drains.
    producer: HeapProd<f32>,
    /// The sample rate the stream actually opened at, in hertz.
    sample_rate: u32,
    /// The channel count the stream actually opened at.
    channels: u16,
}

/// Spawn the audio remote world on a fresh thread, and return the proxy
/// entity carrying its command channel.
///
/// The thread opens the default output device on its first tick and keeps it
/// open; until then, and whenever no device exists, the world runs silently.
/// The factory, when given, runs once on the audio thread as soon as the
/// output's format is known.
pub fn spawn_audio_world(
    host: &mut World,
    config: AudioConfig,
    factory: Option<PcmSourceFactory>,
) -> Entity {
    let mut factory = factory;
    let mut output: Option<OpenOutput> = None;
    let mut source: Option<Box<dyn PcmSource>> = None;
    let mut scratch: Vec<f32> = Vec::new();
    spawn_remote_world(
        host,
        "audio",
        config.command_capacity,
        move |world, control| {
            if control.stop_requested() {
                return false;
            }
            if output.is_none() {
                match open_output(&config) {
                    Some(opened) => {
                        log::info!(
                            "the audio output opened at {} Hz, {} channels",
                            opened.sample_rate,
                            opened.channels
                        );
                        if let Some(factory) = factory.take() {
                            source = Some(factory(world, opened.sample_rate, opened.channels));
                        }
                        output = Some(opened);
                    }
                    None => {
                        log::warn!("no audio output device is available yet");
                    }
                }
            }
            if let Some(output) = output.as_mut() {
                let chunk = FILL_FRAMES * usize::from(output.channels);
                while output.producer.vacant_len() >= chunk {
                    scratch.resize(chunk, 0.0);
                    if let Some(source) = source.as_mut() {
                        source.fill(world, &mut scratch, output.sample_rate, output.channels);
                    }
                    output.producer.push_slice(&scratch);
                }
                // Half a chunk of pacing: the ring stays ahead of the
                // callback without the filler spinning.
                let pace = FILL_FRAMES as f64 / f64::from(output.sample_rate) * 0.5;
                let mut clock = FixedClock::new(Duration::from_secs_f64(pace));
                clock.wait(control);
            } else {
                control.wait_timeout(Duration::from_secs(1));
            }
            true
        },
    )
}

/// Open the default output stream and the ring feeding it, preferring a
/// stereo f32 stream at the configured rate.
fn open_output(config: &AudioConfig) -> Option<OpenOutput> {
    let device = cpal::default_host().default_output_device()?;
    let supported = device.supported_output_configs().ok()?;
    let best = supported
        .filter(|range| range.sample_format() == cpal::SampleFormat::F32)
        .map(|range| {
            let rate = config
                .preferred_sample_rate
                .clamp(range.min_sample_rate(), range.max_sample_rate());
            range.with_sample_rate(rate)
        })
        .max_by_key(|stream| {
            (
                usize::from(stream.channels() == 2),
                // The closer to the requested rate, the better.
                u32::MAX - stream.sample_rate().abs_diff(config.preferred_sample_rate),
            )
        })?;
    let stream_config: cpal::StreamConfig = best.into();
    let channels = stream_config.channels;
    let sample_rate = stream_config.sample_rate;
    let (producer, mut consumer) =
        HeapRb::<f32>::new(config.buffer_frames * usize::from(channels)).split();
    let stream = device
        .build_output_stream::<f32, _, _>(
            stream_config,
            move |out, _| {
                // Underrun: play what is left, silence the rest.
                let filled = consumer.pop_slice(out);
                out[filled..].fill(0.0);
            },
            |error| log::error!("the audio output stream failed ({error})"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(OpenOutput {
        _stream: stream,
        producer,
        sample_rate,
        channels,
    })
}

/// A source that plays nothing: the fallback when no mixer can be built.
#[derive(Clone, Copy, Debug, Default)]
pub struct Silent;

impl PcmSource for Silent {
    fn fill(&mut self, _world: &World, out: &mut [f32], _sample_rate: u32, _channels: u16) {
        out.fill(0.0);
    }
}
