//! The audio world: a callback-driven output stream fed by a ring.
//!
//! The audio thread owns the stream and a lock-free ring buffer; the
//! operating system's callback pops whatever is newest and silences the
//! rest, so a late simulation degrades to silence instead of stuttering.
//! What sound plays is decided by the world's [`OnAudioFill`] components: the
//! thread runs them once per filled chunk and sums their samples into the
//! ring. The output's format is not known until its stream opens, so the
//! thread spawns it as an [`AudioFormat`] component, marks the world with
//! [`AudioOutputOpen`], and a caller subscribes to either — the format to
//! build its sources at the rate the device really uses, the marker to know
//! when the world can sound at all.

use std::cell::RefCell;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use unlit_ecs::{Entity, World, bundle};
use unlit3d::frame::{Frame, dispatch_frame};

use crate::handle::WorldHandle;
use crate::job::{JobSender, SendError};
use crate::thread::FixedClock;
use crate::world::WorldRunner;

/// How many frames the filler generates per pass, the unit the pacing and
/// the ring's headroom are measured in.
const AUDIO_FILL_FRAMES: usize = 256;

/// How an audio world gets its output, whichever way it is driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioOutputMode {
    /// No device: the host pulls the fills with [`dispatch_audio_fill`].
    /// The windowed entry pulls nothing, so a window run stays silent under
    /// this mode unless the world's host pulls it some other way.
    Pulled,
    /// Open nothing until an [`AudioOutputRequest`] lands, then retry
    /// every tick until a device appears. A page asks from the handler of
    /// the user's click: the tick that honours the request runs inside the
    /// browser's sticky activation, so the context it opens is allowed to
    /// start. A browser cannot report an opening failure, so the world
    /// keeps trying rather than giving up after one attempt. A desktop
    /// world asks the same way when its caller decides when sound may
    /// start, so one mode covers both drivers.
    OnRequest,
}

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
    /// How the world gets its output, whichever way it is driven.
    ///
    /// The default pulls none: a world nobody asked to sound stays silent,
    /// whatever runs it. A page asks for the browser's output from a
    /// user-gesture handler with [`request_audio_output`], because that is
    /// where the browser allows a context to start.
    pub output: AudioOutputMode,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            buffer_frames: 4096,
            preferred_sample_rate: 48000,
            command_capacity: 256,
            output: AudioOutputMode::Pulled,
        }
    }
}

/// Where a fill behaviour runs among the world's other sources.
///
/// Lower runs first. Sources at the same order have no order between them: the
/// dispatcher promises nothing about which of them adds its samples first, so a
/// source whose contribution another has to follow states a different order.
/// A source that only adds its own samples does not care where it lands in the
/// band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioFillOrder(pub i32);

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

/// The finished mix a post-fill behaviour observes: every [`OnAudioFill`]
/// source has added its samples and the world's [`GlobalAudioGain`] has
/// scaled the sum, so these are the samples the output stream would receive.
pub struct AudioMix<'a> {
    /// The chunk's samples, interleaved frames.
    pub samples: &'a [f32],
    /// The output's sample rate, in hertz.
    pub sample_rate: u32,
    /// The output's channel count.
    pub channels: u16,
}

impl AudioMix<'_> {
    /// How many frames the mix holds.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

/// The callback a fill behaviour runs: it adds its source's samples to the
/// chunk it is handed.
type AudioFillCallback = Box<dyn FnMut(&World, Entity, &mut AudioChunk)>;

/// The state every fill behaviour shares: its callback and its order.
struct AudioFillSlot {
    callback: AudioFillCallback,
    order: AudioFillOrder,
}

/// A source of audio samples: a component the audio thread runs once per
/// filled chunk.
///
/// Spawn it on its own entity with [`ScopeMut::spawn`](unlit_ecs::ScopeMut::spawn), or
/// bundle it with the state it renders from — a synthesizer, say — and
/// spawn that entity directly. [`dispatch_audio_fill`] runs a world's sources and
/// mixes them.
pub struct OnAudioFill(AudioFillSlot);

