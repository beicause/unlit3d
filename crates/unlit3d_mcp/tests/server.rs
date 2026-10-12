//! End-to-end test: drive the MCP server over a pair of streams, the way an
//! MCP client does.
//!
//! The server runs in this process on its own thread, speaking
//! newline-delimited JSON-RPC to a world that lives on a second thread. The
//! test asks it to build a mesh, tint the clear colour and screenshot the
//! offscreen target, so the whole path — protocol, host, render, readback — is
//! exercised, not just the pieces in isolation.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit_wgpu_test_util::{Ctx, block_on};
use unlit3d::prelude::*;
use unlit3d_mcp::{McpCommand, WorldId, serve_with};

/// The format of the offscreen target the test world renders into.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The offscreen target's size.
const SIZE: (u32, u32) = (960, 720);
/// The offscreen target's multisample count.
const SAMPLES: u32 = 4;

/// Build an empty offscreen world: a render context, a renderer, a mesh source,
/// an input state, a default camera, and a colour target both as a texture a
/// screenshot can read back and as the view the renderer draws into.
fn offscreen_world(ctx: &Ctx) -> World {
    let mut world = World::new();
    let context = spawn_context(
        &mut world,
        ctx.device.clone(),
        ctx.queue.clone(),
        ResourceGraph::new(),
        ctx.capabilities,
    );
    let renderer = world.spawn((Renderer::new(context),));
    world.spawn((RenderLoadOps::default(),));
    let mut source = MeshSource::new(&world, context);
    source.register_unlit_family(&world);
    world.spawn_source(source);
    world.spawn((InputHandle::new(),));

    let color = create_color_target(&ctx.device, COLOR_FORMAT, SIZE.0, SIZE.1);
    let color_view = {
        let mut graph = world
            .get_mut::<ResourceGraph>(context.graph)
            .expect("spawn_context spawned the graph");
        // The texture is registered in its own right so a screenshot can name
        // it, and the view depends on it so the graph keeps it alive.
        let texture = graph.insert(color.clone(), None);
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments = FrameAttachments::new(&world, context, COLOR_FORMAT, SIZE, SAMPLES, true);
    let _ = world.with_mut::<Renderer, _>(renderer, |renderer| {
        attachments.bind(&world, renderer, color_view);
    });

    let aspect = SIZE.0.max(1) as f32 / SIZE.1.max(1) as f32;
    world.spawn((Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(
            glam::Vec3::new(0.0, 1.0, 3.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        ),
        clip_from_view: glam::camera::rh::proj::directx::perspective_infinite_reverse(
            60.0f32.to_radians(),
            aspect,
            0.1,
        ),
        active: true,
    },));

    world
}

/// A running server, its world thread, and the client end of the streams.
struct Server {
    // The stream halves and the thread handles are options so Drop can close
    // the client end before joining: a detached render thread still using the
    // device when the process exits is what makes this test segfault rather
    // than fail.
    reader: Option<BufReader<ReadHalf<tokio::io::DuplexStream>>>,
    writer: Option<WriteHalf<tokio::io::DuplexStream>>,
    next_id: u64,
    render: Option<std::thread::JoinHandle<()>>,
    transport: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Closing the client end ends the service, which drops the last
        // command sender and so lets the render thread's receive fail. The
        // joins wait for both, so no thread outlives the test's device.
        self.reader = None;
        self.writer = None;
        if let Some(transport) = self.transport.take() {
            let _ = transport.join();
        }
        if let Some(render) = self.render.take() {
            let _ = render.join();
        }
    }
}

impl Server {
    /// Build a world on its own thread, serve it over a pair of streams, and
    /// return the client end.
    async fn start() -> Self {
        let ctx = block_on(Ctx::headless());
        let (commands, incoming) = async_channel::bounded::<McpCommand>(4);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let render = std::thread::Builder::new()
            .name("unlit3d-mcp-test-render".to_owned())
            .spawn(move || {
                let mut world = offscreen_world(&ctx);
                ready_tx.send(()).expect("the test waits for the world");
                while let Ok(command) = incoming.recv_blocking() {
                    if command.wants_worlds() {
                        command.answer_worlds(vec![WorldId::Host]);
                    } else {
                        command.dispatch(&mut world);
                    }
                }
            })
            .expect("the render thread starts");
        ready_rx.recv().expect("the world builds");

        let (server_io, client_io) = tokio::io::duplex(1 << 16);
        let transport = std::thread::Builder::new()
            .name("unlit3d-mcp-test-transport".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("the tokio runtime builds");
                runtime.block_on(async move {
                    let service = serve_with(server_io, move |command| {
                        commands.send_blocking(command).is_ok()
                    })
                    .await
                    .expect("the server starts");
                    let _ = service.waiting().await;
                });
            })
            .expect("the transport thread starts");

