English | [简体中文](README.zh-CN.md)

# unlit3d

[![Build](https://github.com/beicause/unlit3d/actions/workflows/ci.yml/badge.svg)](https://github.com/beicause/unlit3d/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](https://github.com/beicause/unlit3d)
[![Cargo](https://img.shields.io/crates/v/unlit3d.svg)](https://crates.io/crates/unlit3d)
[![Documentation](https://docs.rs/unlit3d/badge.svg)](https://docs.rs/unlit3d)

A compact, extensible, opinionated 3D renderer on `wgpu`, plus the ECS layer
built on top of it. It ships a built-in **Lambert** pipeline — simple
diffuse lighting from directional, point, spot and polygon lights — is
mobile-first, and runs on WebGPU as well as on the WebGL2 baseline. It uses
and exposes `wgpu` resources directly, allowing low-level control and
extension, with very little high-level CPU-side abstraction.

It targets developers and coding agents who know graphics rendering. Unlike
mainstream engines aimed at a mass audience, it does not wrap things at a high
level: you need WebGPU knowledge to use it well. It has few dependencies and
compiles quickly.

The project is at an **early stage of development**. The APIs change freely;
treat every crate as work in progress.

## Features

- **Mobile-first, and cross-platform.** Render passes are explicit — usually
  one, more when a frame composites — with transient depth and multisample
  textures, compressed vertices, and no pre-pass, compute shader or shadow path.
  The same frame loop drives a window, a headless
  offscreen target, the web examples and the Android APK.
- **Runs on WebGL2 and on WebGPU-baseline devices.** The adapter is picked at
  runtime, WebGPU first and WebGL2 when that is all the platform has. WebGL2
  lacks parts of the WebGPU baseline — no storage buffers, no `base_vertex` — so
  the shader reads the frame's arrays through `textureLoad` and a mesh's vertex
  offset is baked into its indices; a device that has the baseline keeps the
  storage buffers and the whole-buffer bindings, which cost no extra pass state.
  Which path is taken follows the adapter's own downlevel capabilities, not a
  build feature.
- **A scene the caller composes.** A frame is a sequence of passes, each an
  ordinary entity holding the views it draws into, and each pass records the
  frame sources below it, which contribute their own draws and state where in
  that pass they belong. The built-in mesh rendering, the egui overlay and a
  caller's own source have exactly the same standing.
- **A small ECS.** Borrowing OOP's focus on object state, an entity's archetype
  is immutable, systems are driven externally rather than built in, and a
  behaviour is a component holding a closure rather than a system — see
  [unlit_ecs](./crates/unlit_ecs/README.md). A renderable entity carries its
  mesh, material and pipeline as components, so game logic and drawing share
  one model.
- **A variant-driven lit pipeline.** A shader variant contains exactly the
  channels a mesh uses — position, UV, vertex color, per-instance transform and
  color, base-color texture, skinning, morph targets — and is specialized for
  the frame's target.
- **Lambert lighting, forward clustered.** A lit draw and an unlit one are the
  same variant with one flag set or cleared, over the same vertex streams and the
  same mesh group. The clusters are built on the CPU, so the shader needs no
  compute and WebGL2 runs the same path. No shadows. The mechanism is in
  [`unlit_wgpu`'s lighting section](crates/unlit_wgpu/README.md#lighting-forward-clustered-lambert).
- **Custom shaders and per-instance data.** A caller's own family registers
  through the same call as the built-in one, draws with its own pipeline — a
  hand-written `wgpu` one or a WESL module composed against the crate's public
  shader package — and owns its own per-instance vertex stream, described by the
  same `InstanceData` trait the built-in lit family implements.
- **Optional glTF loading.** The `unlit3d` `gltf` feature loads a glTF 2.0
  document into a plain record of the model — no `World`, no GPU state — and
  patches its images, materials and meshes into the resource graph of a world
  you already render, unloads them again, and spawns the entities that draw the
  document's default scene. Node hierarchies, skinned and morphed primitives,
  blended or cut-off materials, the document's animations — sampled at a
  time you choose — and `KHR_draco_mesh_compression` primitives, all come across.
  Cameras do not: the renderer draws with
  the world's own `Camera`, so the caller places it.
- **Persistent, pooled GPU resources.** Draws work with raw `wgpu` resources;
  they live across frames, are rebuilt only when needed, and share and reuse the
  buffers uploads go through.
- **CPU frustum culling and auto instancing.** Off-screen meshes cost nothing,
  and draws with matching state collapse into one instanced draw.
- **egui and portable input.** egui can be overlaid on the render or drawn into
  a texture of its own; input is unified and driven by OOP-style callbacks.
- **Deterministic and snapshot-tested in CI.** Rendering the same scene offscreen
  produces the same frame every time. CI renders every example scene and compares
  it against its stored image with SSIMULACRA2; see
  [`TESTING.md`](TESTING.md#continuous-integration) for the two tiers of jobs.
- **Built-in MCP support.** An MCP server can be embedded into an application,
  letting AI agents read and write components and take screenshots. The server
  carries no knowledge of the engine: every crate registers the tools that act
  on the types it defines.

## Crates

| Crate | Role |
|-------|------|
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | The lower-level renderer: resource graph, mesh compression, buffer pools, staging, the declarative `Scene`, the built-in lit pipeline, and an egui backend. Knows nothing about ECS. |
| [`unlit3d`](crates/unlit3d/README.md) | The high-level rendering API: ECS components, frame sources, input, UI and winit presentation, plus an optional glTF loader that patches models into another world's `MeshSource`. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The small archetype ECS the high-level layer uses: no change detection, events, relations or scheduler. It also carries the MCP server that exposes a running world over stdio, which each crate extends with its own tools. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, SSIMULACRA2 snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed example with selectable scenes, whose `tests/gpu_suite.rs` compares them against snapshots; it is also one of the two examples the Android APK packages. |
| [`unlit3d_asset_bytes`](unlit3d_examples/asset_bytes/README.md) | The models `unlit3d_examples` draws, carried in the binary: `include_bytes!` sits in an `#[inline(never)]` function body, so those bytes stay out of every dependent's metadata. |
| [`unlit3dx_asset_bytes`](unlit3dx_examples/asset_bytes/README.md) | The soundfont `unlit3dx_examples` plays, carried the same way; its `build.rs` substitutes an empty stub when the file is absent, or fails where `UNLIT3D_REQUIRE_SOUNDFONT` demands the real one. |
| [`unlit3d_cli`](unlit3d_cli/README.md) | A command-line tool that renders glTF documents to an image, driven by a JSON configuration file and overridden by its own options; its `tests/gpu_cli.rs` is the end-to-end snapshot check. |
| [`unlit3dx`](crates/unlit3dx/README.md) | The multi-world runtime: one-way jobs out, subscriptions back — snapshots for state, streams for events — joining a render sub-world with physics, audio and gameplay worlds, each on a thread of its own or stepped by the host's frames. |
| [`xtask`](xtask/README.md) | The repository task runner behind `cargo xtask`. |
| [`unlit3d_benchmarks`](unlit3d_benchmarks/README.md) | The frame-path benchmarks: Criterion throughput numbers and the phase timings the `profiling` scopes report. |

## How the pieces fit

`unlit_wgpu` is the foundation and depends on nothing in the workspace.
`unlit3d` sits on top of it and on `unlit_ecs`, keeping both as direct
dependencies: its `prelude` re-exports what most callers need, and everything
else stays reachable through its own crate path.

Two principles shape the layering: **the built-in pipeline gets no privilege**,
being composed from the same public facilities a caller's own pipeline uses; and
**frame sources are peers**, with the built-in mesh source and a caller's own
pass differing in nothing.

Each crate's README carries the design rationale for what it owns:
[`unlit_wgpu`](crates/unlit_wgpu/README.md) covers the resource graph, the
declarative scene, pipeline specialization and per-frame uploads;
[`unlit3d`](crates/unlit3d/README.md) covers the frame model, the lit
pipeline, UI and input; [`unlit_ecs`](crates/unlit_ecs/README.md) covers why the
ECS is as small as it is.

## Coding principles

- **Prefer general mechanisms over privileges for built-in features.** Design
  for generality so users can customize and extend the library. Built-in
  implementations (lit rendering, say) should not have privileges or private
  internal paths; internal implementations should move outward to keep the
  library customizable and extensible.
- **Route around a backend bug where it bites, and write down why.** A driver
  that miscompiles valid code is worked around at the code that trips it, with
  the symptom, the cause and the cost recorded in
  [`WORKAROUND.md`](WORKAROUND.md), so a later reader can tell a deliberate
  oddity from a mistake.

See [`xtask/README.md`](xtask/README.md) for the `cargo xtask` tasks and their arguments.

## Testing

The layers the tests fall into, how to run them, what each crate covers, the
benchmarks, and the CI workflows that run them are in [`TESTING.md`](TESTING.md).

## Workspace layout

```text
crates/unlit_wgpu/           the renderer, its WESL shaders and its GPU tests
crates/unlit3d/              the ECS-integrated rendering API
crates/unlit_ecs/            the archetype ECS
crates/unlit_wgpu_test_util/ the shared GPU test harness
crates/unlit3dx/             the multi-world runtime: jobs, snapshots, threaded
                             and stepped worlds
unlit3d_examples/            the windowed example, its scenes and their snapshot tests
  asset_bytes/               the glTF models it embeds, in a crate of their own
unlit3d_examples_android/    the shared library an Android activity loads
unlit3dx_examples/           the multi-world demo: five worlds, one window, a web
                             page and an Android activity
  asset_bytes/               the soundfont it embeds, in a crate of its own
unlit3dx_examples_android/   that demo's shared library for its own activity
unlit3d_cli/                 the glTF-to-image command-line tool and its end-to-end test
unlit3d_benchmarks/          the frame-path benchmarks and the profiling runs
android/                     the Gradle project that packages the examples as an APK
xtask/                       the `cargo xtask` task runner
```

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