impl OnAudioFill {
    /// Wrap `f` as an [`OnAudioFill`].
    ///
    /// `order` places it among the world's other sources; a source that
    /// only adds its own samples can keep [`AudioFillOrder::default`], since
    /// the sum does not depend on where it lands in the band.
    #[must_use]
    pub fn new(
        order: AudioFillOrder,
        f: impl FnMut(&World, Entity, &mut AudioChunk) + 'static,
    ) -> Self {
        Self(AudioFillSlot {
            callback: Box::new(f),
            order,
        })
    }

    /// The order this source runs in.
    #[must_use]
    pub fn order(&self) -> AudioFillOrder {
        self.0.order
    }

    /// Run this source for `entity`, adding its samples to `chunk`.
    fn run(&mut self, world: &World, entity: Entity, chunk: &mut AudioChunk) {
        (self.0.callback)(world, entity, chunk);
    }
}

/// The callback a post-fill behaviour runs: it observes the finished mix
/// without changing it.
type AudioPostFillCallback = Box<dyn FnMut(&World, Entity, &AudioMix)>;

/// The state every post-fill behaviour shares: its callback.
struct AudioPostFillSlot {
    callback: AudioPostFillCallback,
}

/// An observer of the finished mix: a component the audio thread runs once
/// per filled chunk, after every [`OnAudioFill`] source has added its
/// samples and the world's [`GlobalAudioGain`] has scaled and clamped the
/// sum.
///
/// The samples [`dispatch_audio_fill`] hands a post-fill behaviour are the
/// ones the output stream would receive, so a limiter, a level meter, or a
/// recorder mounts here rather than among the sources: it observes the mix
/// instead of repeating the dispatcher's gain and clamp itself. A post-fill
/// behaviour cannot change the mix — it sees a shared slice — and two of
/// them run in no stated order between themselves.
pub struct OnAudioPostFill(AudioPostFillSlot);

impl OnAudioPostFill {
    /// Wrap `f` as an [`OnAudioPostFill`].
    #[must_use]
    pub fn new(f: impl FnMut(&World, Entity, &AudioMix) + 'static) -> Self {
        Self(AudioPostFillSlot {
            callback: Box::new(f),
        })
    }

    /// Run this behaviour for `entity`, observing `mix`.
    fn run(&mut self, world: &World, entity: Entity, mix: &AudioMix) {
        (self.0.callback)(world, entity, mix);
    }
}

thread_local! {
    /// The source list [`dispatch_audio_fill`] reuses across fills.
    ///
    /// The initializer is already a const block. Clippy's `thread_local`
    /// analysis differs by target and reports a candidate anyway on Android,
    /// so the lint is silenced here rather than expected: an expectation goes
    /// unfulfilled on every target where it does not fire.
    #[allow(
        clippy::missing_const_for_thread_local,
        reason = "the initializer is already a const block"
    )]
    static SOURCES: RefCell<Vec<(AudioFillOrder, Entity)>> = const { RefCell::new(Vec::new()) };

    /// The post-fill list [`dispatch_audio_fill`] reuses across fills, with
    /// the same reasoning as `SOURCES` above.
    #[allow(
        clippy::missing_const_for_thread_local,
        reason = "the initializer is already a const block"
    )]
    static POST_SOURCES: RefCell<Vec<Entity>> = const { RefCell::new(Vec::new()) };
}

/// The factor the mixed samples scale by, read from the audio world.
///
/// The gain belongs to the output rather than to one source:
/// [`dispatch_audio_fill`] sums every source and scales the sum once, so adding a
/// source changes the mix without changing anyone's level. Soundfont presets
/// leave a lot of headroom, so a mix of a few quiet voices usually wants a
/// gain above one. Samples clamp to the unit range after the gain, so an
/// overdriven mix distorts rather than wraps. A world carrying no gain
/// passes its mix through unchanged.
///
/// A source runs before the gain is applied, so it sees the mix at the
/// sources' own levels. One that mirrors the output — a recorder does —
/// runs after it instead: an [`OnAudioPostFill`] behaviour sees the finished
/// mix, gain and clamp included, rather than scaling the buffer it observes
/// itself.
///
/// The gain is a singleton keyed by [`GlobalAudioGain`]: the mixture is
/// scaled once after every source summed, so two gains on one world would
/// leave the order between them unstated.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlobalAudioGain(pub f32);

