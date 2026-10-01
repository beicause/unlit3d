English | [简体中文](README.zh-CN.md)

# unlit3d

[![Build](https://github.com/beicause/unlit3d/actions/workflows/ci.yml/badge.svg)](https://github.com/beicause/unlit3d/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](https://github.com/beicause/unlit3d)
[![Cargo](https://img.shields.io/crates/v/unlit3d.svg)](https://crates.io/crates/unlit3d)
[![Documentation](https://docs.rs/unlit3d/badge.svg)](https://docs.rs/unlit3d)

A compact, extensible, opinionated 3D renderer for WebGPU, plus the ECS layer
built on top of it. It ships a built-in **unlit** pipeline, is mobile-first, and
does not support WebGL or GLES. It uses and exposes `wgpu` resources directly,
allowing low-level control and extension, with very little high-level
CPU-side abstraction.

It targets developers and coding agents who know graphics rendering. Unlike
mainstream engines aimed at a mass audience, it does not wrap things at a high
level: you need WebGPU knowledge to use it well.

The project is at an **early stage of development**. The APIs change freely;
treat every crate as work in progress.

<details>
<summary>What this project optimizes for, and what it deliberately leaves out</summary>

- **Lightweight and customizable.** No heavy dependencies, fast compile times,
  friendly to AI agents. A streamlined ECS paradigm borrowing from OOP, using
  behavior components more than systems and putting the focus on objects.
- **Mobile and web optimized.** Transient MSAA and depth textures by default,
  compressed vertex attributes, unlit materials by default, a single pass for
  rendering. None of the prepass, PBR lighting or shadows that are expensive on
  mobile, and no compute shaders.
- **Low-level.** `wgpu` resources used directly, buffers manipulated directly.
  Render resources persist across frames, and a resource graph rebuilds them only
  when necessary.
- **Deterministic rendering.** First-class headless rendering and automated CI
  snapshot tests.

Not supported: lighting and shadows, and post-processing.

</details>

## Features

- **Mobile-first, and cross-platform.** One pass per frame, transient depth and
  multisample textures, compressed vertices, and no pre-pass, compute shader,
  lighting or shadow path. The same frame loop drives a window, a headless
  offscreen target, the web example and the Android APK.
- **Runs on WebGL2 and on WebGPU-baseline devices.** The adapter is picked at
  runtime, WebGPU first and WebGL2 when that is all the platform has. WebGL2
  lacks parts of the WebGPU baseline — no storage buffers, no `base_vertex` — so
  the shader reads the frame's arrays through `textureLoad` and a mesh's vertex
  offset is baked into its indices; a device that has the baseline keeps the
  storage buffers and the whole-buffer bindings, which cost no extra pass state.
  Which path is taken follows the adapter's own downlevel capabilities, not a
  build feature.
- **A scene the caller composes.** A frame is assembled from frame sources that
  contribute their own draws and state where in the frame they belong. The
  built-in mesh rendering, the egui overlay and a caller's own pass have exactly
  the same standing.
- **A small ECS.** Borrowing OOP's focus on object state, an entity's archetype
  is immutable, systems are driven externally rather than built in, and a
  behaviour is a component holding a closure rather than a system — see
  [unlit_ecs](./crates/unlit_ecs/README.md). A renderable entity carries its
  mesh, material and pipeline as components, so game logic and drawing share
  one model.
- **A variant-driven unlit pipeline.** A shader variant contains exactly the
  channels a mesh uses — position, UV, vertex color, per-instance transform and
  color, base-color texture, skinning, morph targets — and is specialized for
  the frame's target.
- **Optional glTF loading.** The `unlit3d` `gltf` feature loads a glTF 2.0
  document into a plain record of the model — no `World`, no GPU state — and
  patches its images, materials and meshes into the resource graph of a world
  you already render, unloads them again, and spawns the entities that draw the
  document's default scene. Node hierarchies, skinned and morphed primitives,
  blended or cut-off materials, and the document's animations — sampled at a
  time you choose — all come across. Cameras do not: the renderer draws with
  the world's own `Camera`, so the caller places it.
- **Persistent, pooled GPU resources.** Draws work with raw `wgpu` resources;
  they live across frames, are rebuilt only when needed, and share and reuse the
  buffers uploads go through.
- **CPU frustum culling and auto instancing.** Off-screen meshes cost nothing,
  and draws with matching state collapse into one instanced draw.
- **egui and portable input.** egui can be overlaid on the render or drawn into
  a texture of its own; input is unified and driven by OOP-style callbacks.
- **Deterministic and snapshot-tested in CI.** Rendering the same scene offscreen
  produces the same frame every time. Each desktop platform lints, builds and
  tests the workspace, then renders every example scene and compares it against
  its stored image with SSIMULACRA2; the wasm and Android builds are two more
  jobs.

## Crates

| Crate | Role |
|-------|------|
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | The lower-level renderer: resource graph, mesh compression, buffer pools, staging, the declarative `Scene`, the built-in unlit pipeline, and an egui backend. Knows nothing about ECS. |
| [`unlit3d`](crates/unlit3d/README.md) | The high-level rendering API: ECS components, frame sources, input, UI and winit presentation, plus an optional glTF loader that patches models into another world's `MeshSource`. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The small archetype ECS the high-level layer uses: no change detection, events, relations or scheduler. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, SSIMULACRA2 snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed example with selectable scenes, whose `tests/gpu_scenes.rs` compares them against snapshots; also the Android example, packaged as an APK. |
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
[`unlit3d`](crates/unlit3d/README.md) covers the frame model, the unlit
pipeline, UI and input; [`unlit_ecs`](crates/unlit_ecs/README.md) covers why the
ECS is as small as it is.

## Coding principles

- **Prefer general mechanisms over privileges for built-in features.** Design
  for generality so users can customize and extend the library. Built-in
  implementations (unlit rendering, say) should not have privileges or private
  internal paths; internal implementations should move outward to keep the
  library customizable and extensible.

## Common commands

Commands follow the `cargo xtask` convention:

- **`cargo xtask check`** — clippy over the whole workspace, all targets and all
  features (`-D warnings`), then `cargo fmt --check`. The gate before a commit.
  `--release` uses the release profile.
- **`cargo xtask test`** — `cargo nextest run` over the unit and integration
  tests, then `cargo test --doc` for the doctests nextest does not cover.
  `--release` uses the release profile.
- **`cargo xtask test-wasm`** — build the GPU tests for
  `wasm32-unknown-unknown`, bindgen them, and run them in a real browser through
  WebGL2, one page per test; then load the example and check it drew something.
  Needs `wasm-bindgen-cli` at the version the workspace resolves,
  `cargo nextest`, and Node; it installs the runner's own dependencies and a
  browser, unless `CHROME_PATH` names one to reuse. `--show` opens a visible
  window to watch a failure happen.
- **`cargo xtask run-wasm`** — build the web example and serve it from a built-in
  static server (WebGPU needs a secure context; `file://` will not do).
  `--no-serve` only builds it, and `--release` uses the release profile.
- **`cargo xtask build-android`** — cross-compile the example's shared library
  with `cargo ndk` into `android/app/src/main/jniLibs`, then run Gradle to build
  the APK. Debug by default; `--release` builds an unsigned release APK. Needs
  JDK 17+ and `ANDROID_HOME` (cargo-ndk finds the NDK by itself).
- **`cargo xtask publish`** — upload the publishable crates (`unlit_ecs`,
  `unlit_wgpu`, `unlit3d`) to crates.io in dependency order, waiting for each to
  reach the index before packaging the next. `--dry-run` runs every check
  without uploading.
- **`cargo nextest run`** — use it directly to filter or re-run individual tests
  (`-p <crate>`, `-E 'test(<name>)'`). nextest does not run doctests, so it is no
  substitute for `cargo xtask test`.
- **`cargo bench -p unlit3d_benchmarks`** — time a frame of the rendering path as
  entities per second. `--bench ecs` instead measures the ECS's component-access
  paths on their own, and `--bench profile -- <entities> <frames>` with
  `--features profile-tracing` prints one line per frame phase. See
  [`unlit3d_benchmarks`](unlit3d_benchmarks/README.md) for what each target
  covers.
- **`typos`** — spell check, over the whole repository.
- **`tombi lint --error-on-warnings`** and **`tombi format`** — TOML lint and
  format check. Run them after touching any `Cargo.toml`.

## Tests and benchmarks

The tests fall into three layers, by how close they sit to the code they check:

- **Unit tests** live inside each crate's `src/` (`#[cfg(test)]`), cover only its
  private pure logic — no GPU — and run directly under `cargo nextest run`.
- **Library integration tests** live in each crate's `tests/` and reach the
  crate through its public API only. `unlit_ecs`'s are plain ECS behaviour; the
  two rendering crates' build a headless device with
  [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md), render a scene
  offscreen and assert on the pixels that come back, comparing against no stored
  image.
- **Snapshot tests** compare a frame (or a multi-frame sequence) against an image
  stored in the repository, using SSIMULACRA2. The low-level API's snapshots stay
  in `unlit_wgpu`'s tests; the high-level ECS scenes' run through
  [`unlit3d_examples`](unlit3d_examples/README.md)'s `tests/gpu_scenes.rs`,
  because the example is both the demo and the CI rendering check. Either is
  re-blessed with `SNAPSHOT_UPDATE=1`.

All snapshot baselines therefore live in
[`unlit3d_asset_files`](unlit3d_asset_files/README.md), a git submodule. It is
marked `update = none`, so a project that depends on this one through git fetches
no snapshots it would never compare against; clone it here, where the tests do use
it, with `git submodule update --init --checkout`. After an intentional rendering
change, re-bless the affected snapshots and review the image diff before
committing.
Because the baselines come from one GPU stack, another platform's driver can
score a scene below the threshold with nothing wrong; a frame that does not match
is therefore written before the test fails, and CI uploads those directories as
artifacts. The WebGL2 tier still runs after the default one fails.

### Running them

```text
cargo xtask test                 # the whole workspace: nextest, then the doctests
cargo nextest run -p unlit_wgpu  # one crate
cargo nextest run -E 'test(name)'  # one test
cargo xtask test-wasm            # the GPU tests again, in a browser
```

`nextest` does not run doctests, so `cargo xtask test` follows it with
`cargo test --doc`. Filtering with `cargo nextest run` is for iterating; it is no
substitute for the task, and CI runs the task so the two cannot drift.

`cargo xtask test-wasm` runs the rendering crates' GPU tests a second time, on
`wasm32-unknown-unknown` in a real browser through WebGL2. The test files are the
same ones; a `harness = false` test target is itself the wasm module, and the
harness's macros give it the export the page calls. What the browser presents is
the part no headless device can — a real WebGL2 implementation, with its own
limits, its own shader translation and its own rasterizer. Because that
rasterizer is not the one the snapshots were captured on, a `Tolerance` can name
how far a frame may differ; the cube snapshots use it, and carry the
measurements behind their bound.

It also starts the example in the browser and checks that it drew a shaded image
rather than a blank canvas. The GPU tests never start the example, so without
this a change that compiles and still leaves the page empty would pass them.

Per-crate notes:

| Crate | What its tests cover |
|-------|----------------------|
| [`unlit_ecs`](crates/unlit_ecs/README.md) | World and query behaviour and deferred commands. |
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | Unit tests plus GPU integration tests that render meshes into offscreen textures and compare them against the snapshots under `tests/snapshots` (a symlink into the asset submodule). |
| [`unlit3d`](crates/unlit3d/README.md) | Unit tests plus GPU integration tests that render scenes into offscreen targets and inspect the pixels that come back. Its multi-frame snapshot coverage lives in `unlit3d_examples`. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | How the harness itself behaves — the snapshot comparison, and counting the pixels that differ. The GPU tests it makes possible live in the crates above. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | The command line (`src/cli.rs`) and the fixed-step playback clock (`src/lib.rs`). Its rendering output is checked by the CI snapshot job, not by a `cargo test` target. |

A GPU is needed for the rendering crates' integration tests and for the example.
On a headless CI runner, Mesa's `lavapipe` serves as the software Vulkan
implementation. The wasm pass needs none: the browser falls back to software
WebGL2.

## Benchmarks

[`unlit3d_benchmarks`](unlit3d_benchmarks/README.md) holds three targets: `frame`
for throughput, `ecs` for what one component read costs, and `profile` for a
frame's phase breakdown. It is not part of
the test run: a benchmark binary has no test harness, so it is excluded from
`cargo xtask test` and driven by `cargo bench` instead. What each target covers,
and how to read its output, is in
[that crate's README](unlit3d_benchmarks/README.md).

## Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs the checks above plus
the lint gates, on every push to `main` and every pull request:

- **lint** — `typos`, `tombi lint --error-on-warnings` and `tombi format
  --check` over the TOML, and `cargo fmt --all -- --check`.
- **build** (Linux, macOS, Windows) — clippy with and without the `unlit`
  feature, `cargo build --workspace --all-targets`, `cargo xtask test`,
  `cargo doc` with `-D warnings`, and the workspace's snapshot comparisons
  twice: once at the WebGPU baseline's limits, once with every offscreen device
  narrowed to WebGL2's shape. Linux installs Mesa for `lavapipe`, since the
  runners have no GPU.
- **build-wasm** — clippy and a `wasm32-unknown-unknown` build. The build
  excludes `xtask`, a host-only task runner whose HTTP server does not compile
  for that target.
- **build-android** — the `aarch64-linux-android` cross-build and the Gradle APK.

The snapshot comparisons are the check that makes a rendering regression fail
rather than pass unnoticed, which is why the example's scenes carry them. Their
second run is what reaches the WebGL2 paths on a runner whose own adapter has
storage buffers and `base_vertex`, by setting `UNLIT3D_DEVICE_TIER=webgl2`
rather than by building for the web. `build-wasm` runs the same comparisons in a
real browser, through the same test bodies.

A frame that fails a comparison is written before the test fails, so a CI run
that fails leaves it behind: each pass writes its own directory under
`target/`, and the jobs upload them as artifacts. A mismatch is often the
platform showing through rather than a regression, so having the frame is what
makes the difference something to look at rather than to infer from a score.

## Workspace layout

```text
crates/unlit_wgpu/           the renderer, its WESL shaders and its GPU tests
crates/unlit3d/              the ECS-integrated rendering API
crates/unlit_ecs/            the archetype ECS
crates/unlit_wgpu_test_util/ the shared GPU test harness
unlit3d_examples/            the windowed example, its scenes and their snapshot tests
unlit3d_benchmarks/          the frame-path benchmarks and the profiling runs
android/                     the Gradle project that packages the example as an APK
xtask/                       the `cargo xtask` task runner
```

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
