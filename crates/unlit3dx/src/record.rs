//! Recording the mix an audio world fills into its output buffer.
//!
//! A recording is an [`OnAudioFill`] source ordered after every other one, so it
//! sees the samples the whole world contributed. The samples it keeps are the
//! ones the output stream would receive: the world's [`AudioGain`] scales the
//! sum and it clamps to the unit range, exactly as [`dispatch_audio_fill`] finishes a
//! fill.
//!
//! [`dispatch_audio_fill`]: crate::audio::dispatch_audio_fill
//!
//! The recorder learns the format from the first fill it sees rather than being
//! told one: an audio world publishes its rate only once a device is open, and
//! a world that opens none fills at the rate its sources were built for. A
//! recording stops at `max_seconds` of audio and keeps what it captured, so an
//! unattended recording cannot grow without bound. [`write_wav`] turns the
//! captured samples into a RIFF/WAVE file.

use std::sync::{Arc, Mutex};

use unlit_ecs::{Entity, World};

use crate::audio::{AudioChunk, AudioFillOrder, AudioFormat, AudioGain, OnAudioFill};
use crate::sync::lock;

/// A recording in progress on a world.
///
/// The handle owns the shared buffer the [`OnAudioFill`] source writes into. Keep it
/// around — as a component of the world, for instance — and hand it back to
/// [`RecordingHandle::stop`] or [`RecordingHandle::cancel`] to finish.
#[derive(Clone, Debug)]
pub(crate) struct RecordingHandle {
    entity: Entity,
    state: Arc<Mutex<RecordingState>>,
}

/// What a recording has captured so far.
#[derive(Debug)]
struct RecordingState {
    /// The format of the first fill the recording saw, which every later fill
    /// is taken to share.
    format: Option<AudioFormat>,
    samples: Vec<f32>,
    /// How many samples the recording keeps before it stops capturing.
    max_samples: usize,
    /// How many seconds of audio the recording keeps at most.
    max_seconds: f32,
}

/// The samples a finished recording captured.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordedAudio {
    /// The format the recording captured at.
    pub(crate) format: AudioFormat,
    /// The interleaved samples, in the format's channel order.
    pub(crate) samples: Vec<f32>,
}

impl RecordedAudio {
    /// How many frames were captured.
    #[must_use]
    pub(crate) fn frame_count(&self) -> usize {
        self.samples.len() / usize::from(self.format.channels).max(1)
    }

    /// How many seconds of audio were captured.
    #[must_use]
    pub(crate) fn seconds(&self) -> f64 {
        self.frame_count() as f64 / f64::from(self.format.sample_rate.max(1))
    }
}

/// Start recording everything `world` fills, for at most `max_seconds`.
///
/// The recorder runs after every other [`OnAudioFill`] source and keeps the mix after
/// the world's [`AudioGain`] and the unit-range clamp, so the result is what
/// the output stream receives. No call fills the world: whichever driver
/// usually does (the audio thread, [`dispatch_audio_fill`] directly, a stepped world)
/// keeps doing it, and the recording grows as it goes.
///
/// [`dispatch_audio_fill`]: crate::audio::dispatch_audio_fill
pub(crate) fn start_recording(world: &mut World, max_seconds: f32) -> RecordingHandle {
    let state = Arc::new(Mutex::new(RecordingState {
        format: None,
        samples: Vec::new(),
        // Nothing is kept until a fill names the format the limit is measured
        // in; the first fill replaces this with the real bound.
        max_samples: usize::MAX,
        max_seconds: max_seconds.max(0.0),
    }));
    let recorder = Arc::clone(&state);
    let entity = world.root_scope_mut().spawn((OnAudioFill::new(
        AudioFillOrder(i32::MAX),
        move |world: &World, _entity: Entity, chunk: &mut AudioChunk| {
            let mut state = lock(&recorder);
            if state.format.is_none() {
                let format = AudioFormat {
                    sample_rate: chunk.sample_rate,
                    channels: chunk.channels,
                };
                let frames =
                    (f64::from(format.sample_rate) * f64::from(state.max_seconds)) as usize;
                state.max_samples = frames.saturating_mul(usize::from(format.channels).max(1));
                state.format = Some(format);
            }
            let gain = world
                .root_scope()
                .query::<&AudioGain>()
                .next()
                .map(|(_, gain)| gain.0)
                .unwrap_or(1.0);
            for &sample in chunk.out.iter() {
                if state.samples.len() >= state.max_samples {
                    break;
                }
                state.samples.push((sample * gain).clamp(-1.0, 1.0));
            }
        },
    ),));
    RecordingHandle { entity, state }
}

