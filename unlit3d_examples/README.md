English | [简体中文](README.zh-CN.md)

# unlit3d_examples

A windowed example with selectable scenes, rendered through
[`unlit3d::winit::WindowSurface`](../crates/unlit3d/README.md) — or, headlessly,
into an offscreen target that is read back and compared against stored
snapshots. It is both the workspace's example program and its rendering
regression check in CI, and it is also the Android example, packaged as an APK.

The example is at an **early stage** along with the crates it uses, and it
exercises parts of them that are still under construction.

## Scenes

The example's scenes are the snapshot scenes the GPU tests used to draw in
`crates/unlit3d/tests`: every one of them is now a selectable scene, and the
example's headless path is the check that replaced those tests. The windowed
loop shows the scene the command line selected and lists every scene in a
panel, so one can be switched to at runtime. `--list-scenes` prints the table:

```text
Scenes:
  cube                   the example's own scene: a textured cube with two panels
  ui_only                a rich egui panel, no mesh source and no camera
  mesh_and_ui            a cube with the rich egui panel over it, one pass
  ecs_animated           a grid of cubes filling, moving and recycling over eight frames
  ecs_skinned            a cube bent by a two-joint skin over six frames
  ecs_morphed            a cube blended by two morph targets over six frames
  instanced_skinned_morph three cubes sharing one mesh, deformed per instance
```

Each scene reproduces exactly what its test froze: the same world, camera and
frame sequence, so the stored snapshots in
[`wgpu_unlit_render_asset_files`](../wgpu_unlit_render_asset_files/README.md)
still verify it. The scenes are built with the same public `unlit3d` API any
caller would use — nothing in the example reaches into the crates' internals.

## Role in the workspace

This is the only crate in the workspace that ships a binary, and the only
consumer that combines every other crate: it uses
[`unlit3d`](../crates/unlit3d/README.md) for the frame loop, ECS components,
input and UI, and
[`wgpu_unlit_render`](../crates/wgpu_unlit_render/README.md) for the pipeline
options and the resource graph. Its headless path borrows
[`wgpu_unlit_test_util`](../crates/wgpu_unlit_test_util/README.md) for frame
readback and scoring, behind the `snapshot` feature.

It is a library as well as a binary, because Android starts neither a process
nor a command line: the activity loads the shared library and calls its
`android_main`, and the command-line path is the binary. Both end in the same
windowed loop, so the example only has one frame loop to maintain.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `snapshot` | no | the headless capture path: `--headless`, `--output` and `--snapshot`. It pulls in the test harness's frame readback and perceptual comparison, so the windowed example needs neither, and the wasm and Android builds never see it |

## Running it

```text
cargo run -p unlit3d_examples
```

A window opens with the default scene — a spinning, textured cube and two egui
panels — plus a panel that lists every scene, so one can be switched to at
runtime. `Esc` closes it; the cube's panel's button, checkbox and slider drive
the spin, and dragging with the left button orbits the camera. A scene can be
selected from the command line instead:

```text
cargo run -p unlit3d_examples -- --scene ecs_skinned
```

To run the same example in a browser — where WebGPU needs a secure context, so
`localhost` rather than `file://` — use the task runner:

```text
cargo xtask run-wasm
```

## Android

Android starts an *activity*, not a process: the activity loads the shared
library and calls its `android_main` on a thread of its own, which is why this
crate is a library as well as a binary. `android_main` builds the event loop
with the activity winit requires on that platform and hands it to the same
windowed loop the binary drives, starting with the default scene.

To build the APK, which cross-compiles the library, stages it in the Gradle
project's `jniLibs` directory and then runs the Gradle wrapper there:

```text
cargo xtask build-android
```

The result is `android/app/build/outputs/apk/debug/app-debug.apk`, installable
with `adb install`. `--release` builds a release APK instead, which this
project leaves unsigned. The project's Gradle setup — its `compileSdk`, its
`minSdk`, its one ABI — and the task's own constants have to agree; see
[`xtask/README.md`](../xtask/README.md#cargo-xtask-build-android).

The activity is
[`android/app/src/main/java/org/unlit3d/example/MainActivity.kt`](../android/app/src/main/java/org/unlit3d/example/MainActivity.kt).
It extends `GameActivity`, which loads the library named by the
`android.app.lib_name` manifest entry and calls into it, so the activity itself
only takes the screen over. The whole application — window, GPU context, frame
loop — is this crate's.

## Headless capture

With the `snapshot` feature, the example is its own snapshot runner. It renders
a scene offscreen with no window and no event loop, advancing it by a fixed
timestep so the same command produces the same picture, and compares each frame
against the scene's stored snapshots:

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --scene ecs_skinned
```

`--headless` without the feature reports that the feature is needed and exits
with code 2. `--scene all` runs every scene, which is what CI does.

### Options

Every option is `--name value` or a bare flag; `--name=value` is accepted too.

| Option | Default | Meaning |
|--------|---------|---------|
| `--headless` | off | Render offscreen, read the frame back and exit, without opening a window |
| `--scene <ID>` | `cube` | The scene to run; `all` runs every scene (headless only) |
| `--list-scenes` | off | Print the scene table and exit |
| `--size <WxH>` | the scene's own | Render target size in pixels; also the window's initial size |
| `--frames <N>` | the scene's own | Frames to draw before capturing. The scene's own count is what its snapshots were stored at; `--frames` overrides it |
| `--output <PATH>` | none | Write the captured frame to `PATH` as a lossless WebP |
| `--snapshot <PATH>` | none | Compare the captured frame against the snapshot at `PATH`, instead of the scene's own snapshots |
| `--update` | off | Store the snapshots being compared instead of comparing them |
| `--no-ui` | off | Draw the scene without its UI overlay |
| `--min-score <S>` | `85.0` | Lowest SSIMULACRA2 score that counts as matching |
| `--snapshot-dir <D>` | the asset submodule's `snapshots` | Where the scene's own snapshots resolve, by name |
| `-h`, `--help` | | Print the usage text and the scene table |

`--output`, `--snapshot`, `--update` and `--scene all` have no meaning in the
windowed loop, so asking for one without `--headless` is an error rather than a
silent no-op. `--scene all` cannot be combined with `--output` or `--snapshot`.
`--no-ui` is only honoured by the headless path; the windowed path always
mounts the UI.

### Snapshots

Without `--snapshot`, a headless run compares every frame the scene declares a
snapshot for — a multi-frame scene's whole sequence — against the snapshot
directory. The comparison uses SSIMULACRA2 and exits non-zero when the score
falls below `--min-score`. If a snapshot is missing it refuses to compare and
tells you to generate one with `--update` — writing a missing snapshot would
let a regression pass CI by creating the very file the check is meant to read.

A scene's own snapshots only describe a run that reproduces the scene's stored
settings. Overriding `--size`, `--frames` or `--no-ui` therefore makes a custom
capture that is written with `--output` but not compared against the scene's
snapshots; compare it explicitly with `--snapshot <PATH>` instead.

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --scene all
```

is exactly what the CI snapshot job runs, against the submodule's committed
images. To re-bless one or all of them after an intentional rendering change,
add `--update`, then review the image diffs in
[`wgpu_unlit_render_asset_files`](../wgpu_unlit_render_asset_files/README.md)
before committing them. That submodule is checked out with
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
