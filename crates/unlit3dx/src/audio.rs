//! The audio remote world: a callback-driven output stream fed by a ring.
//!
//! The audio thread owns the stream and a lock-free ring buffer; the
//! operating system's callback pops whatever is newest and silences the
//! rest, so a late simulation degrades to silence instead of stuttering.
//! What sound plays is decided by the world's [`OnFill`] components: the
//! thread runs them once per filled chunk and sums their samples into the
//! ring. The output's format is not known until its stream opens, so the
//! thread publishes it as [`AudioFormat`] and a caller installs its
//! sources then, with an ordinary job: a world that carries none plays
//! nothing and says so through [`AudioReady`].

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use unlit_ecs::{Entity, World};

use crate::channel::{SnapshotReader, snapshot_pair};
use crate::handle::WorldHandle;
use crate::thread::{FixedClock, ThreadControl};
use crate::world::WorldRunner;

/// How many frames the filler generates per pass, the unit the pacing and
/// the ring's headroom are measured in.
const FILL_FRAMES: usize = 256;

/// How the audio world is set up before its first fill.
#[derive(Clone, Debug)]
pub struct AudioConfig {
    /// The ring buffer's length, in frames per channel: the total latency
    /// the filler can smooth over. Must hold at least two fill chunks, or
    /// the filler would never see room to write one.
    pub buffer_frames: usize,
    /// The sample rate to ask the output device for, in hertz.
    pub preferred_sample_rate: u32,
    /// The command channel's capacity, in queued jobs.
    pub command_capacity: usize,
    /// Whether a world the host steps still opens an output device.
    ///
    /// A stepped world opens none by default: a headless run pulls its
    /// fills by hand and has nothing to play to. A browser is the
    /// exception - its output is a Web Audio context, which the page
    /// drives, so it needs no thread of its own to live on.
    pub stepped_device: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            buffer_frames: 4096,
            preferred_sample_rate: 48000,
            command_capacity: 256,
            stepped_device: false,
        }
    }
}

/// Where a fill behaviour runs among the world's other sources.
///
/// Sources with the same order run in the order they were mounted, so a
/// mix is reproducible: the same spawns give the same sum, in the same
/// order, at the same sample offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct FillOrder(pub i32);

/// One chunk of interleaved audio a fill behaviour contributes to.
///
/// The chunk arrives silent and the behaviour adds its samples to it rather
/// than overwriting them: adding is what lets several sources play at once.
/// Samples past the first two channels of a wider stream are the right
/// channel's, which is what a stereo source has to offer.
pub struct AudioChunk<'a> {
    /// The chunk's samples, interleaved frames, silent unless a behaviour
    /// has already added to it.
    pub out: &'a mut [f32],
    /// The output's sample rate, in hertz.
    pub sample_rate: u32,
    /// The output's channel count.
    pub channels: u16,
}

impl AudioChunk<'_> {
    /// How many frames the chunk holds.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.out.len() / usize::from(self.channels.max(1))
    }
}

/// The callback a fill behaviour runs: it adds its source's samples to the
/// chunk it is handed.
type FillCallback = Box<dyn FnMut(&World, Entity, &mut AudioChunk)>;

/// The state every fill behaviour shares: its callback, its order, and when
/// it was mounted.
struct FillSlot {
    callback: FillCallback,
    order: FillOrder,
    mount_index: u64,
}

/// A source of audio samples: a component the audio thread runs once per
/// filled chunk.
///
/// Spawn it on its own entity with [`WorldFillExt::spawn_on_fill`], or
/// mount it with [`WorldFillExt::mount_on_fill`] to bundle it with the
/// state it renders from — a synthesizer, say. [`dispatch_fill`] runs a
/// world's sources and mixes them.
pub struct OnFill(FillSlot);

