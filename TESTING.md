# Testing

English | [简体中文](TESTING.zh-CN.md)

How this workspace checks itself: the layers the tests fall into, how to run
them, what each crate covers, and the CI workflows that run them. The benchmarks
are here too, since they measure the same frame path the tests exercise.

## Test layers

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

Inside the unit layer, some properties are checked against generated inputs rather
than written cases: `proptest` drives the offset allocator (offsets stay aligned and
disjoint, and a request that fits the largest free region is granted), the dependency
graph (every edge it accepts lands in dependency order, and a sweep leaves the
survivors ordered) and mesh compression (positions and directions round-trip within
one quantization step, and indices narrow exactly when they fit). A failing input is
written to the crate's `proptest-regressions/` so the same case runs again; those
files are committed, since the seed is the test case.

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
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d_asset_files/blob/main/README.md),
a git submodule. It is marked `update = none`, so a project that depends on this
one through git fetches no snapshots it would never compare against; clone it
here, where the tests do use it, with
`git submodule update --init --checkout`. After an intentional rendering change,
re-bless the affected snapshots and review the image diff before committing.
Because the baselines come from one GPU stack, another platform's driver can
score a scene below the threshold with nothing wrong; a frame that does not match
is therefore written before the test fails, and CI uploads those directories as
artifacts. The WebGL2 tier still runs after the default one fails.

## Running them

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
`wasm32-unknown-unknown` in a real browser, on both of the browser's GPU
backends. The test files are the same ones: the suite target is itself the wasm
module, and the single `gpu_test_main!` at the suite's root gives it the export
the page calls. What the browser presents is the part no headless device can —
an implementation with its own limits, its own shader translation and its own
rasterizer. Because that rasterizer is not the one the snapshots were captured
on, a `Tolerance` can name how far a frame may differ; the cube snapshots use
it, and carry the measurements behind their bound.

It also starts each example in the browser and checks that it drew a shaded
image rather than a blank canvas. The GPU tests never start them, so without
this a change that compiles and still leaves a page empty would pass them.

A GPU is needed for the rendering crates' integration tests and for the example.
On a headless CI runner, Mesa's `lavapipe` serves as the software Vulkan
implementation. The wasm pass needs no GPU: the browser runs on the SwiftShader
it ships, for both backends. The WebGPU pass needs more than that wherever it
has to read a canvas — a window, and an OpenGL implementation that is not
SwiftShader's, which a bare image has to install. Which check needs which is in
[`WORKAROUND.md`](WORKAROUND.md), "A WebGPU canvas reads back transparent
without a window and a GL driver".

## Per-crate coverage

| Crate | What its tests cover |
|-------|----------------------|
| [`unlit_ecs`](crates/unlit_ecs/README.md) | World and query behaviour, deferred commands, and the MCP server's ECS tools. |
| [`unlit_wgpu`](crates/unlit_wgpu/README.md) | Unit tests plus GPU integration tests that render meshes into offscreen textures and compare them against the snapshots under `tests/snapshots` (a symlink into the asset submodule). |
| [`unlit3d`](crates/unlit3d/README.md) | Unit tests plus GPU integration tests that render scenes into offscreen targets and inspect the pixels that come back, including an end-to-end MCP test that serves a world over a pair of streams and speaks JSON-RPC to it. Its multi-frame snapshot coverage lives in `unlit3d_examples`. |
| [`unlit3d_examples`](unlit3d_examples/README.md) | The command line (`src/cli.rs`), the scene switch and the fixed-step playback clock (`src/tests.rs`), and the window overlays (`src/overlay/tests.rs`). Its rendering output is snapshotted by `tests/gpu_suite.rs`, and `tests/selector_position.rs` covers the panels the window shell keeps across a scene switch. |
| [`unlit3d_cli`](unlit3d_cli/README.md) | The argument parser and configuration merging, plus the end-to-end render test that reproduces the example's reference glTF scene and compares it against the stored snapshots. |
| [`unlit3dx`](crates/unlit3dx/README.md) | The multi-world core: jobs reaching their world, snapshot and stream subscriptions, the state a world publishes about itself, the render bridge's failure paths, and the physics, audio and MCP bridges end to end. |
| [`unlit3dx_examples`](unlit3dx_examples/README.md) | The demo's scene logic as unit tests, and an end-to-end headless run whose captured frames and audio mix compare against the snapshots under the asset submodule; the same worlds also run stepped in a browser and behind an Android activity. |

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
needs. Which tier a commit gets follows from where it lands: a push to any branch
other than `main`, and any pull request, take the light tier, so pushing is
enough to have a change compiled and tested. A push and the pull request opened
from the same branch share one concurrency slot, so the pull request starts no
second run. Pushes to `main`, merge-queue runs and manual runs take the full one.