/// Write interleaved samples as a 16-bit PCM RIFF/WAVE file.
///
/// Each sample clamps to the unit range and then scales to `i16`, the same
/// conversion the soundfont pipeline uses for its own files.
pub(crate) fn write_wav(format: AudioFormat, samples: &[f32]) -> Vec<u8> {
    let channels = format.channels.max(1);
    let sample_rate = format.sample_rate.max(1);
    let block_align = channels * 2;
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        bytes.extend_from_slice(&scaled.to_le_bytes());
    }
    bytes
}

impl RecordingHandle {
    /// The format the recording captured at, once it has seen a fill.
    #[must_use]
    pub(crate) fn format(&self) -> Option<AudioFormat> {
        lock(&self.state).format
    }

    /// How many frames the recording has captured.
    #[must_use]
    pub(crate) fn frame_count(&self) -> usize {
        let state = lock(&self.state);
        let channels = state
            .format
            .map_or(1, |format| usize::from(format.channels).max(1));
        state.samples.len() / channels
    }

    /// How many seconds of audio the recording has captured.
    #[must_use]
    pub(crate) fn seconds(&self) -> f64 {
        let state = lock(&self.state);
        match state.format {
            Some(format) => {
                let channels = usize::from(format.channels).max(1);
                (state.samples.len() / channels) as f64 / f64::from(format.sample_rate.max(1))
            }
            None => 0.0,
        }
    }

    /// Whether the recording reached its limit and stopped capturing.
    ///
    /// A recording that has seen no fill yet is never full: its limit is not
    /// known until the format is.
    #[must_use]
    pub(crate) fn is_full(&self) -> bool {
        let state = lock(&self.state);
        state.format.is_some() && state.samples.len() >= state.max_samples
    }

    /// Stop recording, despawn the source and take the captured samples.
    ///
    /// A recording that saw no fill has no format to write, so there is
    /// nothing to hand back.
    #[must_use]
    pub(crate) fn stop(self, world: &mut World) -> Option<RecordedAudio> {
        world.root_scope_mut().despawn(self.entity);
        let mut state = lock(&self.state);
        let format = state.format?;
        Some(RecordedAudio {
            format,
            samples: std::mem::take(&mut state.samples),
        })
    }

    /// Stop recording, despawn the source and discard the captured samples.
    pub(crate) fn cancel(self, world: &mut World) {
        world.root_scope_mut().despawn(self.entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dispatch_audio_fill;

    fn constant_source(world: &mut World, level: f32) -> Entity {
        world.root_scope_mut().spawn((OnAudioFill::new(
            AudioFillOrder(0),
            move |_world, _entity, chunk: &mut AudioChunk| {
                chunk.out.fill(level);
            },
        ),))
    }

    #[test]
    fn a_recording_captures_the_mixed_samples() {
        let mut world = World::new();
        constant_source(&mut world, 0.75);
        world.root_scope_mut().spawn((AudioGain(2.0),));
        let format = AudioFormat {
            sample_rate: 8,
            channels: 1,
        };
        let handle = start_recording(&mut world, 1.0);
        assert_eq!(handle.format(), None, "nothing fills it yet");
        let mut out = [0.0f32; 4];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);
        assert_eq!(handle.frame_count(), 4);
        assert!(!handle.is_full());
        assert_eq!(handle.format(), Some(format));
        let recorded = handle.stop(&mut world).expect("the recording saw a fill");
        assert_eq!(recorded.samples, vec![1.0; 4]);
        assert_eq!(recorded.frame_count(), 4);
        assert_eq!(recorded.seconds(), 0.5);
    }

    #[test]
    fn a_recording_stops_at_its_limit() {
        let mut world = World::new();
        constant_source(&mut world, 0.0);
        let format = AudioFormat {
            sample_rate: 4,
            channels: 1,
        };
        let handle = start_recording(&mut world, 0.5);
        let mut out = [0.0f32; 8];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);
        assert_eq!(handle.frame_count(), 2);
        assert!(handle.is_full());
        handle.cancel(&mut world);
    }

    #[test]
    fn a_recording_nobody_filled_hands_back_nothing() {
        let mut world = World::new();
        let handle = start_recording(&mut world, 1.0);
        assert!(!handle.is_full());
        assert_eq!(handle.seconds(), 0.0);
        assert_eq!(handle.stop(&mut world), None);
    }

    #[test]
    fn a_wav_file_describes_its_samples() {
        let format = AudioFormat {
            sample_rate: 8000,
            channels: 2,
        };
        let bytes = write_wav(format, &[0.0, -1.0, 1.0, 2.0]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            44 + 8 - 8
        );
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 8000);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(i16::from_le_bytes(bytes[44..46].try_into().unwrap()), 0);
        assert_eq!(
            i16::from_le_bytes(bytes[46..48].try_into().unwrap()),
            -i16::MAX
        );
        assert_eq!(
            i16::from_le_bytes(bytes[50..52].try_into().unwrap()),
            i16::MAX
        );
    }
}
