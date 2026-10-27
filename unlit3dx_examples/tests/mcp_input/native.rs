use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use unlit_wgpu_test_util::{Tolerance, assert_image_snapshot_with_tolerance, snapshots};
use unlit3dx_examples::headless::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};
use unlit3dx_examples::scene::WINDOW_SIZE;

use crate::support;

/// Frames to run after pausing, so the released voices have decayed.
const RELEASE_FRAMES: u32 = 300;

/// The baselines this run compares: the three captures and the mix recorded
/// between the first two.
const BASELINES: [&str; 4] = [
    "unlit3dx/mcp_input_1.webp",
    "unlit3dx/mcp_input_2.webp",
    "unlit3dx/mcp_input_3.webp",
    "unlit3dx/mcp_input_audio.flac",
];

// The captures are the same scene as the headless run's, drawn by whichever
// backend the machine offers, so they take the floors that run measured rather
// than a tighter pair no slower rasterizer can reach.
const FRAME_TOLERANCE: Tolerance = Tolerance {
    min_score: Some(62.0),
    max_outliers: Some(0.005),
    channel_delta: 8,
};

pub async fn run() {
    support::assert_baselines_exist(&BASELINES);
    let soundfont = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/FluidR3Mono_GM.sf3");
    assert!(
        soundfont.is_file(),
        "the MCP interaction test needs the real soundfont at {}",
        soundfont.display()
    );
    let mut mcp = Mcp::start();
    let mut status = mcp.status();
    assert_eq!(
        status["ready"], true,
        "the scene starts before it is stepped"
    );
    // The scene loads its soundfont but starts with both tracks stopped, so a
    // recording over the first second has nothing to capture.
    mcp.tool(
        "start_audio_recording",
        json!({ "world": status["audio_world"] }),
    );
    mcp.step(60);
    status = mcp.status();
    assert_eq!(status["balls"], 0, "automatic drops are disabled");
    assert_eq!(status["sequence_paused"], true);
    assert_eq!(status["sequence_time"], 0.0);
    let stopped = mcp.recording(&status);
    assert!(
        stopped.iter().all(|&sample| sample == 0),
        "the mix is silent while every track is stopped"
    );

    mcp.click(&status, "Drop the next ball");
    mcp.step(60);
    status = mcp.status();
    assert_eq!(status["balls"], 1, "the first panel click dropped one ball");
    assert_eq!(
        status["sequence_paused"], true,
        "the initial mix stays stopped"
    );
    let first_image = mcp.image(&status);
    let [first, second, third] = snapshots![
        "unlit3dx/mcp_input_1.webp",
        "unlit3dx/mcp_input_2.webp",
        "unlit3dx/mcp_input_3.webp",
    ];
    assert_image_snapshot_with_tolerance(
        first,
        &first_image,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );

    mcp.tool(
        "start_audio_recording",
        json!({ "world": status["audio_world"] }),
    );
    mcp.click(&status, "Drop the next ball");
    mcp.click(&status, "Drop the next ball");
    mcp.click(&status, "Play");
    mcp.step(180);
    status = mcp.status();
    assert_eq!(
        status["balls"], 3,
        "two more panel clicks dropped two balls"
    );
    assert_eq!(status["sequence_paused"], false);
    assert!(status["sequence_time"].as_f64().unwrap() > 0.0);
    let second_image = mcp.image(&status);
    assert_image_snapshot_with_tolerance(
        second,
        &second_image,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );
    let playing = mcp.recording(&status);
    assert!(
        playing.iter().any(|&sample| sample != 0),
        "MIDI playback was recorded"
    );
    assert!(
        playing
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap()
            > 100
    );
    support::check_audio_baseline("unlit3dx/mcp_input_audio.flac", &playing);

    mcp.click(&status, "Clear the balls");
    mcp.click(&status, "Pause");
    // A pause releases the notes the sequence pressed, and a released voice
    // still rings out for as long as its patch's release takes. That tail is
    // the last sound the scene makes, so the silent recording below starts
    // once it has decayed.
    mcp.step(RELEASE_FRAMES);
    status = mcp.status();
    assert_eq!(status["balls"], 0, "the clear button removed every ball");
    assert_eq!(
        status["sequence_paused"], true,
        "the pause button stopped the sequence"
    );
    mcp.tool(
        "start_audio_recording",
        json!({ "world": status["audio_world"] }),
    );
    mcp.step(60);
    let third_image = mcp.image(&status);
    assert_image_snapshot_with_tolerance(
        third,
        &third_image,
        WINDOW_SIZE.0,
        WINDOW_SIZE.1,
        FRAME_TOLERANCE,
    );
    let paused = mcp.recording(&status);
    assert!(
        paused.iter().all(|&sample| sample == 0),
        "the paused recording is silent"
    );

    mcp.tool("request_exit", json!({}));
    let status = mcp.child.wait().expect("the example process exits");
    assert!(status.success(), "MCP exit failed: {status}");
    let log = mcp
        .log
        .take()
        .unwrap()
        .join()
        .expect("the stderr reader finishes");
    assert!(!log.contains("panicked"), "the example panicked:\n{log}");
    // wgpu's backends report their own probes at ERROR: the EGL one complains
    // about a missing DRI2 driver, the Vulkan loader about the instance it is
    // still bringing up. Each falls back to a backend that works, and neither
    // message is this run's error to answer for.
    let errors: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(" ERROR ") && !line.contains("wgpu_hal::"))
        .collect();
    assert!(
        errors.is_empty(),
        "the example logged errors:\n{}",
        errors.join("\n")
    );
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    log: Option<std::thread::JoinHandle<String>>,
    next_id: u64,
}

