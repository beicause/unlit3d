English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d_mcp/README.zh-CN.md)

# unlit3d_mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server that lets an
agent read and drive a running unlit3d world. The package is `unlit3d_mcp` and
the binary it builds is `unlit3d-mcp`. It is at an **early stage** along with the
crates it uses.

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
for, and it is the only one the crate implements. It is built on
[`rmcp`](https://docs.rs/rmcp) and tokio, both of which are non-optional
dependencies of this crate; the rest of the workspace never sees them.

Because the world is not `Send`, the server runs on two threads: the render
thread owns the `World` and runs the host, and the protocol thread owns tokio and
the `rmcp` service. Every tool turns into a command sent to the render thread,
which runs it and sends back a JSON value. The two never share the world.

## Tools

The server exposes the world's public surface in four groups.

**World and entities** — `world_summary`, `list_components`,
`list_archetypes`, `list_entities`, `get_entity`, `get_component`,
`set_component`, `spawn_entity`, `despawn_entity`. An entity is named by its
`u64` bits, and a component by its registered name. Entities cannot gain or lose
components after they are spawned — that is the ECS's contract, not a limit of
the server — so `spawn_entity` takes a whole component set and `set_component`
only overwrites values in place. A component that has a natural default can be
given field by field; `set_component` merges what it is given over what the
entity already holds.

**Resource graph** — `graph_summary`, `list_resources`,
`resource_dependencies`, `graph_maintain`, `read_buffer`,
`read_texture_as_image`. Resources are named by their slot index, which is
stable for as long as the resource lives. `graph_maintain` is the same pass the
render loop runs: it drops unreferenced resources and rebuilds dirty ones.
`read_buffer` refuses a buffer that was not created with `COPY_SRC` rather than
letting the device reject the copy.

**Input** — `input_state` and `send_input`. `input_state` reports the state
`InputState` holds, including the events that arrived since they were last
cleared, so a caller can see what happened this frame. `send_input` pushes events
into the world's `InputState` and delivers them through the same `dispatch_input`
pass the window does, so behaviour components see them exactly as they would from
a user.

**Drawing** — `create_mesh`, `remove_mesh`, `render_frame`, `screenshot`,
`load_gltf`. `create_mesh` allocates an unlit mesh from positions, optional uvs,
colors and indices, and spawns the entity that draws it; `remove_mesh` releases
it again. `render_frame` draws one frame, and `screenshot` draws one and returns
the offscreen target as a base64 PNG.

## Library

The binary is a thin wrapper: `serve_stdio` starts the render thread and the
protocol on it. A caller that already has a world builds a `Host` with
`Host::from_world` and serves that instead; `Host::new_offscreen` makes an empty
one with a camera, an unlit family and a render target of its own.

```text
cargo run -p unlit3d_mcp --bin unlit3d-mcp
```

The CLI and the example both take an `--mcp` switch that serves the world they
would otherwise have rendered, over stdio:

```text
cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp
cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube
```

## Tests

`tests/stdio_server.rs` starts the real binary as a child process and speaks
JSON-RPC to it: it lists the tools, reads the world, writes a clear colour, spawns
a mesh, and checks the screenshot pixel by pixel. The world is a real GPU world,
so the test proves the whole path — protocol, host, render, readback — rather
than the pieces in isolation.

```text
cargo nextest run -p unlit3d_mcp
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