/// Run every [`OnAudioFill`] in `world` once, mixing their samples into `out`.
///
/// `out` is interleaved frames and is zeroed first; each source adds its
/// contribution in [`AudioFillOrder`] order — the order is the only thing the
/// dispatcher states, so sources at the same order run in no stated order.
/// Then the world's [`GlobalAudioGain`] — one when it carries none — scales
/// the sum, which clamps to the unit range. This is the whole of the audio
/// thread's per-chunk work, and a caller stepping a world inline calls it the
/// same way.
///
/// Every [`OnAudioPostFill`] in `world` then observes the finished mix: its
/// samples are the ones the output stream would receive, gain and clamp
/// included, and a post-fill behaviour cannot change them.
///
/// A source runs with a shared `&World`, so a structural change it makes goes
/// through [`unlit_ecs::Scope::queue`]. The fill applies that queue itself once
/// every source has run, exactly as [`unlit3d::frame::dispatch_frame`] does for
/// a frame: a caller filling a world the host does not otherwise drive — a
/// recorder draining a stepped audio world between frames — still gets its
/// change committed.
pub fn dispatch_audio_fill(world: &mut World, out: &mut [f32], sample_rate: u32, channels: u16) {
    out.fill(0.0);
    // The list has the same shape every fill, so it is handed out and taken
    // back rather than allocated per chunk on the audio thread. Taking it also
    // keeps the slot free while a source runs.
    let mut sources = SOURCES.with_borrow_mut(std::mem::take);
    sources.clear();
    sources.extend(
        world
            .root_scope()
            .query::<(&OnAudioFill, Entity)>()
            .iter()
            .map(|(on_audio_fill, entity)| (on_audio_fill.order(), entity)),
    );
    sources.sort_unstable_by_key(|(order, _entity)| *order);
    let mut chunk = AudioChunk {
        out,
        sample_rate,
        channels,
    };
    for (_, entity) in &sources {
        if let Ok(mut fill) = world
            .root_scope()
            .try_query_one::<&mut OnAudioFill>(*entity)
        {
            fill.get().run(&*world, *entity, &mut chunk);
        }
    }
    SOURCES.with_borrow_mut(|slot| *slot = sources);
    // The query holds a borrow of the world, so it ends before the queue is
    // applied below.
    let gain = {
        let scope = world.root_scope();
        let gain = scope
            .singleton::<GlobalAudioGain>()
            .and_then(|entity| scope.try_query_one::<&GlobalAudioGain>(entity).ok());
        gain.map_or(1.0, |mut audio_gain| audio_gain.get().0)
    };
    for sample in chunk.out.iter_mut() {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
    // The mix is finished: its samples are what the output stream would
    // receive. The post-fill stage observes it and cannot change it, so a
    // recorder or a limiter mounts here rather than repeating the gain and
    // the clamp themselves.
    let mix = AudioMix {
        samples: &chunk.out[..],
        sample_rate,
        channels,
    };
    let mut posts = POST_SOURCES.with_borrow_mut(std::mem::take);
    posts.clear();
    posts.extend(
        world
            .root_scope()
            .query::<(&OnAudioPostFill, Entity)>()
            .iter()
            .map(|(_post, entity)| entity),
    );
    for entity in &posts {
        if let Ok(mut post) = world
            .root_scope()
            .try_query_one::<&mut OnAudioPostFill>(*entity)
        {
            post.get().run(&*world, *entity, &mix);
        }
    }
    POST_SOURCES.with_borrow_mut(|slot| *slot = posts);
    if let Err(error) = world.apply() {
        log::error!("a command queued by an audio source failed: {error}");
    }
}

/// The format the audio world's output stream opened at.
///
/// A source that renders at a fixed rate — a synthesizer, a sampler — is
/// built at the rate the device actually uses, and that is not known until
/// the stream opens. The audio thread spawns this component the moment it
/// does, so a subscriber can build the world's [`OnAudioFill`] sources at
/// that rate, like any other work for that world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    /// The stream's sample rate, in hertz.
    pub sample_rate: u32,
    /// The stream's channel count.
    pub channels: u16,
}

/// The marker the audio world carries once its output is open.
///
/// A world that opens a device gains it on the tick the stream comes up; a
/// world whose host pulls its fills carries it from its first tick, because
/// the host is the thing that would have opened a device. A subscriber reads
/// it to tell a world with somewhere to play from one still waiting for a
/// device, and combines it with whatever else it needs — the world carrying
/// an [`OnAudioFill`] source, say — to decide when to start playing.
#[derive(Clone, Copy, Debug)]
pub struct AudioOutputOpen;

