English | [简体中文](README.zh-CN.md)

# unlit3d

The high-level rendering API: the ECS-integrated layer that turns a world of
component-carrying entities into GPU draw commands every frame. It bridges
[`unlit_ecs`](../unlit_ecs/README.md) and
[`wgpu_unlit_render`](../wgpu_unlit_render/README.md), and it keeps both as
direct dependencies — their items are reached through their own paths, and the
ones most callers need are re-exported by `prelude`.

Like the rest of the workspace this crate is at an **early stage of
development**; APIs change freely. It stays close to raw `wgpu`: the renderer
uses `wgpu` resources directly, and you are expected to know WebGPU to use it
well.

## Role in the workspace

`unlit3d` is the high-level half. `wgpu_unlit_render` provides the renderer,
the resource graph and the built-in unlit pipeline; `unlit_ecs` provides the
world; this crate provides the components that describe a renderable scene, the
frame-loop structure that drives it, and the platform glue (winit, egui).
`wgpu_unlit_test_util` is a dev-dependency.

## The frame model

A frame is **not** "one main scene plus extras". `Renderer` draws nothing
itself: it owns the frame's render target and records the frame sources it
is given, in the order each source declares through `FrameOrder`.

- Each source builds its own `Scene` in `build_scene`, then the renderer
  records every scene in order into one pass opened over the target's
  attachments. A frame is therefore one encoder and one submission.
- The built-in mesh rendering is one source, `MeshSource`, and it has no more
  privilege than a caller's own. There is no mesh-specific field or draw path
  in the renderer.
- The GPU state — the `wgpu::Device`, the `wgpu::Queue` and the
  `ResourceGraph` — lives in the world as resource components, addressed by a
  `RenderContext`. `spawn_context` spawns it and returns the addresses.
- **Build and record are two phases.** A source registers resources in the
  graph and stages uploads during `build_scene`; recording only reads the
  scenes it already produced, so no borrow conflict ever arises between a
  source's own state and the graph.
- A source's later GPU resources are its own to release; `despawn_source`
  queues the release before despawning, because the graph cannot notice an
  entity going away.

## Components

A renderable entity carries a `GpuMesh`, a `GpuMaterial` and a `GpuPipeline`.
`GpuPipeline` carries a *key*, not a compiled pipeline: which concrete pipeline
an entity needs depends on the frame's render target and the mesh's vertex
layout, neither known at spawn time. A *family* closes that gap — it pairs a
`Variants` cache with a `Specializer` and a `PipelineFactory`, and resolves one
key to a concrete pipeline per frame.

`MeshSource::register_unlit_family` registers the built-in unlit family;
`MeshSource::register_family` registers a caller's own, which is the same route
the built-in one takes. Other components: `Transform`, `Camera`,
`RenderLoadOps`, `InstanceColor` and the `ZSortedDrawing` marker.

`ui::UiPanel` is itself a behaviour component holding a closure, so an
interface is an entity — a frame can hold as many panels as entities, and the
`ui::UiSource` driver runs whatever panels the world carries, in query order.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `ui` | yes | the `ui` module (an egui overlay drawn as a frame source) and the `wgpu_unlit_render` egui backend it draws with |
| `winit` | yes | the `winit` module: `WindowSurface`, which presents a `Renderer` into a window's swap chain, and `input::winit::WinitInput` |

With `--no-default-features` the crate keeps the ECS components, the frame
sources, the mesh path, the pipeline abstraction and the portable `input`
module — none of which depend on egui or winit.

## Usage

The minimal frame skeleton, with the geometry and the `device`/`queue` pair
passed in. `spawn_context` puts the GPU state in the world, `MeshSource` is
mounted as a source, and `Renderer` is the frame driver:

```rust,no_run
use unlit3d::prelude::*;
use wgpu_unlit_render::pipeline::UnlitOptions;
use wgpu_unlit_render::resources::ResourceGraph;

let mut world = LocalWorld::new();
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());

let mut mesh_source = MeshSource::new(&world, ctx);
mesh_source.register_unlit_family(&world);
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
let source = spawn_source(&mut world, mesh_source);
let renderer = world.spawn((Resource, Renderer::new(ctx)));

// Geometry and materials are allocated through the source, so they land in
// the frame's resource graph; the returned handles go on the entity.
let mesh = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        source.allocate_unlit_mesh(
            &world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    })
    .unwrap();

world.spawn((Transform::default(), mesh, material, UnlitPipeline::new(key)));
```

A UI panel is a behaviour component, so mounting an interface is an ordinary
spawn:

```rust
use unlit3d::prelude::*;

world.spawn((UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),));
```

The crate-level documentation carries a complete, compiling example — spawning
the context, allocating mesh and material, binding an offscreen target and
rendering one frame. `unlit3d_examples` is a full windowed program built on
this API.

## Input

`input` is the portable half of input handling: event types and the behaviour
components that react to them, depending on neither winit nor egui. A frame
feeds events into the `InputState` resource, runs `dispatch_input` to drive the
behaviours, then calls `InputState::clear_events` once every consumer has read
them — the events are traversed read-only, never taken, because more than one
consumer sees the same frame. Translation from a windowing library's events
lives behind the corresponding feature: `input::winit::WinitInput` forwards
`WindowEvent`s, and `ui::convert` turns the crate's events into egui's.

## Tests

```text
cargo xtask test                # the whole workspace
cargo nextest run -p unlit3d    # just this crate
```

The GPU integration tests render scenes into offscreen targets and inspect the
pixels that come back. The multi-frame snapshot coverage of those scenes now
lives in [`unlit3d_examples`](../../unlit3d_examples/README.md): its scenes are
ported from the tests that used to live here, and its headless path compares
them against the SSIMULACRA2 snapshots under
[`unlit3d_asset_files`](../../unlit3d_asset_files/README.md).
Clone the submodule with `git submodule update --init`; re-bless intentional
changes with `cargo run -p unlit3d_examples --features snapshot -- --headless
--scene all --update` and review the image diff.

## See also

- [`wgpu_unlit_render`](../wgpu_unlit_render/README.md) — the renderer underneath.
- [`unlit_ecs`](../unlit_ecs/README.md) — the world the components live in.
- [`unlit3d_examples`](../../unlit3d_examples/README.md) — a runnable windowed example.
- [`docs/DESIGN.md`](../../docs/DESIGN.md) — the design document (in Chinese).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
