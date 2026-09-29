English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.zh-CN.md)

# unlit_wgpu_test_util

The shared GPU test harness for the workspace's rendering crates. Every test
drives wgpu through a real [`wgpu::Device`] and reads buffer or texture data
back for assertions; this crate is what sets that device up, what reads the data
back, what turns a rendered frame into a perceptual snapshot assertion, and what
runs the same test in a browser as well as on the host.

It is at an **early stage of development**; APIs change freely. It is not part
of the public rendering API: nothing in the shipped rendering path depends on
it. It is a **dev-dependency** of
[`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.md)
and
[`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md),
and an optional dependency of
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md)
behind its `snapshot` feature.

## One test, two runners

A test is an `async fn` that takes no arguments and returns nothing, and the
crate's macros register it twice over: natively it runs in a process, and on
`wasm32-unknown-unknown` the same file *becomes* a wasm module a browser drives.

Because wgpu's adapter and device requests are asynchronous, so is the harness.
On the host that is invisible — [`block_on`] drives them to completion. In a
browser there is nothing to block on: the page has one thread and the request
resolves on a microtask it has to yield to, so the test itself is spawned onto
the browser's task queue and its verdict reported back through
`sessionStorage`.

That asymmetry is why a test file ends with two macro calls:

```rust,ignore
gpu_tests! {
    renders_a_cube_over_the_clear_color,
    #[should_panic(expected = "morphing without targets")]
    morphing_without_targets_panics,
}

gpu_test_main!(all_tests());
```

`gpu_tests!` lists the tests in the file and gives each one the body beside its
name; `gpu_test_main!` emits the `main` that runs them. Native test targets are
declared `harness = false` in their `Cargo.toml`, because the list is what the
harness runs rather than the one rustc generates.

`#[should_panic]` works on both runners, but the browsers reach it differently:
the host catches the unwind, while a wasm panic traps its instance and cannot be
caught at all — so the harness installs a panic hook that compares the message
against the expectation and reports through the same channel as a pass.

There is a third mode. With `UNLIT3D_WASM_TEST` in the environment, a native run
becomes a *proxy* instead of a runner: it lists every test so `cargo nextest` can
drive it, and each trial asks a local Node runner to run that test in a real
browser, reporting the browser's verdict as its own. `cargo xtask test-wasm`
sets it.

## What it provides

- [`Ctx::headless`] — a ready-to-use headless [`wgpu::Device`] and
  [`wgpu::Queue`]. It installs the logger backend on first use.
- [`Ctx::headless_for`] — the same for a named [`DeviceTier`]. A tier narrows
  the device to what a less capable platform offers, so a test can reach the
  paths that platform takes on hardware that is not that platform.
  [`Ctx::headless`] reads the tier from `UNLIT3D_DEVICE_TIER`, which is how the
  suite is run a second time against WebGL2's shape; unset, it asks for the
  WebGPU baseline rather than the adapter's own limits. On the web the device is
  created from a canvas and the `GL` backend, since that is what a browser
  offers in place of a native API.
- [`init_logging`] — the logger backend on its own, for tests that never build a
  [`Ctx`]. Natively it is `env_logger` reading `RUST_LOG`; on the web,
  `console_log` forwards to the browser console. The default level is `warn`.
- Buffer and texture readback: [`readback_buffer`] and [`read_texture_bytes`]
  (the latter undoes the row padding a texture copy requires), plus
  [`ColorTarget`], [`Frame`], [`texel_bytes`], [`bg_entry`], [`rgb`],
  [`srgb_to_linear_u8`] and [`count_pixels_off_background`].
- With the `snapshot` feature: snapshot assertions, described below.

`log` is re-exported, so a test crate needs no `log` dependency of its own.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `snapshot` | no | snapshot assertions via SSIMULACRA2, plus `image` for WebP encoding and decoding |

## Snapshots

With `snapshot` enabled, a test can store a rendered frame or assert it against a
stored one. `assert_image_snapshot` compares the frame against the snapshot named
`name` within `DEFAULT_TOLERANCE`; `assert_image_snapshot_with_tolerance` takes
an explicit `Tolerance`.

A `Tolerance` names up to two gates, and both must pass:

- a **score floor**, on SSIMULACRA2's 0–100 scale, which notices a change spread
  thinly over the whole frame;
- an **outlier allowance**, the fraction of pixels permitted to differ from the
  baseline by more than `channel_delta` (8 of 255 by default), which notices a
  change confined to a few pixels that a whole-frame average barely registers.

Either may be left unset. A snapshot that needs a looser tolerance should carry
the measurements that justify it, since a bound set without them is
indistinguishable from one chosen to make a failure go away.

`score_frame_webp` and `store_frame_webp` are the underlying pieces, for a caller
that wants to report the score rather than assert.

Where the baseline comes from depends on the target, and the `snapshot!` macro
is what arranges it:

- **Natively** the name is looked up under `tests/snapshots`, relative to the
  process's working directory, and read at comparison time so it can be
  rewritten. That directory is a symlink into the
  [`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
  submodule; clone it with `git submodule update --init`.
- **On the web** there is no filesystem, so the bytes are embedded into the wasm
  binary at compile time with `include_bytes!`, resolved relative to the file the
  macro is invoked from. The comparison is otherwise the same one — the frame is
  really decoded and really scored in the browser.

A missing snapshot is *stored* rather than compared, and setting
`SNAPSHOT_UPDATE=1` re-stores every snapshot it touches. So a snapshot that was
never committed silently passes CI by writing itself — which is why the CI
snapshot job checks the submodule out. On the web a missing snapshot cannot
arise: the build would not have compiled.

A frame that *fails* its comparison is kept before the assertion fails, so a run
that fails in CI leaves the frame behind for the job to upload: natively it is
written under `UNLIT3D_SNAPSHOT_MISMATCH_DIR` (or `target/snapshot-mismatches`),
and on the web it is encoded in the browser and handed to the test runner, which
is the process that has a filesystem. A mismatch is often the platform showing
through rather than a regression — the stored frames come from one GPU stack, and
another driver's rounding is enough to put a frame below the bar — so having the
frame is what makes the difference something to look at rather than to infer from
a score.

To re-bless a snapshot after an intentional rendering change:

```text
SNAPSHOT_UPDATE=1 cargo nextest run -p unlit_wgpu
git -C unlit3d_asset_files diff   # review before committing
```

## Usage

```rust,no_run
use unlit_wgpu_test_util::{Ctx, read_texture_bytes};

let ctx = unlit_wgpu_test_util::block_on(Ctx::headless());
let target = unlit_wgpu_test_util::ColorTarget::new(&ctx.device, "example", 64, 64);
// ... render into target.view ...
let bytes = read_texture_bytes(&ctx, &target.texture, 64, 64, 4);

#[cfg(feature = "snapshot")]
unlit_wgpu_test_util::assert_image_snapshot(
    unlit_wgpu_test_util::snapshot!("example.webp"),
    &bytes,
    64,
    64,
);
```

## Tests

This crate's own tests cover the harness's internals, such as how an outlier is
counted. The workspace's GPU tests live beside the crates they exercise, and
`cargo xtask test` runs them on the host while `cargo xtask test-wasm` runs the
same tests in a browser. Both are described in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
