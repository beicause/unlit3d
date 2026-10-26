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
  compressed vertex attributes, unlit materials by default, and a short,
  explicit list of render passes. None of the prepass, PBR lighting or shadows
  that are expensive on mobile, and no compute shaders.
- **Low-level.** `wgpu` resources used directly, buffers manipulated directly.
  Render resources persist across frames, and a resource graph rebuilds them only
  when necessary.
- **Deterministic rendering.** First-class headless rendering and automated CI
  snapshot tests.

Not supported: lighting and shadows, and post-processing.

</details>

## Features

- **Mobile-first, and cross-platform.** Render passes are explicit — usually
  one, more when a frame composites — with transient depth and multisample
  textures, compressed vertices, and no pre-pass, compute shader, lighting or
  shadow path. The same frame loop drives a window, a headless
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
- **A variant-driven unlit pipeline.** A shader variant contains exactly the
  channels a mesh uses — position, UV, vertex color, per-instance transform and
  color, base-color texture, skinning, morph targets — and is specialized for
  the frame's target.
- **Custom shaders and per-instance data.** A caller's own family registers
  through the same call as the built-in one, draws with its own pipeline — a
  hand-written `wgpu` one or a WESL module composed against the crate's public
  shader package — and owns its own per-instance vertex stream, described by the
  same `InstanceData` trait the built-in unlit family implements.
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
- **Built-in MCP support.** An MCP server can be embedded into an application,
  letting AI agents read and write components and take screenshots. The server
  carries no knowledge of the engine: every crate registers the tools that act
  on the types it defines.

## Crates

| Crate | Role |
|-------|------|
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | The lower-level renderer: resource graph, mesh compression, buffer pools, staging, the declarative `Scene`, the built-in unlit pipeline, and an egui backend. Knows nothing about ECS. |
| [`unlit3d`](crates/unlit3d/README.md) | The high-level rendering API: ECS components, frame sources, input, UI and winit presentation, plus an optional glTF loader that patches models into another world's `MeshSource`. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The small archetype ECS the high-level layer uses: no change detection, events, relations or scheduler. It also carries the MCP server that exposes a running world over stdio, which each crate extends with its own tools. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, SSIMULACRA2 snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed example with selectable scenes, whose `tests/gpu_suite.rs` compares them against snapshots; it is also one of the two examples the Android APK packages. |
| [`unlit3d_asset_bytes`](unlit3d_examples/asset_bytes/README.md) | The models `unlit3d_examples` draws, carried in the binary: `include_bytes!` sits in an `#[inline(never)]` function body, so those bytes stay out of every dependent's metadata. |
| [`unlit3dx_asset_bytes`](unlit3dx_examples/asset_bytes/README.md) | The soundfont `unlit3dx_examples` plays, carried the same way; its `build.rs` substitutes an empty stub when the file is absent. |
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

- **`cargo xtask check`** — clippy over the whole workspace, with the default
  features and then with all of them, plus each package on its own with the
  default features (`-D warnings`); then the doc build with every feature and
  `-D warnings`, so a broken intra-doc link fails here as it would in CI; then
  `cargo fmt --check`. The gate before a commit. The per-package pass is what
  catches an optional feature a crate's own code no longer uses, which a
  workspace-wide run hides by unifying features. `--release` uses the release
  profile.
- **`cargo xtask test`** — `cargo nextest run` over the unit and integration
  tests, then `cargo test --doc` for the doctests nextest does not cover.
  `--release` uses the release profile.
- **`cargo xtask test-wasm`** — build the GPU tests for
  `wasm32-unknown-unknown`, bindgen them, and run them in a real browser through
  WebGL2, one page per test; then load each example and check it drew
  something. Needs `wasm-bindgen-cli` at the version the workspace resolves,
  `cargo nextest`, and Node; it installs the runner's own dependencies and a
  browser, unless `CHROME_PATH` names one to reuse. `--show` opens a visible
  window to watch a failure happen.
- **`cargo xtask run-wasm`** — build the web examples and serve them from a
  built-in static server (WebGPU needs a secure context; `file://` will not
  do). `--app` picks which are built — `unlit3d`, `unlit3dx` or `all`, the
  default — and the landing page at the root links whichever are there;
  `--no-serve` only builds. Every wasm build, here and in `test-wasm`, uses
  the workspace's `wasm-dev` cargo profile: release's settings at an
  `opt-level` aimed at size, since a browser downloads the binary.
