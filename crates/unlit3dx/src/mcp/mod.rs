//! The MCP tools an audio world answers: recording the mix it fills.
//!
//! Each tool routes its work to the world it names through [command], exactly
//! as the rendering tools do. The tools register themselves through
//! [ToolSource], so a server built from [unlit_ecs] carries them without
//! depending on this crate.

mod bridge;

pub use bridge::serve_mcp;

use unlit_ecs::mcp::serde_json::{self, Value};
use unlit_ecs::mcp::{
    CommandSender, McpServer, ToolResult, ToolRouter, ToolSource, WorldArg, command, tool,
};
use unlit_ecs::{Entity, World, bundle};

use crate::record::{RecordingHandle, start_recording, write_wav};

/// How long a recording lasts when its caller names no limit.
const DEFAULT_RECORDING_SECONDS: f32 = 60.0;

/// The recording in progress on a world, kept as a component so it lives on
/// that world's own thread.
struct ActiveRecording(RecordingHandle);

unlit_ecs::mcp::inventory::submit! {
    ToolSource { extend: register }
}

/// Register every audio tool on `router`.
fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool(
        "start_audio_recording",
        "Start recording the mix an audio world fills. The recording runs until it is stopped or cancelled, or until it reaches its length limit; query it with audio_recording_status.",
        start_audio_recording,
    ));
    router.add_route(tool(
        "stop_audio_recording",
        "Stop an audio world's recording and write what it captured as a 16-bit PCM WAV file.",
        stop_audio_recording,
    ));
    router.add_route(tool(
        "cancel_audio_recording",
        "Stop an audio world's recording and discard what it captured.",
        cancel_audio_recording,
    ));
    router.add_route(tool(
        "audio_recording_status",
        "Report what an audio world's recording has captured so far, or that none is in progress.",
        audio_recording_status,
    ));
}

/// Start recording the mix of the audio world the arguments name.
async fn start_audio_recording(commands: CommandSender, args: StartRecording) -> ToolResult {
    command(&commands, args.world, move |world| {
        start(world, args.duration_seconds)
    })
    .await
}

/// Finish a recording and write its file.
async fn stop_audio_recording(commands: CommandSender, args: StopRecording) -> ToolResult {
    command(&commands, args.world, move |world| {
        stop(world, &args.directory, args.file_name.as_deref())
    })
    .await
}

/// Discard the recording in progress.
async fn cancel_audio_recording(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, cancel).await
}

/// Report what the recording in progress has captured.
async fn audio_recording_status(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, status).await
}

/// Start a recording, unless one is already running.
fn start(world: &mut World, duration_seconds: Option<f32>) -> Result<Value, String> {
    if world
        .root_scope()
        .query::<&ActiveRecording>()
        .next()
        .is_some()
    {
        return Err("a recording is already in progress on this world".to_owned());
    }
    let seconds = duration_seconds.unwrap_or(DEFAULT_RECORDING_SECONDS);
    let handle = start_recording(world, seconds);
    let report = Started {
        duration_seconds: seconds,
    };
    world
        .root_scope_mut()
        .spawn(bundle!(ActiveRecording(handle),));
    serde_json::to_value(report).map_err(|error| error.to_string())
}

/// Stop the recording, write its samples and report the file.
fn stop(world: &mut World, directory: &str, file_name: Option<&str>) -> Result<Value, String> {
    let handle = take(world)?;
    let Some(audio) = handle.stop(world) else {
        return Err(
            "the recording captured no fill; check that the world has sources to play".to_owned(),
        );
    };
    let bytes = write_wav(audio.format, &audio.samples);
    let name = unlit_ecs::mcp::saved_name(file_name, "audio", "wav");
    let saved = unlit_ecs::mcp::write_file(directory, &name, &bytes)?;
    let report = Recorded {
        path: saved.path,
        size: saved.size,
        frames: audio.frame_count(),
        seconds: audio.seconds(),
        sample_rate: audio.format.sample_rate,
        channels: audio.format.channels,
    };
    serde_json::to_value(report).map_err(|error| error.to_string())
}

/// Stop the recording and throw its samples away.
fn cancel(world: &mut World) -> Result<Value, String> {
    take(world)?.cancel(world);
    Ok(serde_json::json!({ "cancelled": true }))
}

/// Report how far the recording in progress has come.
fn status(world: &mut World) -> Result<Value, String> {
    let Some(recording) = world
        .root_scope()
        .query::<&ActiveRecording>()
        .next()
        .map(|active_recording| active_recording.0.clone())
    else {
        return Ok(serde_json::json!({ "recording": false }));
    };
    let format = recording.format();
    let report = InProgress {
        recording: true,
        sample_rate: format.map(|format| format.sample_rate),
        channels: format.map(|format| format.channels),
        frames: recording.frame_count(),
        seconds: recording.seconds(),
        full: recording.is_full(),
    };
    serde_json::to_value(report).map_err(|error| error.to_string())
}

