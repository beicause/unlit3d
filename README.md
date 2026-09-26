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

The project is at an **early stage of development**. The APIs change freely, and
the design document still lists unfinished work; treat every crate as work in
progress.

## Features

- **Mobile-first, and cross-platform.** One pass per frame, transient depth and
  multisample textures, compressed vertices, and no pre-pass, compute shader,
  lighting or shadow path. The same frame loop drives a window, a headless
  offscreen target, the web example and the Android APK.
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
| [`unlit3d`](crates/unlit3d/README.md) | The high-level rendering API: ECS components, frame sources, input, UI and winit presentation. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The small archetype ECS the high-level layer uses: no change detection, events, relations or scheduler. |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, SSIMULACRA2 snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed example with selectable scenes and a headless snapshot mode; also the Android example, packaged as an APK. |
| [`xtask`](xtask/README.md) | The repository task runner behind `cargo xtask`. Not a workspace member. |

## How the pieces fit

`unlit_wgpu` is the foundation and depends on nothing in the workspace.
`unlit3d` sits on top of it and on `unlit_ecs`, keeping both as direct
dependencies: its `prelude` re-exports what most callers need, and everything
else stays reachable through its own crate path.

Two principles shape the layering: **the built-in pipeline gets no privilege**,
being composed from the same public facilities a caller's own pipeline uses; and
**frame sources are peers**, with the built-in mesh source and a caller's own
pass differing in nothing.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the design rationale, the
architecture and the implementation plan. It is written in Chinese.

## Requirements

- A recent stable Rust toolchain (edition 2024).
- A WebGPU-capable device to run GPU tests and the example. On a headless CI
  runner, Mesa's `lavapipe` serves as the software Vulkan implementation.
- [`cargo-nextest`](https://nexte.st) for the test suite.
- [`typos`](https://github.com/crate-ci/typos) and
  [`tombi`](https://github.com/tombi-toml/tombi) to reproduce the CI lint
  steps.

## Common commands

Commands follow the `cargo xtask` convention:

- **`cargo xtask check`** — clippy over the whole workspace, all targets and all
  features (`-D warnings`), then `cargo fmt --check`. The gate before a commit.
  `--release` uses the release profile.
- **`cargo xtask test`** — `cargo nextest run` over the unit and integration
  tests, then `cargo test --doc` for the doctests nextest does not cover.
  `--release` uses the release profile.
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
- **`typos`** — spell check, over the whole repository.
- **`tombi lint --error-on-warnings`** and **`tombi format`** — TOML lint and
  format check. Run them after touching any `Cargo.toml`.

## Tests

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
  in `unlit_wgpu`'s tests (re-bless with `SNAPSHOT_UPDATE=1`); the
  high-level ECS scenes' run through
  [`unlit3d_examples`](unlit3d_examples/README.md)'s headless mode (`--scene all`
  verifies them, `--update` re-blesses them), because the example is both the
  demo and the CI rendering check.

All snapshot baselines therefore live in
[`unlit3d_asset_files`](unlit3d_asset_files/README.md), a git submodule; clone it
with `git submodule update --init`. After an intentional rendering change,
re-bless the affected snapshots and review the image diff before committing.

## Workspace layout

```text
crates/unlit_wgpu/           the renderer, its WESL shaders and its GPU tests
crates/unlit3d/              the ECS-integrated rendering API
crates/unlit_ecs/            the archetype ECS
crates/unlit_wgpu_test_util/ the shared GPU test harness
unlit3d_examples/            the windowed example, its scenes and its snapshot runner
android/                     the Gradle project that packages the example as an APK
xtask/                       the `cargo xtask` task runner (excluded from the workspace)
docs/DESIGN.md               the design document
```

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