        let (client_r, client_w) = tokio::io::split(client_io);
        let mut server = Self {
            reader: Some(BufReader::new(client_r)),
            writer: Some(client_w),
            next_id: 0,
            render: Some(render),
            transport: Some(transport),
        };
        // The handshake every client makes before it calls a tool.
        server
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "unlit3d-mcp-test", "version": "0" },
                }),
            )
            .await;
        server.notify("notifications/initialized", json!({})).await;
        server
    }

    /// Send a request and read the reply with its id.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await;
        loop {
            let message = self.receive().await;
            if message.get("id") == Some(&json!(id)) {
                return message["result"].clone();
            }
        }
    }

    /// Send a notification, which gets no reply.
    async fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }

    /// Write one newline-delimited JSON-RPC message.
    async fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        let writer = self.writer.as_mut().expect("the server is running");
        writer
            .write_all(line.as_bytes())
            .await
            .expect("the server reads the message");
        writer
            .flush()
            .await
            .expect("the message reaches the server");
    }

    /// Read one newline-delimited JSON-RPC message.
    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = self
            .reader
            .as_mut()
            .expect("the server is running")
            .read_line(&mut line)
            .await
            .expect("the server writes a reply");
        assert!(read > 0, "the server closed the stream");
        serde_json::from_str(&line).expect("the reply is JSON")
    }

    /// Call a tool, returning its structured content, or the content block.
    async fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.try_tool(name, arguments)
            .await
            .unwrap_or_else(|| panic!("the {name} tool succeeds"))
    }

    /// Call a tool, returning None when the tool itself reports an error.
    async fn try_tool(&mut self, name: &str, arguments: Value) -> Option<Value> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
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

/// The image content block of a tool result.
fn image_block(result: &Value) -> &Value {
    result["content"]
        .as_array()
        .expect("the result has content")
        .iter()
        .find(|block| block["type"] == "image")
        .expect("the result carries an image content block")
}

/// The base64 `data` of a tool result's image content block, decoded, which
/// must carry `mime_type`.
fn image_bytes_of_type(result: &Value, mime_type: &str) -> Vec<u8> {
    let block = image_block(result);
    assert_eq!(block["mimeType"], mime_type, "the block carries its format");
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(block["data"].as_str().expect("the image has data"))
        .expect("the image data is base64")
}

/// The base64 `data` of a tool result's image content block, decoded; a read
/// tool answers with lossless WebP unless it is asked for another format.
fn image_bytes(result: &Value) -> Vec<u8> {
    image_bytes_of_type(result, "image/webp")
}

