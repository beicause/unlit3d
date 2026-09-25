English | [简体中文](README.zh-CN.md)

# unlit3d_examples

A windowed unlit cube with an egui overlay, rendered through
[`unlit3d::winit::WindowSurface`](../crates/unlit3d/README.md) — or, headlessly,
into an offscreen target that is read back and compared against a snapshot.
It is both the workspace's example program and its rendering-regression check
in CI.

The example is at an **early stage** along with the crates it uses, and it
exercises parts of them that are still under construction.

## Role in the workspace

This is the only binary crate in the workspace, and the only consumer that
combines every other crate: it uses [`unlit3d`](../crates/unlit3d/README.md)
for the frame loop, ECS components, input and UI, and
[`wgpu_unlit_render`](../crates/wgpu_unlit_render/README.md) for the pipeline
options and the resource graph. Its headless path borrows
[`wgpu_unlit_test_util`](../crates/wgpu_unlit_test_util/README.md) for frame
readback and scoring, behind the `snapshot` feature.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `snapshot` | no | the headless capture path: `--headless`, `--output` and `--snapshot`. It pulls in the test harness's frame readback and perceptual comparison, so the windowed example needs neither, and the wasm and Android builds never see it |

## Running it

```text
cargo run -p unlit3d_examples
```

A window opens with a spinning, textured cube and two egui panels. `Esc`
closes it; the panel's button, checkbox and slider drive the spin, and dragging
with the left button orbits the camera.

To run the same example in a browser — where WebGPU needs a secure context, so
`localhost` rather than `file://` — use the task runner:

```text
cargo xtask run-wasm
```

## Headless capture

With the `snapshot` feature, the example is its own capture tool. It renders
offscreen with no window and no event loop, advancing the scene by a fixed
timestep so the same command produces the same picture, then reads the frame
back:

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --snapshot frame.webp
```

`--headless` without the feature reports that the feature is needed and exits
with code 2.

### Options

Every option is `--name value` or a bare flag; `--name=value` is accepted too.

| Option | Default | Meaning |
|--------|---------|---------|
| `--headless` | off | Render offscreen, read the frame back and exit, without opening a window |
| `--size <WxH>` | `960x720` | Render target size in pixels; also the window's initial size |
| `--frames <N>` | `2` | Frames to draw before capturing. The second frame is the first egui has its font metrics for, so at least two are needed for laid-out text |
| `--output <PATH>` | none | Write the captured frame to `PATH` as a lossless WebP |
| `--snapshot <PATH>` | none | Compare the captured frame against the snapshot at `PATH` |
| `--update` | off | Store `PATH` instead of comparing against it |
| `--no-ui` | off | Draw the cube without the UI overlay |
| `--min-score <S>` | `85.0` | Lowest SSIMULACRA2 score that counts as matching |
| `-h`, `--help` | | Print the usage text |

`--output`, `--snapshot` and `--update` have no meaning in the windowed loop, so
asking for one without `--headless` is an error rather than a silent no-op.
`--update` also requires `--snapshot <PATH>`. `--no-ui` is only honoured by the
headless path; the windowed path always mounts the UI.

### Snapshots

`--snapshot` compares with SSIMULACRA2 and exits non-zero when the score falls
below `--min-score`. If the snapshot does not exist it refuses to compare and
tells you to generate one with `--update` — writing a missing snapshot would
let a regression pass CI by creating the very file the check is meant to read.
When comparing against a snapshot outside the window, pass `--no-ui` if you
are checking the 3D scene alone.

This is exactly what the CI snapshot job runs, against the submodule's
committed image:

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --frames 30 \
    --snapshot wgpu_unlit_render_asset_files/snapshots/example.webp
```

To re-bless it after an intentional rendering change, add `--update`, then
review the image diff in
[`wgpu_unlit_render_asset_files`](../wgpu_unlit_render_asset_files/README.md)
before committing it. That submodule is checked out with
`git submodule update --init`.

## Tests

The crate's only tests are for the hand-written command-line parser
(`src/cli.rs`), which has no argument-parsing dependency of its own:

```text
cargo nextest run -p unlit3d_examples
```

The example's rendering output is checked by the CI snapshot job above rather
than by a `cargo test` target.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