The light tier runs what a change can break on its own, within the time a
reviewer spends reading it: one platform, and for the web and for Android the
lint rather than the browser or the APK around it. `main` then runs the rest,
which is the part that proves a release ships.

Every branch other than `main`, whether pushed on its own or opened as a pull
request:

- **lint** — `typos`, `tombi lint --error-on-warnings` and `tombi format
  --check` over the TOML, and `cargo fmt --all -- --check`.
- **build** (Linux) — clippy with default features, with no features at all and
  with every feature, then `cargo xtask test` and `cargo doc` with `-D warnings`.
  The test task is where the workspace's snapshot comparisons run, twice: once at
  the WebGPU baseline's limits, once with every offscreen device narrowed to
  WebGL2's shape. Linux installs Mesa for `lavapipe`, since the runners have no
  GPU, and fetches the demo soundfont so the audio baselines compare real sound.
- **miri** — `cargo xtask miri`: the `unlit_ecs` tests interpreted under Miri,
  which is the only check that sees undefined behaviour in the storage that hands
  out references through `&World`. No other crate runs under it.
- **check-wasm** — clippy for `wasm32-unknown-unknown` over the workspace, over
  the four libraries without their default features and over the test binaries.
  It excludes `xtask`, a host-only task runner whose HTTP server does not
  compile for that target.
- **check-android** — clippy for `aarch64-linux-android` with every feature and with
  none, which is the only thing that compiles the code behind
  `cfg(target_os = "android")`. The host clippy in `build` never reaches it.

`main` — every push to it, a merge queue run, and a manual run from it:

- **build** runs on macOS and Windows as well as Linux.
- **build-wasm** — the three `check-wasm` clippy passes, then every wasm test
  binary bindgen'd into a page and driven through a real browser, once on WebGPU
  and once on WebGL2, both on software devices. Linting those builds here is why
  `check-wasm` does not run on `main`.
- **build-android** — both `check-android` clippy passes, then the cross-build of
  the shared libraries and Gradle around it for the APK. It runs those passes
  itself, so `check-android` does not run on `main` either.

Each `check-*` job shares its cache with the job that does the same subject on
`main` (`shared-key` on Swatinem/rust-cache), so what the light tier compiles is
what the merge restores instead of recompiling. The two workflows set the same
`env` block for that reason: rust-cache hashes every `CARGO*` and `RUST*`
variable into the key, so a variable set on one side and not on the other splits
the cache it was meant to share. Every job also saves its caches even when it
fails (`cache-on-failure`), so an iteration on a red branch still leaves the next
run warmer than a cold one would be.

The frames a failed run leaves behind are uploaded per pass, each from its own
directory: `target/snapshot-mismatches` and `target/snapshot-mismatches-webgl2`
from `build`, and `target/snapshot-mismatches-wasm` and
`target/snapshot-mismatches-wasm-webgl2` from `build-wasm`. A
mismatch is often the platform showing through rather than a regression, so
having the frame is what makes the difference something to look at rather than to
infer from a score.

[`.github/workflows/pages.yml`](.github/workflows/pages.yml) publishes the site
to GitHub Pages on every push to `main`: a landing page at the root linking the
two web examples and the API docs, each example in a directory of its own
beside it, and the workspace's rustdoc under `docs/`, which the `unlit3d`
example's corner links to as well. The deploy builds the docs on nightly, since
the page listing the crates and the `doc(cfg(...))` labels are nightly-only,
and tolerates doc warnings there: a published page with a broken link beats a
site held back by it. `ci.yml` is where a doc warning fails a build, on the
branch rather than at the deploy.
