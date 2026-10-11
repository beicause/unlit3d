English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d_mcp/README.zh-CN.md)

# unlit3d_mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server that lets an
agent read and drive a running unlit3d world. The package is `unlit3d_mcp`. It
is at an **early stage** along with the crates it uses.

## No privileged path

The server is a separate crate for a reason: it has no access the caller does
not. Every tool is written against the same public API a person would use — a
`World`, a `Renderer`, a `MeshSource`, a `ResourceGraph` — and nothing in
`unlit3d` or `unlit_wgpu` knows the protocol exists. There is no private channel
for built-in pipelines and no shortcut for the server, so anything the server can
do, a caller can do; and anything a caller cannot do, the server cannot either.

The one thing that has to be written down is which components a JSON bridge can
name: components are any `'static` type, so a world alone cannot name one. A
component becomes addressable by submitting an entry to a link-time table in
`unlit3d`'s `reflect` feature — the entry pairs the type with a way in and a way
out of JSON.

For most components there is no second declaration to write: the component
derives its own reflection with [`facet`](https://docs.rs/facet), so its struct
fields **are** the JSON shape, and `ComponentEntry::new` (or
`ComponentEntry::default`, for a component that has a natural default to merge a
partial value over) turns that reflection into the codec. A field whose type glam
or the ECS owns is reflected through a proxy from the same feature, which is why
the server enables it. A component whose fields cannot be reflected — a GPU
handle, a private field — reflects through a proxy instead; one that cannot be
written makes its proxy’s conversion fail, so a write reports why rather than
inventing a value.

Entries are collected from wherever they are written, so a crate that defines its
own components registers them itself with `inventory::submit!` — nothing has to
be handed a registry. The table is link-time, so a crate whose entries are never
reached from the binary is not linked in either; a binary that wants a crate’s
components names that crate.

A component is addressed by its bare type name (`Transform`) or by its
module-qualified one (`unlit3d::components::Transform`); the latter tells two
same-named components in different modules apart.

## Transport

The transport is **stdio**: the server reads JSON-RPC requests on stdin and
writes responses to stdout. That is the transport MCP clients launch a command
for; a host with a window serves the same server over stdio too. It is built on
[`rmcp`](https://docs.rs/rmcp) and tokio, both of which are non-optional
dependencies of this crate; the rest of the workspace never sees them.

Because a world is not `Send`, the transport never touches one: the protocol
thread owns tokio and the `rmcp` service, and every tool packs a closure and a
`WorldId` into an `McpCommand`, hands it to a sink (`serve_stdio_with`), and
awaits a JSON value. The thread that owns the world — a render thread the
standalone binary starts, or a window's event loop — calls
`McpCommand::dispatch` to run it. The two never share a world, and nothing here
assumes how many worlds there are or that a world holds a rendering device.

## Tools

The server exposes a world's public surface in four groups. Every tool names the
world it runs against with a `WorldId`: the string `"Host"` for the world the
owning thread holds directly, or `{"Handle": bits}` for a world carried by a
host entity. `list_worlds` reports which worlds the host carries — including
worlds on other threads, which only the host can name — so ask it before naming
a world. GPU tools additionally name the entity handles they need — a render
context, a renderer, a mesh source, a resource — and the server passes them
through untouched, leaving their resolution to the thread that owns the world.

**World and entities** — `list_worlds`, `world_summary`, `list_components`,
`list_archetypes`, `list_entities`, `get_entity`, `get_component`,
`set_component`, `spawn_entity`, `despawn_entity`. An entity is named by its
`u64` bits, and a component by its registered name. Entities cannot gain or lose
components after they are spawned — that is the ECS's contract, not a limit of
the server — so `spawn_entity` takes a whole component set and `set_component`
only overwrites values in place. A component that has a natural default can be
given field by field; `set_component` merges what it is given over what the
entity already holds.

**Resource graph** — `graph_summary`, `list_resources`,
`resource_dependencies`, `graph_maintain`, `read_buffer`, `save_buffer`,
`read_texture_as_image`, `save_texture_as_image`. A resource is named by the
`u64` id `list_resources` reports — the `ResId` bits: slot index in the low 32,
the slot's generation in the high 32 — and the graph resolves it back to a
handle. An id to a removed resource stops resolving rather than quietly naming
the resource that reused its slot. `graph_maintain` is the same pass the
render loop runs: it drops unreferenced resources and rebuilds dirty ones.
`read_buffer` refuses a buffer that was not created with `COPY_SRC` rather than
letting the device reject the copy. `save_buffer` and `save_texture_as_image`
read the same way but write to a file in the directory they are given —
creating it when it does not exist — and answer with the written path and size,
so a caller reads bytes off disk instead of decoding base64 or an inline image.

**Input** — `input_state` and `send_input`. `input_state` reports the state
`InputState` holds, including the events that arrived since they were last
cleared, so a caller can see what happened this frame. `send_input` pushes events
into the world's `InputHandle` and delivers them through the same `dispatch_input`
pass the window does, so behaviour components see them exactly as they would from
a user.

**Drawing** — `create_mesh`, `remove_mesh`, `render_frame`,
`read_screenshot`, `save_screenshot`, `load_gltf`. `create_mesh` allocates an
unlit mesh from positions, optional uvs, colors and indices, and spawns the
entity that draws it; `remove_mesh` releases it again. `render_frame` draws one
frame, and `read_screenshot` draws one and returns the offscreen target as an
image content block, so a client renders the pixels directly, with the image size
as structured content; `read_texture_as_image` answers the same way.
`save_screenshot` draws one too and writes the PNG to a file, answering with its
path and size.

The `save_*` tools exist because an inline image or base64 payload costs a model
context it cannot easily shed, while a file on disk can be read back a piece at a
time, or not at all. Prefer them, and reach for the `read_*` tools only when the
caller genuinely wants the pixels in the reply.

## Library

The crate is the transport and nothing else: it holds no world and builds no
GPU device. A host that already owns a world — a window's event loop, the CLI,
or a thread of its own — calls `serve_stdio_with` and passes a sink that posts
each `McpCommand` to that thread, which resolves the command's `WorldId` and
calls `McpCommand::dispatch`. A `list_worlds` command has no job: the host
recognises it with `McpCommand::wants_worlds` and answers it with
`McpCommand::answer_worlds`, naming every world it carries — including worlds
on other threads, which only the host can name. The tools are plain functions
over `&mut World` (and, for the GPU ones, the entity handles they are given), so
a host can call one directly without the protocol in the way.

The CLI and the example both take an `--mcp` switch that serves the world they
would otherwise have rendered, over stdio. The CLI builds a headless world and
serves it; the example keeps its window and serves the scene from its event
loop, with a second world in the same host blitting that scene's offscreen frame
into the swap chain:

```text
cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp
cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube
```

## Tests

`tests/server.rs` builds a real GPU world on a thread of its own, serves it over
a pair of streams, and speaks JSON-RPC to it from the test: it lists the tools,
reads the world, writes a clear colour, spawns a mesh, and checks the screenshot
pixel by pixel. The world is a real GPU world, so the test proves the whole path
— protocol, host, render, readback — rather than the pieces in isolation.

```text
cargo nextest run -p unlit3d_mcp
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