- **`cargo xtask build-android`** — cross-compile both examples' shared
  libraries (`unlit3d_examples_android` and `unlit3dx_examples_android`, which
  hold the Android entry points and nothing else) with `cargo ndk` into
  `android/app/src/main/jniLibs`, then run Gradle to build the APK. Debug by
  default; `--release` builds an unsigned release APK. Needs JDK 17+ and
  `ANDROID_HOME` (cargo-ndk finds the NDK by itself).
- **`cargo xtask publish`** — upload the publishable crates (`unlit_ecs`,
  `unlit_wgpu`, `unlit3d`, `unlit3dx`) to crates.io in dependency order,
  waiting for each to reach the index before packaging the next. Arguments
  after `--` are passed through to each `cargo publish`, so `-- --dry-run`
  runs every check without uploading.
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
  [`unlit3d_examples`](unlit3d_examples/README.md)'s `tests/gpu_suite.rs`,
  because the example is both the demo and the CI rendering check; and
  [`unlit3d_cli`](unlit3d_cli/README.md)'s `tests/gpu_cli.rs` renders the same
  reference glTF scene through the command-line tool's library and compares it
  against the same snapshots. Either is re-blessed with `SNAPSHOT_UPDATE=1`.

Each crate's integration tests are **one binary**, not one per source file: the
files under `tests/` are modules of the suite file beside them (`gpu_suite.rs`,
or `suite.rs` where the tests are plain `libtest`), pulled in with `#[path]`.
They share the crate's code and, on the GPU side, the shared harness, so
splitting them into targets meant linking that code once per file — thirty
binaries across the workspace, each carrying its own copy of the same runtime.
The binary's name is the crate's, and its `required-features` are the features
its cases need, so a crate whose suite needs an optional feature runs it only
when that feature is on.

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
cargo nextest run -p unlit_wgpu --all-features  # one crate
cargo nextest run -E 'test(name)'  # one test
cargo xtask test-wasm            # the GPU tests again, in a browser
```

A suite binary requires the features its cases need, so a crate run without them
builds it out: `cargo nextest run -p unlit3d` on its own would run the crate's
unit tests and skip the GPU suite, which needs `gltf`, `mcp` and `ui`.

`nextest` does not run doctests, so `cargo xtask test` follows it with
`cargo test --doc`. Filtering with `cargo nextest run` is for iterating; it is no
substitute for the task, and CI runs the task so the two cannot drift.

`cargo xtask test-wasm` runs the rendering crates' GPU tests a second time, on
`wasm32-unknown-unknown` in a real browser through WebGL2. The test files are the
same ones: the suite target is itself the wasm module, and the single
`gpu_test_main!` at the suite's root gives it the export the page calls. What the
browser presents is
the part no headless device can — a real WebGL2 implementation, with its own
limits, its own shader translation and its own rasterizer. Because that
rasterizer is not the one the snapshots were captured on, a `Tolerance` can name
how far a frame may differ; the cube snapshots use it, and carry the
measurements behind their bound.

It also starts each example in the browser and checks that it drew a shaded
image rather than a blank canvas. The GPU tests never start them, so without
this a change that compiles and still leaves a page empty would pass them.

Per-crate notes:

| Crate | What its tests cover |
|-------|----------------------|
| [`unlit_ecs`](crates/unlit_ecs/README.md) | World and query behaviour, deferred commands, and the MCP server's ECS tools. |
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | Unit tests plus GPU integration tests that render meshes into offscreen textures and compare them against the snapshots under `tests/snapshots` (a symlink into the asset submodule). |
| [`unlit3d`](crates/unlit3d/README.md) | Unit tests plus GPU integration tests that render scenes into offscreen targets and inspect the pixels that come back. An end-to-end MCP test builds a real GPU world on a thread, serves it over a pair of streams, and speaks JSON-RPC to it: it lists the tools, reads the world, spawns a mesh and checks the screenshot pixel by pixel. Its multi-frame snapshot coverage lives in `unlit3d_examples`. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | How the harness itself behaves — the snapshot comparison, and counting the pixels that differ. The GPU tests it makes possible live in the crates above. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | The command line (`src/cli.rs`) and the fixed-step playback clock (`src/lib.rs`). Its rendering output is checked by the CI snapshot job, not by a `cargo test` target. |
| [`unlit3d_cli`](unlit3d_cli/README.md) | The argument parser and configuration merging, plus the end-to-end render test that reproduces the example's reference glTF scene and compares it against the stored snapshots. |
| [`unlit3dx`](crates/unlit3dx/README.md) | The multi-world core: jobs reaching their world, snapshot and stream subscriptions, the state a world publishes about itself, the render bridge's failure paths, and the physics, audio and MCP bridges end to end. |
| [`unlit3dx_examples`](unlit3dx_examples/README.md) | The demo's scene logic as unit tests, and an end-to-end headless run whose captured frames and audio mix compare against the snapshots under the asset submodule; the same worlds also run stepped in a browser and behind an Android activity. |

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

The checks above plus the lint gates run in two tiers, split over two workflows
so that the jobs they share are written once:
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) is the light tier, and
[`.github/workflows/ci-main.yml`](.github/workflows/ci-main.yml) is the full one,
which calls the light tier's jobs with `full` set and adds what only a release
needs. Which tier a run gets is decided by the ref it runs on, not by what
started it: `main` and the merge queue, a `main` run whatever ref it names, take
the whole thing, and every other branch takes the light one. That covers a pull
request from the branch and also a branch pushed with no pull request at all, so
pushing is enough to have a change compiled and tested; opening the pull request
is not what starts the build.

The light tier runs what a change can break on its own, within the time a
reviewer spends reading it: one platform, and for the web and for Android the
lint rather than the browser or the APK around it. `main` then runs the rest,
which is the part that proves a release ships.

Every branch other than `main`, whether pushed on its own or opened as a pull
request:

- **lint** — `typos`, `tombi lint --error-on-warnings` and `tombi format
  --check` over the TOML, and `cargo fmt --all -- --check`.
- **build** (Linux) — clippy with and without the `unlit` feature,
  `cargo build --workspace --all-targets`, `cargo xtask test`, `cargo doc`
  with `-D warnings`, and the workspace's snapshot comparisons twice: once at
  the WebGPU baseline's limits, once with every offscreen device narrowed to
  WebGL2's shape. Linux installs Mesa for `lavapipe`, since the runners have no
  GPU.
- **check-wasm** — clippy for `wasm32-unknown-unknown` over the workspace and
  over the test binaries. It excludes `xtask`, a host-only task runner whose HTTP
  server does not compile for that target.
- **check-android** — clippy for `aarch64-linux-android`, the only thing that
  compiles the code behind `cfg(target_os = "android")`. The host clippy in
  `build` never reaches it.

`main` — every push to it, a merge queue run, and a manual run from it:

- **build** runs on macOS and Windows as well as Linux.
- **build-wasm** — the same two clippy passes over the `wasm32-unknown-unknown`
  build, then every wasm test binary bindgen'd into a page driven through a real
  browser. Linting those builds here is why `check-wasm` does not run on `main`.
- **build-android** — the `aarch64-linux-android` clippy, then the cross-build of
  the shared libraries and Gradle around it for the APK. It runs that clippy
  itself, so `check-android` does not run on `main` either.

Each `check-*` job shares its cache with the job that does the same subject on
`main` (`shared-key` on Swatinem/rust-cache), so what the light tier compiles is
what the merge restores instead of recompiling. The two workflows set the same
`env` block for that reason: rust-cache hashes every `CARGO*` and `RUST*`
variable into the key, so a variable set on one side and not on the other splits
the cache it was meant to share. Every job also saves its caches even when it
fails (`cache-on-failure`), so an iteration on a red branch still leaves the next
run warmer than a cold one would be.

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

[`.github/workflows/pages.yml`](.github/workflows/pages.yml) publishes the site
to GitHub Pages on every push to `main`: a landing page at the root linking the
two web examples and the API docs, each example in a directory of its own
beside it, and the workspace's rustdoc under `docs/`. The `unlit3d` example's
corner links to those docs as well. The docs build on nightly, because the
page listing the crates and the `doc(cfg(...))` labels are nightly-only;
`ci.yml` keeps checking them on stable so a break is caught on the branch
rather than at the deploy. That check is where a doc warning fails a build — the
deploy publishes the pages it has, since a page with a broken link is worth more
than a site held back by it.

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