impl OnFill {
    /// Wrap `f` as an [`OnFill`].
    ///
    /// `order` places it among the world's other sources; a source that
    /// only adds its own samples can keep [`FillOrder::default`].
    #[must_use]
    pub fn new(order: FillOrder, f: impl FnMut(&World, Entity, &mut AudioChunk) + 'static) -> Self {
        Self(FillSlot {
            callback: Box::new(f),
            order,
            mount_index: 0,
        })
    }

    /// The order this source runs in.
    #[must_use]
    pub fn order(&self) -> FillOrder {
        self.0.order
    }

    /// When this source was mounted, which breaks ties between equal orders.
    #[must_use]
    pub fn mount_index(&self) -> u64 {
        self.0.mount_index
    }

    /// Record when this source was mounted.
    fn set_mount_index(&mut self, index: u64) {
        self.0.mount_index = index;
    }

    /// Run this source for `entity`, adding its samples to `chunk`.
    fn run(&mut self, world: &World, entity: Entity, chunk: &mut AudioChunk) {
        (self.0.callback)(world, entity, chunk);
    }
}

/// The next mount index to hand out.
///
/// Kept in the world so it is monotonic across the world's life rather than
/// per driver: two drivers sharing a world must not hand out the same
/// indices, or the tie-break would not be total.
#[derive(Debug, Default)]
struct FillMounts(u64);

/// Spawning fill behaviours, which record the order they were mounted in.
pub trait WorldFillExt {
    /// Spawn `behaviour` on an entity of its own, as the next source
    /// mounted in the world.
    fn spawn_on_fill(&mut self, behaviour: OnFill) -> Entity;

    /// Record `behaviour` as the next source mounted and hand it back, so
    /// it can be bundled with the components it renders from.
    fn mount_on_fill(&mut self, behaviour: OnFill) -> OnFill;
}

impl WorldFillExt for World {
    fn spawn_on_fill(&mut self, behaviour: OnFill) -> Entity {
        let behaviour = self.mount_on_fill(behaviour);
        self.spawn((behaviour,))
    }

    fn mount_on_fill(&mut self, mut behaviour: OnFill) -> OnFill {
        let index = next_mount_index(self);
        behaviour.set_mount_index(index);
        behaviour
    }
}

/// Take the next mount index from the world's counter, spawning it if absent.
fn next_mount_index(world: &mut World) -> u64 {
    let counter = world
        .query::<&FillMounts>()
        .next()
        .map(|(entity, _)| entity);
    let Some(counter) = counter else {
        world.spawn((FillMounts(1),));
        return 0;
    };
    world
        .with_mut::<FillMounts, _>(counter, |mounts| {
            let index = mounts.0;
            mounts.0 += 1;
            index
        })
        .expect("the mount counter exists")
}

/// The factor the mixed samples scale by, read from the audio world.
///
/// The gain belongs to the output rather than to one source:
/// [`dispatch_fill`] sums every source and scales the sum once, so adding a
/// source changes the mix without changing anyone's level. Soundfont presets
/// leave a lot of headroom, so a mix of a few quiet voices usually wants a
/// gain above one. Samples clamp to the unit range after the gain, so an
/// overdriven mix distorts rather than wraps. A world carrying no gain
/// passes its mix through unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioGain(pub f32);

