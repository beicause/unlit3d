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
| [`unlit3d`](crates/unlit3d/README.md) | The upper rendering API: ECS components, frame sources, the mesh source with pipeline families, input, UI overlay, and winit presentation. |
| [`unlit_ecs`](crates/unlit_ecs/README.md) | The archetype ECS the upper layer is written against. Deliberately small: no change detection, hooks, events, relations or scheduler. |
| [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.md) | The headless GPU test harness: device setup, buffer and texture readback, and optional SSIMULACRA2 image snapshots. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | A windowed unlit cube with an egui overlay — and its own headless capture mode. Also the Android example, packaged as an APK. |
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

```text
cargo xtask check          # clippy over the whole workspace, then `cargo fmt --check`
cargo xtask test           # nextest over unit and integration tests, then the doctests
cargo xtask run-wasm       # build the web example and serve it on localhost
cargo xtask build-android  # build the Android example's library and APK
```

`cargo xtask check` and `cargo xtask test` both accept `--release`, as does
`cargo xtask build-android`, which additionally needs an Android SDK, an NDK
and a JDK 17 or newer. Lint the TOML with `tombi lint --error-on-warnings` and
check spelling with `typos`.

The renderer's GPU tests compare frames against images in
[`wgpu_unlit_render_asset_files`](wgpu_unlit_render_asset_files/README.md), a
git submodule linked in as `tests/snapshots` in each test crate. Clone it with
`git submodule update --init`, and re-bless a snapshot you intentionally
changed with `SNAPSHOT_UPDATE=1`.

## Workspace layout

```text
crates/wgpu_unlit_render/   the renderer, its WESL shaders and its GPU tests
crates/unlit3d/             the ECS-integrated rendering API
crates/unlit_ecs/           the archetype ECS
crates/wgpu_unlit_test_util/ the shared GPU test harness
unlit3d_examples/           the windowed example and its capture mode
android/                    the Gradle project that packages the example as an APK
xtask/                      the `cargo xtask` task runner (excluded from the workspace)
docs/DESIGN.md              the design document
```

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