impl Mcp {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_unlit3dx-examples"))
            .args(["--headless", "--mcp"])
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the example process starts");
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let stderr = BufReader::new(child.stderr.take().unwrap());
        let log = std::thread::spawn(move || {
            let mut log = String::new();
            for line in stderr.lines() {
                let line = line.unwrap();
                log.push_str(&line);
                log.push('\n');
            }
            log
        });
        let mut this = Self {
            child,
            input,
            output,
            log: Some(log),
            next_id: 0,
        };
        this.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "unlit3dx-mcp-input-test", "version": "1" }
            }),
        );
        this.send(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
        );
        this
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.input, "{message}").expect("the MCP request writes");
        self.input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "the MCP server closed stdout"
            );
            let message: Value = serde_json::from_str(&line).expect("the MCP reply is JSON");
            if message["id"] == id {
                assert!(
                    message.get("error").is_none(),
                    "MCP {method} failed: {message}"
                );
                return message["result"].clone();
            }
        }
    }

    /// The whole result of a tool call, content blocks included.
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert_ne!(result["isError"], true, "MCP tool {name} failed: {result}");
        result
    }

    /// The structured content of a tool call that answers with JSON alone.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call(name, arguments)["structuredContent"].clone()
    }

    fn step(&mut self, frames: u32) {
        self.tool("step_frames", json!({ "frames": frames }));
    }

    fn status(&mut self) -> Value {
        self.tool("scene_status", json!({}))
    }

    /// Press and release a panel button, then advance one frame so the panel
    /// sees the click. A panel resolves at most one click per frame, so two
    /// clicks need a frame between them.
    fn click(&mut self, status: &Value, label: &str) {
        let position = status["buttons"][label].clone();
        assert!(
            position.is_array(),
            "the panel has a {label} button: {status}"
        );
        let events = json!([
            { "Mouse": { "Moved": { "position": position } } },
            { "Mouse": { "Button": { "position": position, "button": "Primary", "pressed": true } } },
            { "Mouse": { "Button": { "position": position, "button": "Primary", "pressed": false } } }
        ]);
        assert_eq!(
            self.tool("send_input", json!({ "events": events }))["delivered"],
            true
        );
        self.step(1);
    }

    fn image(&mut self, status: &Value) -> Vec<u8> {
        let result = self.call(
            "read_texture_as_image",
            json!({
                "world": status["render_world"],
                "id": status["target"],
                "format": "png"
            }),
        );
        let block = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the MCP result contains an image");
        assert_eq!(block["mimeType"], "image/png");
        let png = STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("the MCP image decodes")
            .to_rgba8();
        assert_eq!(image.dimensions(), WINDOW_SIZE);
        image.into_raw()
    }

    /// Stop the recording and take its mix, as 16-bit samples.
    ///
    /// The tool answers with an audio content block, so the samples come back
    /// as a FLAC stream a decoder reads rather than as a base64 field the test
    /// would have to parse out of the JSON.
    fn recording(&mut self, status: &Value) -> Vec<i16> {
        let result = self.call(
            "read_audio_recording",
            json!({ "world": status["audio_world"], "format": "flac" }),
        );
        let block = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == "audio")
            .expect("the MCP result contains a sound");
        assert_eq!(block["mimeType"], "audio/flac");
        let structured = result["structuredContent"].clone();
        assert_eq!(structured["sample_rate"], AUDIO_SAMPLE_RATE);
        assert_eq!(structured["channels"], AUDIO_CHANNELS);
        assert!(structured["frames"].as_u64().unwrap() > 0);
        let flac = STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
        let mut reader = claxon::FlacReader::new(flac.as_slice()).expect("the MCP sound decodes");
        reader
            .samples()
            .map(|sample| sample.expect("a decodable sample") as i16)
            .collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            let _ = self.child.wait();
        }
    }
}