/// Run every [`OnFill`] in `world` once, mixing their samples into `out`.
///
/// `out` is interleaved frames and is zeroed first; each source adds its
/// contribution in [`FillOrder`] order, then the world's [`AudioGain`] —
/// one when it carries none — scales the sum, which clamps to the unit
/// range. This is the whole of the audio thread's per-chunk work, and a
/// caller stepping a world inline calls it the same way.
///
/// A source must not change the world's set of sources while it runs: it
/// has a shared `&World`, so structural work goes through
/// [`World::queue`] and is applied by the world's own driver between fills.
pub fn dispatch_fill(world: &World, out: &mut [f32], sample_rate: u32, channels: u16) {
    out.fill(0.0);
    let mut sources: Vec<(FillOrder, u64, Entity)> = world
        .query::<&OnFill>()
        .map(|(entity, fill)| (fill.order(), fill.mount_index(), entity))
        .collect();
    sources.sort_unstable_by_key(|(order, index, _)| (*order, *index));
    let mut chunk = AudioChunk {
        out,
        sample_rate,
        channels,
    };
    for (_, _, entity) in sources {
        world
            .with_mut::<OnFill, _>(entity, |fill| fill.run(world, entity, &mut chunk))
            .expect("the source entity exists");
    }
    let gain = world
        .query::<&AudioGain>()
        .next()
        .map(|(_, gain)| gain.0)
        .unwrap_or(1.0);
    for sample in chunk.out.iter_mut() {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
}

/// The format the audio world's output stream opened at.
///
/// A source that renders at a fixed rate — a synthesizer, a sampler — is
/// built at the rate the device actually uses, and that is not known until
/// the stream opens. The audio thread publishes this as soon as it does, so
/// the job that spawns the world's [`OnFill`] sources can be sent then,
/// like any other work for that world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    /// The stream's sample rate, in hertz.
    pub sample_rate: u32,
    /// The stream's channel count.
    pub channels: u16,
}

/// The audio world's output-format mailbox, carried on the proxy entity the
/// host spawned the world through.
#[derive(Clone, Debug)]
pub struct AudioFormatInbox(pub SnapshotReader<AudioFormat>);

/// Whether the audio world can sound, published by the audio thread once
/// its output is open and the world carries at least one [`OnFill`] source.
///
/// Impacts that happen while the world is still loading its soundfont
/// must not queue as note jobs: they would all fire the instant the
/// synth appears, a burst instead of the spread of hits that made them.
/// Consumers read this and drop the sounds that belong to the silent
/// past.
#[derive(Clone, Copy, Debug)]
pub struct AudioReady(pub bool);

/// The audio world's readiness mailbox, carried on the proxy entity the
/// host spawned the world through.
#[derive(Clone, Debug)]
pub struct AudioReadyInbox(pub SnapshotReader<AudioReady>);
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

