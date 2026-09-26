English | [简体中文](README.zh-CN.md)

# unlit3d

A compact, opinionated 3D renderer for **unlit** draws on WebGPU, plus the ECS
layer built on top of it. The workspace targets WebGPU (and the native backends
behind it) with a mobile-first bias; WebGL and GLES are not supported. It draws
a whole scene — opaque and transparent instances alike — in a single render
pass into one `wgpu::TextureView`, with CPU frustum culling and a first-class
headless path for CI snapshot testing.

The project is at an **early stage of development**. The APIs change freely, and
the design document still lists unfinished work; treat every crate as work in
progress.

## Crates

| Crate | Role |
|-------|------|
| [`wgpu_unlit_render`](crates/wgpu_unlit_render/README.md) | The lower-level renderer: resource graph, mesh compression, buffer pools, staging, the declarative `Scene`, the built-in unlit pipeline, and an egui backend. Knows nothing about ECS. |
| [`unlit3d`](crates/unlit3d/README.md) | The high-level rendering API: ECS components, frame sources, the mesh source with pipeline families, input, UI overlay, and winit presentation. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The archetype ECS the high-level layer is written against. Deliberately small: no change detection, hooks, events, relations or scheduler. |
| [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, and optional SSIMULACRA2 image snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed example with selectable scenes — and its own headless snapshot mode. Also the Android example, packaged as an APK. |
| [`xtask`](xtask/README.md) | The repository task runner behind `cargo xtask`. Not a workspace member. |

## How the pieces fit

`wgpu_unlit_render` is the foundation and depends on nothing in the workspace.
`unlit3d` sits on top of it and on `unlit_ecs`, and keeps both as direct
dependencies rather than re-exporting them wholesale: its `prelude` re-exports
the items most callers need, and everything else stays reachable through its
own crate path. `wgpu_unlit_test_util` is a dev-dependency of the two rendering
crates; `unlit3d_examples` uses it only behind its `snapshot` feature.

Two principles shape the layering:

- **The built-in pipeline gets no privilege.** Everything the unlit pipeline
  uses — binding slots, vertex compression, resource tracking, the variant
  cache — is public, and the pipeline is composed from the same facilities a
  caller's own pipeline would use.
- **Frame sources are peers.** A frame is assembled from several
  `unlit3d::source::FrameSource` implementations that each produce a
  `wgpu_unlit_render::scene::Scene`; built-in mesh rendering is one source and
  a caller's own pass is another, with no less privilege.

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
- **`cargo nextest run`** — use it directly to filter or re-run individual tests
  (`-p <crate>`, `-E 'test(<name>)'`). nextest does not run doctests, so it is no
  substitute for `cargo xtask test`.
- **`typos`** — spell check, over the whole repository.
- **`tombi lint --error-on-warnings`** and **`tombi format`** — TOML lint and
  format check. Run them after touching any `Cargo.toml`.

## Tests

The tests fall into three layers, by how close they sit to the code they check:

- **Unit tests** live inside each crate's `src/` (`#[cfg(test)]`) and cover only
  that crate's private, pure logic: no GPU, no `LocalWorld`. They run directly
  under `cargo nextest run`.
- **Library integration tests** live in each crate's `tests/` and reach the
  crate through its public API only. `unlit_ecs`'s are plain ECS behaviour;
  the two rendering crates' are GPU tests that build a headless device with
  [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.md), render a
  scene offscreen and assert on the pixels that come back — but compare against
  no stored image.
- **Snapshot tests** are the subset of integration tests that compare a frame
  (or a multi-frame sequence) against an image stored in the repository, using
  the SSIMULACRA2 perceptual metric to freeze the rendering result. The rule is:
  the **low-level API's snapshots stay in `wgpu_unlit_render`'s tests**
  (`tests/snapshots` links in the submodule; re-bless with `SNAPSHOT_UPDATE=1`),
  while the **high-level ECS scenes' snapshots run through
  [`unlit3d_examples`](unlit3d_examples/README.md)'s headless mode** (`--scene
  all` verifies them, `--update` re-blesses them), because the example is both
  the demo and the CI rendering check and those scenes should not be maintained
  twice.

All snapshot baselines therefore live in
[`unlit3d_asset_files`](unlit3d_asset_files/README.md), a
git submodule. Clone it with `git submodule update --init`. After an
intentional rendering change, re-bless the affected snapshots the way each layer
above describes, and review the image diff before committing.

## Workspace layout

```text
crates/wgpu_unlit_render/   the renderer, its WESL shaders and its GPU tests
crates/unlit3d/             the ECS-integrated rendering API
crates/unlit_ecs/           the archetype ECS
crates/wgpu_unlit_test_util/ the shared GPU test harness
unlit3d_examples/           the windowed example, its scenes and its snapshot runner
android/                    the Gradle project that packages the example as an APK
xtask/                      the `cargo xtask` task runner (excluded from the workspace)
docs/DESIGN.md              the design document
```

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