/// Take the recording in progress out of the world.
fn take(world: &mut World) -> Result<RecordingHandle, String> {
    let entity = world
        .root_scope()
        .query::<(&ActiveRecording, Entity)>()
        .next()
        .map(|(_active_recording, entity)| entity)
        .ok_or_else(|| "no recording is in progress on this world".to_owned())?;
    let handle = world
        .root_scope()
        .query_one::<&ActiveRecording>(entity)
        .expect("the recording entity exists")
        .0
        .clone();
    world.root_scope_mut().despawn(entity);
    Ok(handle)
}

/// Arguments for starting a recording.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct StartRecording {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: unlit_ecs::mcp::WorldId,
    /// How many seconds of audio to record at most. The recording stops and
    /// keeps what it captured when it reaches the limit.
    #[serde(default)]
    pub duration_seconds: Option<f32>,
}

/// Arguments for finishing a recording.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct StopRecording {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: unlit_ecs::mcp::WorldId,
    /// The directory to write the file into; created when it does not exist.
    pub directory: String,
    /// The file name to write; it is given a `.wav` extension when it carries
    /// none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// A recording that has just started.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct Started {
    /// How many seconds the recording will capture at most.
    duration_seconds: f32,
}

/// What a recording in progress has captured.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct InProgress {
    /// Whether a recording is in progress.
    recording: bool,
    /// The rate the recording captures at, once a fill has named it.
    sample_rate: Option<u32>,
    /// How many channels the recording captures, once a fill has named them.
    channels: Option<u16>,
    /// How many frames the recording has captured.
    frames: usize,
    /// How many seconds of audio the recording has captured.
    seconds: f64,
    /// Whether the recording reached its limit and stopped capturing.
    full: bool,
}

/// A finished recording, written to a file.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct Recorded {
    /// The absolute path of the file that was written.
    path: String,
    /// How many bytes were written.
    size: usize,
    /// How many frames the file holds.
    frames: usize,
    /// How many seconds of audio the file holds.
    seconds: f64,
    /// The rate the recording captured at.
    sample_rate: u32,
    /// How many channels the recording captured.
    channels: u16,
}

#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;
    use crate::audio::{
        AudioChunk, AudioFillOrder, AudioFormat, AudioGain, OnAudioFill, dispatch_audio_fill,
    };

    /// A world with one source that fills a constant level, plus the format
    /// its fills carry.
    fn audio_world(level: f32) -> (World, AudioFormat) {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle!(AudioGain(1.0),));
        world.root_scope_mut().spawn(bundle!(OnAudioFill::new(
            AudioFillOrder(0),
            move |_world, _entity, chunk: &mut AudioChunk| chunk.out.fill(level),
        ),));
        let format = AudioFormat {
            sample_rate: 8,
            channels: 1,
        };
        (world, format)
    }

    #[test]
    fn a_recording_started_and_stopped_writes_its_mix() {
        let (mut world, format) = audio_world(0.5);
        let started = start(&mut world, Some(1.0)).expect("the recording starts");
        assert_eq!(started["duration_seconds"], 1.0);

        let mut out = [0.0f32; 4];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);
        let status = status(&mut world).expect("a recording reports");
        assert_eq!(status["recording"], true);
        assert_eq!(status["frames"], 4);
        assert_eq!(status["sample_rate"], 8);

        let directory = std::env::temp_dir().join(format!("unlit3dx-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let recorded = stop(&mut world, &directory.to_string_lossy(), Some("take.wav"))
            .expect("the recording is written");
        assert_eq!(recorded["frames"], 4);
        assert_eq!(recorded["channels"], 1);
        assert_eq!(recorded["sample_rate"], 8);
        let path = recorded["path"].as_str().expect("the file has a path");
        assert!(
            path.ends_with("take.wav"),
            "the file keeps its name: {path}"
        );
        let bytes = std::fs::read(path).expect("the file is written");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 8);
    }

    #[test]
    fn a_second_recording_is_refused_until_the_first_ends() {
        let (mut world, _) = audio_world(0.0);
        start(&mut world, None).expect("the first recording starts");
        let refused = start(&mut world, None).expect_err("the second is refused");
        assert!(refused.contains("already in progress"), "{refused}");
        cancel(&mut world).expect("the recording is cancelled");
        start(&mut world, None).expect("a fresh recording starts");
    }

    #[test]
    fn a_cancelled_or_finished_recording_reports_nothing() {
        let (mut world, _) = audio_world(0.0);
        assert!(status(&mut world).expect("no recording is fine")["recording"] == false);
        let missing = cancel(&mut world).expect_err("there is nothing to cancel");
        assert!(missing.contains("no recording"), "{missing}");
    }

    #[test]
    fn a_recording_that_saw_no_fill_is_reported_when_stopped() {
        let (mut world, _) = audio_world(0.0);
        start(&mut world, Some(1.0)).expect("the recording starts");
        let directory =
            std::env::temp_dir().join(format!("unlit3dx-mcp-empty-{}", std::process::id()));
        let failed = stop(&mut world, &directory.to_string_lossy(), None)
            .expect_err("a recording with no fill cannot be written");
        assert!(failed.contains("captured no fill"), "{failed}");
    }
}