/// Spawn the audio world driven by `R`: on its own thread
/// ([`Threaded`](crate::world::Threaded)), or stepped by the host
/// ([`Stepped`](crate::world::Stepped)).
///
/// The world's entity carries the command channel, [`AudioFormatInbox`] and
/// [`AudioReadyInbox`]. A self-driven world opens the default output device
/// on its first tick and keeps it open, publishing its format once it
/// knows it; until then, and whenever no device exists, the world runs
/// silently. A stepped world opens no device either — the host pulls its
/// fills with [`dispatch_fill`] — unless its config asks for the browser's,
/// which a page drives with no thread at all; without one its format inbox
/// stays empty. Either way the world mixes whatever [`OnFill`] sources a
/// caller has since queued into it, and a world nobody has installed stays
/// silent.
///
/// Both runners share one tick body, so a stepped world opens its output and
/// fills its ring exactly as a threaded one does; the only difference is
/// that its host calls [`crate::world::step_stepped_world`] once per frame
/// instead of a thread pacing itself.
///
/// # Panics
///
/// Panics when `buffer_frames` cannot hold two fill chunks, or when
/// `command_capacity` is zero.
pub fn spawn_audio_world_with<R: WorldRunner>(
    host: &mut World,
    config: AudioConfig,
) -> WorldHandle {
    // The filler writes in chunks of FILL_FRAMES frames, so a ring smaller
    // than one chunk would never see room and the stream would stay silent
    // forever. Two chunks leave room to top the ring up between wakeups.
    assert!(
        config.buffer_frames >= FILL_FRAMES * 2,
        "buffer_frames must hold at least two fill chunks of {FILL_FRAMES} frames"
    );
    let mut output: Option<OpenOutput> = None;
    // Whether opening an output has been attempted: a self-driven world
    // keeps retrying until a device appears, while a browser's context
    // either opens or never will.
    let mut attempted = false;
    let mut scratch: Vec<f32> = Vec::new();
    let (format_writer, format_reader) = snapshot_pair::<AudioFormat>();
    let (ready_writer, ready_reader) = snapshot_pair::<AudioReady>();
    // The readiness flips only when the output and the sources appear or
    // vanish, so the mailbox sees one publish per transition.
    let mut ready = false;
    // The pacing clock follows the ring: it is created once the stream's
    // sample rate is known and then keeps its own cadence, so a slow tick
    // does not push every later wakeup out.
    let mut clock: Option<FixedClock> = None;
    R::spawn(
        host,
        "audio",
        config.command_capacity,
        (
            AudioFormatInbox(format_reader),
            AudioReadyInbox(ready_reader),
        ),
        |_| (),
        move |world, control, ()| {
            if control.stop_requested() {
                return false;
            }
            // A world that owns an output device opens it on its first
            // tick. A stepped world owns one only when its config asks for
            // the browser's: the host pulls the fills of every other
            // stepped world by hand.
            if opens_output(&config, control)
                && output.is_none()
                && (!attempted || control.self_driven())
            {
                attempted = true;
                match open_output(&config) {
                    Some(opened) => {
                        log::info!(
                            "the audio output opened at {} Hz, {} channels",
                            opened.sample_rate,
                            opened.channels
                        );
                        // Published once, before the first fill: whoever
                        // waits on it builds their sources at the rate the
                        // device really uses.
                        format_writer.publish(AudioFormat {
                            sample_rate: opened.sample_rate,
                            channels: opened.channels,
                        });
                        // Half a chunk of pacing: the ring stays ahead of
                        // the callback without the filler spinning.
                        let pace = FILL_FRAMES as f64 / f64::from(opened.sample_rate) * 0.5;
                        clock = Some(FixedClock::new(Duration::from_secs_f64(pace)));
                        output = Some(opened);
                    }
                    None => {
                        log::warn!("no audio output device is available yet");
                    }
                }
            }
            // A source takes time to build — a soundfont is parsed on this
            // thread — so a device may be open for seconds before the world
            // carries one. Publish the transition, not the state, so the
            // mailbox stays quiet.
            // A stepped world that asked for no device is open by
            // definition: its host pulls the fills. One that asked for a
            // browser's context sounds only once that context exists.
            let open = output.is_some() || (!control.self_driven() && !config.stepped_device);
            let sounding = open && world.query::<&OnFill>().next().is_some();
            if sounding != ready {
                ready = sounding;
                ready_writer.publish(AudioReady(sounding));
                if sounding {
                    log::info!("the audio world can sound");
                } else {
                    log::warn!("the audio world cannot sound");
                }
            }
            if let Some(output) = output.as_mut() {
                let chunk = FILL_FRAMES * usize::from(output.channels);
                while output.producer.vacant_len() >= chunk {
                    scratch.resize(chunk, 0.0);
                    dispatch_fill(world, &mut scratch, output.sample_rate, output.channels);
                    output.producer.push_slice(&scratch);
                }
                if let Some(clock) = clock.as_mut() {
                    clock.wait(control);
                } else {
                    control.wait_timeout(Duration::from_secs(1));
                }
            } else if control.self_driven() {
                // A self-driven world with no device yet waits for one; a
                // browser's context that failed to open is not worth
                // retrying every frame, so a stepped world just returns.
                control.wait_timeout(Duration::from_secs(1));
            }
            true
        },
    )
}

/// Whether a world driven this way owns an output device.
///
/// A self-driven world always does. A world the host steps does only when
/// its config asks for the browser's output: a Web Audio context is driven
/// by the page, so it needs no thread of its own to live on.
fn opens_output(config: &AudioConfig, control: &ThreadControl) -> bool {
    control.self_driven() || config.stepped_device
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
