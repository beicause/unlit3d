//! The end-to-end MCP test's body, which only a host can run.
//!
//! The MCP server speaks newline-delimited JSON-RPC over a native pipe, so
//! neither the protocol nor the modules it reaches exist on wasm. The registry
//! in the parent module declares the test `host_only`, and this module is
//! compiled only where it can run.

use std::path::Path;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf};
use unlit_ecs::bundle;
use unlit_ecs::mcp::{McpCommand, WorldEntry, WorldRef, serve_with};
use unlit_wgpu::render_attachments::create_color_target;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};
use unlit_wgpu_test_util::{Ctx, block_on};
use unlit3d::frame::{Frame, dispatch_frame};
use unlit3d::prelude::*;

/// The format of the offscreen target the test world renders into.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The offscreen target's size.
const SIZE: (u32, u32) = (960, 720);
/// The offscreen target's multisample count.
const SAMPLES: u32 = 4;

/// A real glTF binary in the asset submodule, for the path form of the tool.
const FOX_GLB: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../unlit3d_asset_files/assets/Fox.glb"
);

/// The clear colour the world is framed against. It is written as a linear
/// colour and stored sRGB-encoded, the way every colour on this target is.
const CLEAR: [f32; 4] = [0.1, 0.2, 0.3, 1.0];