/// The marker an [`AudioOutputMode::OnRequest`] audio world carries once a
/// [`request_audio_output`] job has reached it. It stays for the world's
/// life, so a subscriber can tell a world still waiting for its page's
/// first click from one that has been asked and is only missing a device.
#[derive(Clone, Copy, Debug)]
pub struct AudioOutputRequest;

/// Ask `sender`'s audio world to open its output device.
///
/// The request travels as a job: it lands in the world's queue and is
/// honoured on the tick that drains it, which on the web is the same frame
/// the user's click ran in — inside the browser's sticky activation, so
/// the context that tick opens is allowed to start. Asking a world that
/// does not open on request costs nothing; asking twice is harmless.
///
/// # Errors
///
/// Errors when the queue is full or the world has stopped, like any job.
pub fn request_audio_output(sender: &JobSender) -> Result<(), SendError> {
    sender.send(Box::new(|world: &mut World| {
        if world
            .root_scope()
            .query::<&AudioOutputRequest>()
            .iter()
            .next()
            .is_none()
        {
            world.root_scope_mut().spawn(bundle![AudioOutputRequest]);
        }
    }))
}

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
/// The world's entity carries the command channel. Whether a device is
/// opened follows [`AudioOutputMode`] for either runner: [`AudioOutputMode::Pulled`]
/// opens nothing — the host pulls its fills with [`dispatch_audio_fill`] — and
/// [`AudioOutputMode::OnRequest`]
/// waits for a [`request_audio_output`] job, which a page sends from a
/// user-gesture handler because that is where a browser allows a context to
/// start. Until the device comes up the world gains neither [`AudioFormat`]
/// nor [`AudioOutputOpen`], and whenever no device exists it runs silently.
/// Either way the world mixes whatever [`OnAudioFill`] sources a caller has
/// since queued into it, and a world nobody has installed stays silent.
///
/// Nothing is published until someone subscribes: a subscriber's collector
/// runs once per tick, after the output's state has been updated.
///
/// Both runners share one tick body, so a stepped world honours
/// [`AudioOutputMode`] exactly as a threaded one does; the only difference is
/// that its host calls [`crate::world::step_stepped_world`] once per frame
/// instead of a thread pacing itself. A Pulled one therefore sounds only
/// while its host keeps pulling, whatever drives it.
///
/// # Panics
///
/// Panics when `buffer_frames` cannot hold two fill chunks.
pub fn spawn_audio_world_with<R: WorldRunner>(
    host: &mut World,
    config: AudioConfig,
) -> WorldHandle {
    // The filler writes in chunks of AUDIO_FILL_FRAMES frames, so a ring smaller
    // than one chunk would never see room and the stream would stay silent
    // forever. Two chunks leave room to top the ring up between wakeups.
    assert!(
        config.buffer_frames >= AUDIO_FILL_FRAMES * 2,
        "buffer_frames must hold at least two fill chunks of {AUDIO_FILL_FRAMES} frames"
    );
    let mut output: Option<OpenOutput> = None;
    // Whether an [`AudioOutputRequest`] has landed: once true, a world in
    // [`AudioOutputMode::OnRequest`] keeps trying to open a device every
    // tick, because a browser cannot report an opening failure to retry
    // against.
    let mut requested = false;
    let mut scratch: Vec<f32> = Vec::new();
    // The sounding state flips only when the output and the sources appear
    // or vanish, so the log carries one line per transition.
    let mut ready = false;
    // The pacing clock follows the ring: it is created once the stream's
    // sample rate is known and then keeps its own cadence, so a slow tick
    // does not push every later wakeup out.
    let mut clock: Option<FixedClock> = None;
    // A threaded world's `World` is created on and owned by its thread, so
    // its host cannot reach it to call [`dispatch_audio_fill`]: a Pulled
    // config leaves it silent for good. The default config is Pulled, so a
    // caller that wanted sound is told here instead of chasing a missing device.
    if R::SELF_DRIVEN && config.output == AudioOutputMode::Pulled {
        log::warn!(
            "the audio world is self-driven in the Pulled mode, so nothing \
             fills it; the host calls dispatch_audio_fill, or the config asks \
             for OnRequest"
        );
    }
    // The world's own frame ordinal. The tick is the world's frame: one fill
    // of the ring is one turn, so the pacing a reader sees is the rate the
    // device really consumes samples at.
    let mut frame_index = 0_u32;
    R::spawn(
        host,
        "audio",
        config.command_capacity,
        |_| (),
        move |world, control, (), delta_seconds| {
            if control.stop_requested() {
                return false;
            }
            // Whether this tick wants a device open: [`AudioOutputMode::Pulled`] never,
            // [`AudioOutputMode::OnRequest`] once its page has asked. The config speaks for
            // every world, self-driven or stepped alike.
            if let AudioOutputMode::OnRequest = config.output
                && world
                    .root_scope()
                    .query::<&AudioOutputRequest>()
                    .iter()
                    .next()
                    .is_some()
            {
                requested = true;
            }
            let wants_output = match config.output {
                AudioOutputMode::Pulled => false,
                AudioOutputMode::OnRequest => requested,
            };
            if wants_output && output.is_none() {
                match open_output(&config) {
                    Some(opened) => {
                        log::info!(
                            "the audio output opened at {} Hz, {} channels",
                            opened.sample_rate,
                            opened.channels
                        );
                        // Spawned once, before the first fill: whoever
                        // waits on it builds their sources at the rate the
                        // device really uses.
                        world.root_scope_mut().spawn(bundle![AudioFormat {
                            sample_rate: opened.sample_rate,
                            channels: opened.channels,
                        },]);
                        // Half a chunk of pacing: the ring stays ahead of
                        // the callback without the filler spinning.
                        let pace = AUDIO_FILL_FRAMES as f64 / f64::from(opened.sample_rate) * 0.5;
                        clock = Some(FixedClock::new(Duration::from_secs_f64(pace)));
                        output = Some(opened);
                    }
                    None => {
                        // A page's request can arrive before a device the
                        // system is ready to grant exists; retrying is the
                        // world's whole strategy, so this stays quiet.
                        log::debug!("no audio output device is available yet");
                    }
                }
            }
            // A source takes time to build — a soundfont is parsed on this
            // thread — so a device may be open for seconds before the world
            // carries one. Log the transition, not the state, so the log
            // stays quiet. A Pulled world has no transition of its own to
            // log: whether it sounds is whether its host pulls, which the
            // host already knows. Only an OnRequest world reports one.
            let open = output.is_some() || config.output == AudioOutputMode::Pulled;
            // The marker goes on the first tick the output is open, and the
            // world keeps it: an entity's components are fixed once spawned.
            if open
                && world
                    .root_scope()
                    .query::<&AudioOutputOpen>()
                    .iter()
                    .next()
                    .is_none()
            {
                world.root_scope_mut().spawn(bundle![AudioOutputOpen]);
            }
            if let AudioOutputMode::OnRequest = config.output {
                let sounding = output.is_some()
                    && world
                        .root_scope()
                        .query::<&OnAudioFill>()
                        .iter()
                        .next()
                        .is_some();
                if sounding != ready {
                    ready = sounding;
                    if sounding {
                        log::info!("the audio world can sound");
                    } else {
                        log::warn!("the audio world cannot sound");
                    }
                }
            }
            if let Some(output) = output.as_mut() {
                let chunk = AUDIO_FILL_FRAMES * usize::from(output.channels);
                while output.producer.vacant_len() >= chunk {
                    scratch.resize(chunk, 0.0);
                    dispatch_audio_fill(world, &mut scratch, output.sample_rate, output.channels);
                    output.producer.push_slice(&scratch);
                }
                if let Some(clock) = clock.as_mut() {
                    clock.wait(control);
                } else {
                    control.wait_timeout(Duration::from_secs(1));
                }
            } else {
                // Without a device the world waits for one. A stepped world's
                // wait returns at once, so it retries at its host's pace.
                control.wait_timeout(Duration::from_secs(1));
            }
            // The tick is this world's frame: sources that pace themselves by
            // it read a clock that matches the fills.
            let mut frame = Frame {
                delta_time: delta_seconds,
                index: frame_index,
            };
            dispatch_frame(world, &mut frame);
            frame_index = frame.index.wrapping_add(1);
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
