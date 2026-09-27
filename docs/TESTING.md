# Tests and benchmarks

English | [简体中文](TESTING.zh-CN.md)

How this repository is checked, and where each kind of check lives. The commands
themselves are listed under [Common commands](../README.md#common-commands) in
the root README.

## The test layers

The tests fall into three layers, by how close they sit to the code they check:

- **Unit tests** live inside each crate's `src/` (`#[cfg(test)]`), cover only its
  private pure logic — no GPU — and run directly under `cargo nextest run`.
- **Library integration tests** live in each crate's `tests/` and reach the
  crate through its public API only. `unlit_ecs`'s are plain ECS behaviour; the
  two rendering crates' build a headless device with
  [`unlit_wgpu_test_util`](../crates/unlit_wgpu_test_util/README.md), render a
  scene offscreen and assert on the pixels that come back, comparing against no
  stored image.
- **Snapshot tests** compare a frame (or a multi-frame sequence) against an image
  stored in the repository, using SSIMULACRA2. The low-level API's snapshots stay
  in `unlit_wgpu`'s tests (re-bless with `SNAPSHOT_UPDATE=1`); the high-level ECS
  scenes' run through
  [`unlit3d_examples`](../unlit3d_examples/README.md)'s headless mode (`--scene
  all` verifies them, `--update` re-blesses them), because the example is both
  the demo and the CI rendering check.

All snapshot baselines therefore live in
[`unlit3d_asset_files`](../unlit3d_asset_files/README.md), a git submodule; clone
it with `git submodule update --init`. After an intentional rendering change,
re-bless the affected snapshots and review the image diff before committing.

### Running them

```text
cargo xtask test                 # the whole workspace: nextest, then the doctests
cargo nextest run -p unlit_wgpu  # one crate
cargo nextest run -E 'test(name)'  # one test
```

`nextest` does not run doctests, so `cargo xtask test` follows it with
`cargo test --doc`. Filtering with `cargo nextest run` is for iterating; it is no
substitute for the task, and CI runs the task so the two cannot drift.

Per-crate notes:

| Crate | What its tests cover |
|-------|----------------------|
| [`unlit_ecs`](../crates/unlit_ecs/README.md) | World and query behaviour, deferred commands, and that `SendWorld` can be shared across threads. No GPU, so it runs anywhere. |
| [`unlit_wgpu`](../crates/unlit_wgpu/README.md) | Unit tests plus GPU integration tests that render meshes into offscreen textures and compare them against the snapshots under `tests/snapshots` (a symlink into the asset submodule). |
| [`unlit3d`](../crates/unlit3d/README.md) | Unit tests plus GPU integration tests that render scenes into offscreen targets and inspect the pixels that come back. Its multi-frame snapshot coverage lives in `unlit3d_examples`. |
| [`unlit_wgpu_test_util`](../crates/unlit_wgpu_test_util/README.md) | Nothing of its own: it is the harness the other crates' GPU tests use. |
| [`unlit3d_examples`](../unlit3d_examples/README.md) | The command line (`src/cli.rs`) and the fixed-step playback clock (`src/lib.rs`). Its rendering output is checked by the CI snapshot job, not by a `cargo test` target. |

A GPU is needed for the rendering crates' integration tests and for the example.
On a headless CI runner, Mesa's `lavapipe` serves as the software Vulkan
implementation.

## Benchmarks

[`unlit3d_benchmarks`](../unlit3d_benchmarks/Cargo.toml) holds two targets. It is
not part of the test run: a benchmark binary has no test harness, so it is
excluded from `cargo xtask test` and driven by `cargo bench` instead.

### `frame` — how fast

[Criterion](https://docs.rs/criterion) over the frame path, reported as entities
per second. It has two groups:

- **`frame`** — building one frame of a scene, at 100 through 100 000 entities,
  in both a `visible` and a `culled` variant, so a change in either the draw path
  or the culling path shows up on its own.
- **`spawn world`** — constructing the world itself, which is setup rather than
  per-frame work and is measured separately for that reason.

```text
cargo bench -p unlit3d_benchmarks
cargo bench -p unlit3d_benchmarks --bench frame -- 'frame/visible/100000'
```

Name the target (`--bench frame`) when filtering: a filter only reaches the
target it is passed to, and the `profile` target takes positional arguments of
its own, so `cargo bench -p unlit3d_benchmarks -- <filter>` runs both.

Criterion writes its reports under `target/criterion`, and compares each run
against the previous one. Treat a difference of a few percent as noise: this is
a frame benchmark on a shared machine, and the numbers move with the load on it.
Compare runs taken under similar load, and prefer a repeat over a single sample.

### `profile` — where it went

The same worlds under the `profiling` scopes, printing one line per phase per
frame. This is the target to reach for when a benchmark says a change is slower
and the question becomes which phase it was.

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- [entities] [frames]
```

Both arguments are optional and positional: an entity count runs only that case,
and a frame count overrides the default of two. Without `--features
profile-tracing` the scopes compile away — `profiling::scope!` is a no-op with no
backend — so the run reports no timings at all.

Each phase is named `module.phase`, outer to inner, so the printed span paths
read as the tree they describe. `renderer.frame` is the root; a frame of a scene
with many entities breaks down like this:

```text
renderer.frame
├── renderer.frame.build_sources
│   └── mesh_source.build
│       ├── mesh_source.metadata.upload
│       ├── mesh_source.uniforms.upload
│       ├── mesh_source.global_groups.rebuild
│       ├── scene.cull
│       ├── scene.resolve
│       │   └── scene.resolve.family
│       ├── scene.sort
│       ├── mesh_source.poses.pack
│       ├── mesh_source.poses.upload
│       ├── mesh_source.instances.upload
│       └── mesh_source.assemble
│           ├── mesh_source.assemble.handles
│           └── mesh_source.assemble.draws
├── renderer.frame.resolve_order
├── renderer.frame.record_passes
└── renderer.frame.submit
```

Read them as a breakdown, not as a promise: the scopes cover the frame's own CPU
work, so time spent inside the GPU driver or waiting on the device is not
attributed to any of them.

To pull just the phases of the `visible` cases out of a run:

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- 100000 3 \
  | awk '/^== visible/{f=1} /^== culled/{f=0} f' \
  | grep time.busy
```

## Continuous integration

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs the checks above
plus the lint gates, on every push to `main` and every pull request:

- **lint** — `typos`, `tombi lint --error-on-warnings` and `tombi format
  --check` over the TOML, and `cargo fmt --all -- --check`.
- **build** (Linux, macOS, Windows) — clippy with and without the `unlit`
  feature, `cargo build --workspace --all-targets`, `cargo xtask test`,
  `cargo doc` with `-D warnings`, and the example's headless snapshot comparison.
  Linux installs Mesa for `lavapipe`, since the runners have no GPU.
- **build-wasm** — clippy and a `wasm32-unknown-unknown` build.
- **build-android** — the `aarch64-linux-android` cross-build and the Gradle APK.

The snapshot comparison is the check that makes a rendering regression fail
rather than pass unnoticed, which is why the example's scenes carry it.