/// Build an empty offscreen world: a render context, a pass, a renderer, a mesh
/// source, an input state, a default camera, and a colour target both as a
/// texture a screenshot can read back and as the view the pass draws into.
fn offscreen_world(ctx: &Ctx) -> World {
    let mut world = World::new();
    let context = spawn_context(&mut world, ctx.render_device(), ResourceGraph::new());
    // The pass carries the load ops the frame is opened with, so the MCP
    // client can patch them on the entity it finds them on.
    let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
    world.root_scope_mut().spawn(Renderer::new(context));
    let mut source = MeshSource::new(&world.root_scope(), context);
    source.register_lit_family(&world.root_scope());
    let source_entity = world
        .scope_mut(pass)
        .spawn(bundle![Source::new(FrameSourceOrder::MESH, source)]);
    // The world dispatches its own input, so it holds a buffer of its own and
    // the writer that feeds it.
    let input = InputWriter::new();
    {
        let shared = world.shared();
        let mut shared_view = world.scope_mut(shared);
        shared_view.spawn(input.subscribe());
        shared_view.spawn(bundle![input]);
    }

    let color = create_color_target(&ctx.device, COLOR_FORMAT, SIZE.0, SIZE.1);
    let color_view = {
        let mut resource_graph_row = context.graph(&world.root_scope());

        let graph = resource_graph_row.get();
        // The texture is registered in its own right so a screenshot can name
        // it, and the view depends on it so the graph keeps it alive. The label
        // is what a client reads to find the target among the graph's textures.
        let texture = graph.insert_labelled(color.clone(), None, Some("scene color"));
        let view = graph.insert(
            TextureExt::create_view(&color, &wgpu::TextureViewDescriptor::default()),
            None,
        );
        graph.add_dependency(&view, &texture);
        view
    };
    let attachments =
        FrameAttachments::new(&world, context, COLOR_FORMAT, SIZE, SAMPLES, true, false);
    world
        .root_scope()
        .query_one::<&mut FramePass>(pass)
        .get()
        .set_attachments(&attachments, color_view);

    let aspect = SIZE.0.max(1) as f32 / SIZE.1.max(1) as f32;
    // The camera sits in the source's own subtree, which is what it frames.
    world.scope_mut(source_entity).spawn(bundle![Camera {
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
    },]);

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
                // The test host owns no window and no frame loop: it renders
                // only when a tool asks. State a few frames by hand, so the
                // frame clock has a loop to report.
                for index in 0..3u32 {
                    let mut frame = Frame {
                        delta_time: 1.0 / 60.0,
                        index,
                    };
                    dispatch_frame(&mut world, &mut frame);
                }
                ready_tx.send(()).expect("the test waits for the world");
                while let Ok(command) = incoming.recv_blocking() {
                    if command.wants_worlds() {
                        command.answer_worlds(vec![WorldEntry {
                            world: WorldRef::Host,
                            name: "host",
                        }]);
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

    /// Send a request and read the whole reply, so a test can inspect a
    /// protocol-level error as well as a tool's own result.
    async fn try_request(&mut self, method: &str, params: Value) -> Value {
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
                return message;
            }
        }
    }

    /// Send a request and read its result, panicking on a protocol error.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.try_request(method, params).await["result"].clone()
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

/// The id of the world's scene-colour target, found the way a client finds it.
///
/// The listing is the only thing a client sees, so the target is named by the
/// label the scene gave it rather than by position among the graph's textures.
async fn scene_color(server: &mut Server) -> u64 {
    let resources = server.tool("list_resources", json!({})).await;
    let labelled = resources
        .as_array()
        .expect("list_resources returns an array")
        .iter()
        .filter(|resource| resource["label"] == "scene color")
        .collect::<Vec<_>>();
    assert_eq!(
        labelled.len(),
        1,
        "the world labels one scene-colour target: {resources}"
    );
    assert_eq!(
        labelled[0]["kind"], "Texture",
        "the label is on the texture"
    );
    labelled[0]["id"]
        .as_u64()
        .expect("a resource id is a number")
}

/// Render one frame and read the offscreen target back as pixels.
async fn screenshot(server: &mut Server) -> image::RgbaImage {
    let target = scene_color(server).await;
    // A screenshot answers with an image content block, not structured
    // content, so read the raw result rather than the tool helper's value.
    let shot = server
        .request(
            "tools/call",
            json!({
                "name": "read_screenshot",
                "arguments": { "target": target },
            }),
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
    image
}

/// A glTF document holding one red triangle, its buffer inline as a data URI.
///
/// The material states `KHR_materials_unlit`, so the frame carries its base
/// colour whatever the lights do, which is what a world without lights needs
/// for a pixel assertion to hold.
fn triangle_document() -> String {
    // Three positions as f32, then three u16 indices.
    const BIN_BASE64: &str = "AAAAAAAAAD8AAAAAAAAAvwAAAL8AAAAAAAAAPwAAAL8AAAAAAAABAAIA";
    format!(
        r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["KHR_materials_unlit"],"scene":0,
        "scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
        "materials":[{{"extensions":{{"KHR_materials_unlit":{{}}}},
        "pbrMetallicRoughness":{{"baseColorFactor":[1.0,0.0,0.0,1.0]}}}}],
        "accessors":[
        {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
        "min":[-0.5,-0.5,0.0],"max":[0.5,0.5,0.0]}},
        {{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}],
        "bufferViews":[
        {{"buffer":0,"byteOffset":0,"byteLength":36,"target":34962}},
        {{"buffer":0,"byteOffset":36,"byteLength":6,"target":34963}}],
        "buffers":[{{"byteLength":42,"uri":"data:application/octet-stream;base64,{BIN_BASE64}"}}]}}"#
    )
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

/// Drive the whole exchange, on the runtime the MCP plumbing awaits on.
///
/// The registered body is a future because the harness drives one, but the
/// streams here are in-memory and the server side owns its own thread, so the
/// work itself is the runtime's to block on.
pub fn run() {
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
            "spawn_gltf",
            "frame_clock",
            "frame_clocks",
            "send_input",
            "read_buffer",
            "save_buffer",
            "read_screenshot",
            "save_texture_as_image",
            "save_screenshot",
            "despawn_entity",
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
        assert_eq!(worlds[0]["world"], "Host", "and it is the host world");
        assert_eq!(worlds[0]["name"], "host", "under the name it was spawned");

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
                    "value": { "color": { "clear": CLEAR } },
                }),
            )
            .await;

        // A red triangle from an inline document, so the centre of the frame
        // is the mesh and the rest is the clear colour.
        server
            .tool("spawn_gltf", json!({ "document": triangle_document() }))
            .await;
        let image = screenshot(&mut server).await;
        let centre = pixel(&image, SIZE.0 / 2, SIZE.1 / 2);
        assert!(
            centre[0] > 200 && centre[1] < 60 && centre[2] < 60,
            "the centre should be the red mesh, got {centre:?}"
        );
        let clock = server.tool("frame_clock", json!({})).await;
        assert_eq!(
            clock["index"].as_u64(),
            Some(2),
            "the clock follows the frames the host stated: {clock}"
        );
        assert!(
            (clock["fps"].as_f64().unwrap_or(0.0) - 60.0).abs() < 1.0,
            "the instantaneous rate is the last frame's: {clock}"
        );
        assert_eq!(
            clock["average_fps"],
            serde_json::Value::Null,
            "the monitor a first call spawns has seen no frame yet: {clock}"
        );
        assert_eq!(
            clock["average_window_frames"],
            serde_json::Value::Null,
            "so the window it averages over is empty: {clock}"
        );

        // The listing reports the same clock under the name the world list
        // gives it, so a client compares worlds without sampling each one.
        let clocks = server.tool("frame_clocks", json!({})).await;
        let clocks = clocks.as_array().expect("frame_clocks returns an array");
        assert_eq!(clocks.len(), 1, "the host holds one world: {clocks:?}");
        assert_eq!(clocks[0]["name"], "host", "under the name it was spawned");
        assert_eq!(
            clocks[0]["clock"]["index"].as_u64(),
            Some(2),
            "the listed clock is the one frame_clock reports: {clocks:?}"
        );

        // The label is how a client names the target it did not build: the
        // scene states which of the graph's textures the frame lands in, so no
        // tool has to guess.
        let target = scene_color(&mut server).await;

        // A target is required, not inferred. Without one there is nothing to
        // read, and a world with several textures has no way to say which. The
        // request is rejected before the tool runs, so it comes back as a
        // protocol error rather than a tool result.
        let untargeted = server
            .try_request(
                "tools/call",
                json!({ "name": "read_screenshot", "arguments": {} }),
            )
            .await;
        assert!(
            untargeted.get("error").is_some() && untargeted.get("result").is_none(),
            "a screenshot without a target is refused: {untargeted}"
        );

        // A view of the target names it too: the pass that draws into an
        // offscreen target holds the view, not the texture, so a client
        // reading the pass's own handles can still take the shot.
        let resources = server.tool("list_resources", json!({})).await;
        let views = resources
            .as_array()
            .expect("list_resources returns an array")
            .iter()
            .filter(|resource| resource["kind"] == "TextureView")
            .map(|resource| resource["id"].as_u64().expect("a resource id is a number"))
            .collect::<Vec<_>>();
        assert!(
            views.len() > 1,
            "the offscreen world registers a view per attachment, got {views:?}"
        );
        let mut by_view = None;
        for view in views {
            let result = server
                .request(
                    "tools/call",
                    json!({ "name": "read_screenshot", "arguments": { "target": view } }),
                )
                .await;
            if result["isError"] == true {
                continue;
            }
            let decoded = image::load_from_memory(&image_bytes(&result))
                .expect("a view-named screenshot is a WebP")
                .to_rgba8();
            by_view = Some(decoded);
        }
        let by_view = by_view.expect("the colour view is readable");
        assert_eq!(by_view, image, "a view names the texture it views");

        // The label names one texture, and the view of it shows the same
        // picture: a client that reads the listing and a client that reads the
        // pass's own handles reach the same target.
        let by_label = server
            .request(
                "tools/call",
                json!({ "name": "read_screenshot", "arguments": { "target": target } }),
            )
            .await;
        let by_label = image::load_from_memory(&image_bytes(&by_label))
            .expect("a label-named screenshot is a WebP")
            .to_rgba8();
        assert_eq!(
            by_label, by_view,
            "the labelled texture and its view are one picture"
        );

        // A document and a path are the two ways to name the same thing: asking
        // for neither is a bad request, and both leaves it ambiguous.
        assert!(
            server.try_tool("spawn_gltf", json!({})).await.is_none(),
            "a request naming neither a path nor a document is refused"
        );
        assert!(
            server
                .try_tool(
                    "spawn_gltf",
                    json!({ "path": FOX_GLB, "document": triangle_document() }),
                )
                .await
                .is_none(),
            "a request naming both a path and a document is refused"
        );
        // A real file on disk, so the path form is exercised against the glTF
        // loader's own importer as well as the inline one.
        let fox = server.tool("spawn_gltf", json!({ "path": FOX_GLB })).await;
        assert!(
            fox["entities"]
                .as_array()
                .is_some_and(|entities| !entities.is_empty()),
            "spawning a file spawns its scene: {fox}"
        );
        // The model is one ECS subtree rooted at the returned bundle entity;
        // the response keeps the root and every child handle so callers can
        // inspect or despawn any part of the model.
        let fox_entities = fox["entities"]
            .as_array()
            .expect("spawn_gltf returns entity handles");
        assert!(
            fox_entities.len() > 1,
            "a model spawn returns its root and descendants: {fox}"
        );
        assert_eq!(
            fox["names"]
                .as_array()
                .and_then(|names| names.first())
                .and_then(Value::as_str),
            Some("lit model"),
            "the first handle identifies the model bundle root: {fox}"
        );
        assert!(
            fox["names"]
                .as_array()
                .is_some_and(|names| { names.iter().any(|name| name == "lit model primitive") }),
            "the response includes a drawable descendant: {fox}"
        );

        // Despawning a model root removes every returned descendant as well.
        let entity = fox_entities[0]
            .as_u64()
            .expect("the model root is an entity");
        server
            .tool("despawn_entity", json!({ "entity": entity }))
            .await;
        let remaining = server.tool("list_entities", json!({})).await;
        assert!(
            !remaining
                .as_array()
                .expect("list_entities returns an array")
                .iter()
                .any(|row| fox_entities.contains(&row["entity"])),
            "no entity of the despawned model remains"
        );

        // The save tools answer with the path they wrote instead of the image,
        // so a caller reads a file off disk rather than decoding base64.
        let directory = temp_dir("screenshot");
        let saved = server
            .tool(
                "save_screenshot",
                json!({
                    "target": target,
                    "directory": directory.to_str().unwrap(),
                }),
            )
            .await;
        let path = saved["path"]
            .as_str()
            .expect("save_screenshot reports a path");
        // The path's own file name, rather than what follows the last `/`:
        // a path is spelled with the separator of the platform that wrote it.
        let default_name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("the path names a file");
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
                    "target": target,
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
                    "target": target,
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
                    "target": target,
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

        // A sent event lands in the queue of every subscriber, and the queue is
        // a component: a caller reads what a world has not dispatched yet
        // through the same reflection it reads any other component with.
        server
            .tool(
                "send_input",
                json!({ "events": [{ "Key": { "key": "A", "pressed": true } }] }),
            )
            .await;
        let entities = server.tool("list_entities", json!({})).await;
        let queue = entities
            .as_array()
            .and_then(|entities| {
                entities.iter().find(|entity| {
                    // A component is listed under the name the compiler gave it,
                    // so match the type the way the lookups above do.
                    entity["components"].as_array().is_some_and(|names| {
                        names
                            .iter()
                            .any(|name| name.as_str().unwrap_or_default().ends_with("InputEvents"))
                    })
                })
            })
            .expect("the world carries a subscription");
        let entity = queue["entity"].clone();
        let args = json!({ "entity": entity, "component": "InputEvents" });
        let events = server.tool("get_component", args.clone()).await;
        let pending = events["events"]
            .as_array()
            .expect("the subscription lists what it has not read");
        assert_eq!(pending.len(), 1, "the one sent event is pending: {events}");
        assert!(
            events.to_string().contains("\"A\""),
            "the event names the key that was sent: {events}"
        );
        // Reading does not consume: the world still dispatches the event.
        let again = server.tool("get_component", args).await;
        assert_eq!(again["events"], events["events"]);
    });
}
