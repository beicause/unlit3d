//! End-to-end test: drive the `unlit3d-mcp` binary over stdio the way an MCP
//! client does.
//!
//! The server is spawned as a child process, spoken to in newline-delimited
//! JSON-RPC, and asked to build a mesh, tint the clear colour and screenshot
//! the offscreen target. Nothing here reaches into the crate: the binary is the
//! same one a client launches, and the protocol is the real transport.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A running `unlit3d-mcp` child, with a request id of its own.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_unlit3d-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the server binary starts");
        let stdin = child.stdin.take().expect("the child has a stdin");
        let stdout = BufReader::new(child.stdout.take().expect("the child has a stdout"));
        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let initialized = server.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "unlit3d-mcp-test", "version": "0" },
            }),
        );
        assert_eq!(initialized["protocolVersion"], "2024-11-05");
        server.notify("notifications/initialized", json!({}));
        server
    }

    /// Send a request and read the one reply it gets.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{message}").expect("the request is written");
        self.stdin.flush().expect("the request is flushed");

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("the server replies");
        let reply: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("bad reply {line:?}: {error}"));
        assert_eq!(reply["id"], self.next_id, "the reply answers the request");
        assert!(reply.get("error").is_none(), "the request failed: {reply}");
        reply["result"].clone()
    }

    fn notify(&mut self, method: &str, params: Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(self.stdin, "{message}").expect("the notification is written");
        self.stdin.flush().expect("the notification is flushed");
    }

    /// Call a tool and return its structured content.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.try_tool(name, arguments)
            .unwrap_or_else(|| panic!("the tool {name} failed"))
    }

    /// Call a tool, returning the content when it succeeds and None when the
    /// tool itself reports an error.
    fn try_tool(&mut self, name: &str, arguments: Value) -> Option<Value> {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        if result["isError"] == true {
            return None;
        }
        Some(
            result
                .get("structuredContent")
                .cloned()
                .unwrap_or_else(|| result["content"].clone()),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The base64 `data` of a structured image result, decoded.
fn image_bytes(value: &Value) -> Vec<u8> {
    assert_eq!(value["mime_type"], "image/png");
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value["data"].as_str().expect("the image has data"))
        .expect("the image data is base64")
}

/// An RGBA pixel of a decoded PNG.
fn pixel(image: &image::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn the_stdio_server_lists_its_tools_and_renders() {
    let mut server = Server::start();

    // The tool list is the server's whole surface: every tool the host exposes
    // is reachable from a client.
    let listed = server.request("tools/list", json!({}));
    let names: Vec<&str> = listed["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool has a name"))
        .collect();
    for expected in [
        "world_summary",
        "get_component",
        "set_component",
        "create_mesh",
        "send_input",
        "read_buffer",
        "screenshot",
        "render_frame",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    // A fresh offscreen world has entities, and one of them is the clear state.
    let summary = server.tool("world_summary", json!({}));
    assert!(
        summary["entities"].as_u64().unwrap_or(0) > 0,
        "the offscreen world has entities: {summary}"
    );

    let entities = server.tool("list_entities", json!({ "limit": 64 }));
    let load_ops = entities
        .as_array()
        .expect("list_entities returns an array")
        .iter()
        .find(|entity| {
            entity["components"]
                .as_array()
                .expect("an entity lists components")
                .iter()
                .any(|name| name.as_str().unwrap_or_default().ends_with("RenderLoadOps"))
        })
        .expect("the world carries a RenderLoadOps")["entity"]
        .as_u64()
        .expect("an entity id is a number");

    // A distinctive clear colour, so the screenshot proves the write landed.
    server.tool(
        "set_component",
        json!({
            "entity": load_ops,
            "component": "RenderLoadOps",
            "value": { "color": { "clear": [0.1, 0.2, 0.3, 1.0] } },
        }),
    );

    // A red triangle at the centre of the default camera's view.
    server.tool(
        "create_mesh",
        json!({
            "positions": [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            "color": [1.0, 0.0, 0.0, 1.0],
        }),
    );

    let shot = server.tool("screenshot", json!({}));
    let (width, height) = (
        shot["width"].as_u64().expect("the image has a width") as u32,
        shot["height"].as_u64().expect("the image has a height") as u32,
    );
    assert_eq!((width, height), (960, 720));

    let image = image::load_from_memory(&image_bytes(&shot))
        .expect("the screenshot is a PNG")
        .to_rgba8();

    // The corner is the clear colour, sRGB-encoded; the centre is the mesh.
    let corner = pixel(&image, 2, 2);
    assert!(
        corner[2] > corner[1] && corner[1] > corner[0] && corner[0] > 20,
        "the corner should be the clear colour, got {corner:?}"
    );
    let centre = pixel(&image, width / 2, height / 2);
    assert!(
        centre[0] > 200 && centre[1] < 60 && centre[2] < 60,
        "the centre should be the red mesh, got {centre:?}"
    );

    // The graph's own listing names every resource; buffers that were not made
    // for readback are refused, and one that was can be read through the same
    // tool a client would use.
    let resources = server.tool("list_resources", json!({}));
    let buffers: Vec<u64> = resources
        .as_array()
        .expect("list_resources returns an array")
        .iter()
        .filter(|resource| resource["kind"] == "Buffer")
        .map(|resource| {
            resource["index"]
                .as_u64()
                .expect("a resource index is a number")
        })
        .collect();
    assert!(!buffers.is_empty(), "the world has buffer resources");
    let mut read = None;
    let mut refusals = 0;
    for buffer in buffers {
        match server.try_tool(
            "read_buffer",
            json!({ "index": buffer, "offset": 0, "size": 16 }),
        ) {
            Some(value) => {
                read = Some(value);
                break;
            }
            None => refusals += 1,
        }
    }
    let read = read.expect("a buffer resource can be read back");
    assert_eq!(read["size"], 16, "read_buffer honours the requested size");
    assert!(
        read["data"].as_str().is_some_and(|data| !data.is_empty()),
        "read_buffer returns base64 data: {read}"
    );
    assert!(
        refusals > 0,
        "a buffer without COPY_SRC is refused rather than crashing the host"
    );

    // A sent event stays in the state until it is cleared, and reading the
    // state reports it, so a caller can see what happened this frame.
    server.tool(
        "send_input",
        json!({ "events": [{ "Key": { "key": "A", "pressed": true } }] }),
    );
    let input = server.tool("input_state", json!({}));
    let events = input["events"]
        .as_array()
        .expect("the input state lists its events");
    assert_eq!(events.len(), 1, "the one sent event is reported: {input}");
    assert!(
        input.to_string().contains("\"A\""),
        "the event names the key that was sent: {input}"
    );
}