/// An RGBA pixel of a decoded PNG.
fn pixel(image: &image::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

/// A fresh directory under the system temp directory for a save tool to write
/// into, named for the test that uses it.
fn temp_dir(name: &str) -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("unlit3d-mcp-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("the temp directory is created");
    directory
}

#[test]
fn the_server_lists_its_tools_and_renders() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the test runtime builds");
    runtime.block_on(async {
        let mut server = Server::start().await;

        // The tool list is the server's whole surface: every tool the host
        // exposes is reachable from a client.
        let listed = server.request("tools/list", json!({})).await;
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .expect("tools is an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("a tool has a name"))
            .collect();
        for expected in [
            "list_worlds",
            "world_summary",
            "get_component",
            "set_component",
            "create_mesh",
            "send_input",
            "read_buffer",
            "save_buffer",
            "read_screenshot",
            "save_texture_as_image",
            "save_screenshot",
            "render_frame",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool {expected}: {names:?}"
            );
        }

        // The host names the one world it holds, and it is the host world.
        let worlds = server.tool("list_worlds", json!({})).await;
        let worlds = worlds.as_array().expect("list_worlds returns an array");
        assert_eq!(worlds.len(), 1, "the test host holds one world");
        assert_eq!(worlds[0], "Host", "and it is the host world");

        // A fresh offscreen world has entities, and one of them is the clear
        // state.
        let summary = server.tool("world_summary", json!({})).await;
        assert!(
            summary["entities"].as_u64().unwrap_or(0) > 0,
            "the offscreen world has entities: {summary}"
        );

        let entities = server.tool("list_entities", json!({ "limit": 64 })).await;
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
        server
            .tool(
                "set_component",
                json!({
                    "entity": load_ops,
                    "component": "RenderLoadOps",
                    "value": { "color": { "clear": [0.1, 0.2, 0.3, 1.0] } },
                }),
            )
            .await;

        // A red triangle, so the centre of the frame is the mesh and the rest
        // is the clear colour.
        server
            .tool(
                "create_mesh",
                json!({
                    "positions": [
                        [0.0, 0.5, 0.0],
                        [-0.5, -0.5, 0.0],
                        [0.5, -0.5, 0.0],
                    ],
                    "color": [1.0, 0.0, 0.0, 1.0],
                }),
            )
            .await;

        // A screenshot answers with an image content block, not structured
        // content, so read the raw result rather than the tool helper's value.
        let shot = server
            .request(
                "tools/call",
                json!({ "name": "read_screenshot", "arguments": {} }),
            )
            .await;
        let image = image::load_from_memory(&image_bytes(&shot))
            .expect("the screenshot is a WebP")
            .to_rgba8();
        assert_eq!(
            image.dimensions(),
            SIZE,
            "the screenshot is the target size"
        );
        let centre = pixel(&image, SIZE.0 / 2, SIZE.1 / 2);
        assert!(
            centre[0] > 200 && centre[1] < 60 && centre[2] < 60,
            "the centre should be the red mesh, got {centre:?}"
        );

        // The save tools answer with the path they wrote instead of the image,
        // so a caller reads a file off disk rather than decoding base64.
        let directory = temp_dir("screenshot");
        let saved = server
            .tool(
                "save_screenshot",
                json!({ "directory": directory.to_str().unwrap() }),
            )
            .await;
        let path = saved["path"]
            .as_str()
            .expect("save_screenshot reports a path");
        let default_name = path.rsplit('/').next().expect("the path names a file");
        assert!(
            default_name.starts_with("screenshot-") && default_name.ends_with(".webp"),
            "save_screenshot defaults to a fresh webp file: {saved}"
        );
        assert_eq!(
            saved["size"].as_u64(),
            Some(
                std::fs::metadata(path)
                    .expect("the screenshot file exists")
                    .len()
            ),
            "the reported size matches the file on disk"
        );
        let saved_image = image::open(path)
            .expect("the saved screenshot is a WebP")
            .to_rgba8();
        assert_eq!(
            saved_image.dimensions(),
            SIZE,
            "the saved screenshot is the target size"
        );

        // The name decides the format: a .png name writes a PNG, and the
        // extension is appended when the name carries none.
        let png = server
            .tool(
                "save_screenshot",
                json!({
                    "directory": directory.to_str().unwrap(),
                    "file_name": "explicit.png",
                }),
            )
            .await;
        let png_path = png["path"]
            .as_str()
            .expect("save_screenshot reports a path");
        assert!(
            png_path.ends_with("explicit.png"),
            "save_screenshot honours an explicit .png name: {png}"
        );
        assert_eq!(
            image::open(png_path)
                .expect("the named file is a PNG")
                .to_rgba8()
                .dimensions(),
            SIZE,
            "the PNG is the target size"
        );
        let jpeg = server
            .tool(
                "save_screenshot",
                json!({
                    "directory": directory.to_str().unwrap(),
                    "file_name": "explicit.jpg",
                }),
            )
            .await;
        let jpeg_path = jpeg["path"]
            .as_str()
            .expect("save_screenshot reports a path");
        assert_eq!(
            image::open(jpeg_path)
                .expect("the named file is a JPEG")
                .to_rgba8()
                .dimensions(),
            SIZE,
            "the JPEG is the target size"
        );
        let named = server
            .tool(
                "save_screenshot",
                json!({
                    "directory": directory.to_str().unwrap(),
                    "file_name": "named",
                }),
            )
            .await;
        assert!(
            named["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("named.webp")),
            "a name without an extension gets the default one: {named}"
        );
        let _ = std::fs::remove_dir_all(&directory);

        // The graph's own listing names every resource; buffers that were not
        // made for readback are refused, and one that was can be read through
        // the same tool a client would use.
        let resources = server.tool("list_resources", json!({})).await;
        let buffers: Vec<u64> = resources
            .as_array()
            .expect("list_resources returns an array")
            .iter()
            .filter(|resource| resource["kind"] == "Buffer")
            .map(|resource| resource["id"].as_u64().expect("a resource id is a number"))
            .collect();
        assert!(!buffers.is_empty(), "the world has buffer resources");
        let mut read = None;
        let mut refusals = 0;
        for buffer in buffers {
            match server
                .try_tool(
                    "read_buffer",
                    json!({ "id": buffer, "offset": 0, "size": 16 }),
                )
                .await
            {
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
        server
            .tool(
                "send_input",
                json!({ "events": [{ "Key": { "key": "A", "pressed": true } }] }),
            )
            .await;
        let input = server.tool("input_state", json!({})).await;
        let events = input["events"]
            .as_array()
            .expect("the input state lists its events");
        assert_eq!(events.len(), 1, "the one sent event is reported: {input}");
        assert!(
            input.to_string().contains("\"A\""),
            "the event names the key that was sent: {input}"
        );
    });
}
