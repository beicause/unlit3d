English | [简体中文](README.zh-CN.md)

# unlit_wgpu_test_util

The shared GPU test harness for the workspace's rendering crates. Every test
drives wgpu through a real `wgpu::Device` and reads buffer or texture data back
for assertions; this crate is what sets that device up, what reads the data
back, and what turns a rendered frame into a perceptual snapshot assertion.

It is deliberately self-contained: besides `wgpu`, `glam`, `log` and `pollster`,
plus the platform logger backend and — behind the `snapshot` feature —
`fast-ssim2` and `image`, it depends on nothing.

This crate is at an **early stage of development**; APIs change freely. It is
not part of the public rendering API.

## Role in the workspace

`unlit_wgpu_test_util` is a **dev-dependency** of
[`unlit_wgpu`](../unlit_wgpu/README.md) and
[`unlit3d`](../unlit3d/README.md), and an optional dependency of
[`unlit3d_examples`](../../unlit3d_examples/README.md) behind its `snapshot`
feature. Nothing in the shipped rendering path depends on it.

## What it provides

- `Ctx::headless()` — a ready-to-use headless `wgpu::Device` and `wgpu::Queue`,
  with the adapter and device requests driven synchronously by `pollster`. It
  installs the logger backend on first use.
- `init_logging()` — the logger backend on its own, for tests that never build
  a `Ctx`. Natively it is `env_logger` reading `RUST_LOG`; on the web,
  `console_log` forwards to the browser console. The default level is `warn`.
- Buffer and texture readback: `readback_buffer` and `read_texture_bytes` (the
  latter undoes the row padding a texture copy requires), plus `ColorTarget`,
  `Frame`, `texel_bytes`, `bg_entry`, `rgb`, `srgb_to_linear_u8` and
  `count_pixels_off_background`.
- With the `snapshot` feature: SSIMULACRA2 snapshot assertions.

`log` is re-exported, so a test crate needs no `log` dependency of its own.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `snapshot` | no | perceptual snapshot assertions via SSIMULACRA2, plus `image` for WebP encoding and decoding |

## Snapshots

With `snapshot` enabled, a test can store a rendered frame or assert it against
a stored one:

- `assert_image_snapshot(name, rgba, width, height)` compares the frame against
  the snapshot named `name` and fails when the score is below
  `DEFAULT_MIN_SCORE` (85.0). `assert_image_snapshot_with_threshold` takes an
  explicit threshold.
- `score_frame_webp` and `store_frame_webp` are the underlying pieces, for a
  caller that wants to report the score rather than assert.
- Snapshots are looked up under `tests/snapshots`, relative to the process's
  working directory, and stored as lossless WebP. That directory is a symlink
  into the
  [`unlit3d_asset_files`](../../unlit3d_asset_files/README.md)
  submodule; clone it with `git submodule update --init`.

A missing snapshot is *stored* rather than compared, and setting
`SNAPSHOT_UPDATE=1` re-stores every snapshot it touches. So a snapshot that was
never committed silently passes CI by writing itself — which is why the CI
snapshot job checks the submodule out and the example's headless path refuses
to compare against a snapshot that does not exist.

To re-bless a snapshot after an intentional rendering change:

```text
SNAPSHOT_UPDATE=1 cargo nextest run -p unlit_wgpu
git -C unlit3d_asset_files diff   # review before committing
```

## Usage

```rust
use unlit_wgpu_test_util::{Ctx, read_texture_bytes};

let ctx = Ctx::headless();
let target = unlit_wgpu_test_util::ColorTarget::new(&ctx.device, "example", 64, 64);
// ... render into target.view ...
let bytes = read_texture_bytes(&ctx, &target.texture, 64, 64, 4);

#[cfg(feature = "snapshot")]
unlit_wgpu_test_util::assert_image_snapshot("example.webp", &bytes, 64, 64);
```

## Tests

This crate has no tests of its own; it is the harness the other crates' GPU
tests use. Run the workspace's suite with `cargo xtask test`, and remember that
it needs a working WebGPU adapter.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
