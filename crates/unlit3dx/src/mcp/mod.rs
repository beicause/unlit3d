//! The MCP tools an audio world answers: recording the mix it fills.
//!
//! Each tool routes its work to the world it names through [command], exactly
//! as the rendering tools do. The tools register themselves through
//! [ToolSource], so a server built from [unlit_ecs] carries them without
//! depending on this crate.

mod bridge;

pub use bridge::{pump_mcp, serve_mcp};

use unlit_ecs::mcp::serde_json::{self, Value};
use unlit_ecs::mcp::{
    CommandSender, EncodedAudio, McpServer, Output, ToolResult, ToolRouter, ToolSource, WorldArg,
    command, run, tool,
};
use unlit_ecs::{Entity, World, bundle};
use unlit3d::winit::builtin::ExitRequest;

use crate::record::{RecordingFormat, RecordingHandle, start_recording};

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
        "Stop an audio world's recording and write what it captured to a file in a directory, returning the written path and size. The file name's extension picks the format: .flac (lossless, the default when it has none) or .wav. Prefer this over read_audio_recording, which returns the mix inline.",
        stop_audio_recording,
    ));
    router.add_route(tool(
        "cancel_audio_recording",
        "Stop an audio world's recording and discard what it captured.",
        cancel_audio_recording,
    ));
    router.add_route(tool(
        "read_audio_recording",
        "Stop an audio world's recording and return its captured mix as an audio content block, without writing a file. The format argument picks the encoding: \"flac\" (lossless, the default) or \"wav\".",
        read_audio_recording,
    ));
    router.add_route(tool(
        "request_exit",
        "Ask the host to exit normally. The host must carry an ExitRequest component.",
        request_host_exit,
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

/// Finish a recording and return its sound without touching the filesystem.
async fn read_audio_recording(commands: CommandSender, args: ReadRecording) -> ToolResult {
    run(&commands, args.world, move |world| {
        read(world, args.format.as_deref())
    })
    .await
}

/// Ask a windowed or headless host to exit normally.
async fn request_host_exit(commands: CommandSender, args: WorldArg) -> ToolResult {
    command(&commands, args.world, |world| {
        let mut found = false;
        for mut request in world.root_scope().query::<&mut ExitRequest>() {
            request.0 = true;
            found = true;
        }
        if !found {
            return Err("the host has no exit request".to_owned());
        }
        Ok(serde_json::json!({ "requested": true }))
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
        .spawn(bundle![ActiveRecording(handle)]);
    serde_json::to_value(report).map_err(|error| error.to_string())
}

/// Stop the recording, write its samples and report the file.
fn stop(world: &mut World, directory: &str, file_name: Option<&str>) -> Result<Value, String> {
    // The name is settled before the recording is taken: an extension the
    // server cannot write is a mistake to report while the mix is still in the
    // world, not after it has been despawned.
    let name = unlit_ecs::mcp::saved_name(file_name, "audio", "flac");
    let format = RecordingFormat::of(&name)?;
    let handle = take(world)?;
    let Some(audio) = handle.stop(world) else {
        return Err(
            "the recording captured no fill; check that the world has sources to play".to_owned(),
        );
    };
    let bytes = format.encode(audio.format, &audio.samples)?;
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

/// Stop the recording and answer with its sound in memory.
fn read(world: &mut World, format: Option<&str>) -> Result<Output, String> {
    use base64::Engine as _;

    let format = match format {
        Some(name) => RecordingFormat::from_name(name)?,
        None => RecordingFormat::DEFAULT,
    };
    let handle = take(world)?;
    let Some(audio) = handle.stop(world) else {
        return Err("the recording captured no fill".to_owned());
    };
    let bytes = format.encode(audio.format, &audio.samples)?;
    Ok(Output::Audio(EncodedAudio {
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime_type: format.mime_type().to_owned(),
        structured: serde_json::json!({
            "size": bytes.len(),
            "frames": audio.frame_count(),
            "seconds": audio.seconds(),
            "sample_rate": audio.format.sample_rate,
            "channels": audio.format.channels,
        }),
    }))
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
    world.root_scope_mut().despawn(entity).unwrap();
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
    /// The file name to write; it is given a `.flac` extension when it
    /// carries none.
    #[serde(default)]
    pub file_name: Option<String>,
}

/// Arguments for finishing a recording in memory.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ReadRecording {
    /// The world to drive: `"Host"` for the world the transport thread itself
    /// holds, or `{"Handle": bits}` for a world a host entity carries.
    #[serde(default)]
    pub world: unlit_ecs::mcp::WorldId,
    /// The audio format to answer with: `"flac"` (lossless, the default) or
    /// `"wav"`.
    #[serde(default)]
    pub format: Option<String>,
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
        AudioChunk, AudioFillOrder, AudioFormat, GlobalAudioGain, OnAudioFill, dispatch_audio_fill,
    };

    /// A world with one source that fills a constant level, plus the format
    /// its fills carry.
    fn audio_world(level: f32) -> (World, AudioFormat) {
        let mut world = World::new();
        world.root_scope_mut().spawn(bundle![GlobalAudioGain(1.0)]);
        world.root_scope_mut().spawn(bundle![OnAudioFill::new(
            AudioFillOrder(0),
            move |_world, _entity, chunk: &mut AudioChunk| chunk.out.fill(level),
        ),]);
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
    fn a_name_without_an_extension_is_written_as_flac() {
        let (mut world, format) = audio_world(0.5);
        start(&mut world, Some(1.0)).expect("the recording starts");
        let mut out = [0.0f32; 4];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);

        let directory =
            std::env::temp_dir().join(format!("unlit3dx-mcp-{}-flac", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let recorded = stop(&mut world, &directory.to_string_lossy(), Some("take"))
            .expect("the recording is written");
        let path = recorded["path"].as_str().expect("the file has a path");
        assert!(
            path.ends_with("take.flac"),
            "the name is given the default extension: {path}"
        );
        let bytes = std::fs::read(path).expect("the file is written");
        assert_eq!(&bytes[0..4], b"fLaC");
    }

    #[test]
    fn a_name_the_server_cannot_write_leaves_the_recording_in_the_world() {
        let (mut world, format) = audio_world(0.5);
        start(&mut world, Some(1.0)).expect("the recording starts");
        let mut out = [0.0f32; 4];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);

        let directory =
            std::env::temp_dir().join(format!("unlit3dx-mcp-{}-refused", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let refused = stop(&mut world, &directory.to_string_lossy(), Some("take.mp3"))
            .expect_err("an extension the server cannot write is refused");
        assert!(refused.contains("mp3"), "{refused}");
        assert_eq!(
            status(&mut world).expect("the recording is still there")["frames"],
            4,
            "a refused name keeps the mix it could not write"
        );

        let recorded = stop(&mut world, &directory.to_string_lossy(), Some("take.flac"))
            .expect("a name the server can write finishes the recording");
        assert_eq!(recorded["frames"], 4);
    }

    #[test]
    fn a_finished_recording_is_answered_with_its_sound() {
        use base64::Engine as _;

        let (mut world, format) = audio_world(0.5);
        start(&mut world, Some(1.0)).expect("the recording starts");
        let mut out = [0.0f32; 4];
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);

        let Output::Audio(sound) = read(&mut world, None).expect("the recording is read") else {
            panic!("the answer carries a sound");
        };
        assert_eq!(sound.mime_type, "audio/flac");
        assert_eq!(sound.structured["frames"], 4);
        let flac = base64::engine::general_purpose::STANDARD
            .decode(&sound.data)
            .expect("the sound is base64");
        assert_eq!(&flac[0..4], b"fLaC");

        start(&mut world, Some(1.0)).expect("the recording starts");
        dispatch_audio_fill(&world, &mut out, format.sample_rate, format.channels);
        let Output::Audio(sound) = read(&mut world, Some("wav")).expect("the recording is read")
        else {
            panic!("the answer carries a sound");
        };
        assert_eq!(sound.mime_type, "audio/wav");
        let wav = base64::engine::general_purpose::STANDARD
            .decode(&sound.data)
            .expect("the sound is base64");
        assert_eq!(&wav[0..4], b"RIFF");
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
