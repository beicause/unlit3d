English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)

# unlit3d_examples

A windowed example with selectable scenes, rendered through
[`unlit3d::winit::WindowSurface`]. Its scenes are also drawn offscreen by
`tests/gpu_scenes.rs`, which compares every frame against a stored snapshot, so
the crate is both the workspace's example program and a rendering regression
check. It is also the Android example, packaged as an APK.

It is at an **early stage** along with the crates it uses, and it exercises
parts of them that are still under construction.

## Scenes

The example's scenes are the snapshot scenes the GPU tests used to draw in
`crates/unlit3d/tests`: every one of them is now a selectable scene, and
`tests/gpu_scenes.rs` is the check that replaced those tests. The windowed loop
shows the scene the command line selected and lists every scene in a panel, so
one can be switched to at runtime. `--list-scenes` prints the table:

```text
Scenes:
  spin_cube              the example's own scene: a textured cube with two panels
  ui_only                a rich egui panel, no mesh source and no camera
  mesh_and_ui            a cube with the rich egui panel over it, one pass
  ecs_animated           a grid of cubes filling, moving and recycling over eight frames
  ecs_skinned            a cube bent by a two-joint skin over six frames
  ecs_morphed            a cube blended by two morph targets over six frames
  instanced_skinned_morph three cubes sharing one mesh, deformed per instance
  mesh_topologies        every primitive topology, indexed and non-indexed
  transparent_zsorted    translucent panes composited back to front over opaque cubes
```

Each scene reproduces exactly what its test froze: the same world, camera and
frame sequence, so the stored snapshots still verify it. `mesh_topologies` and
`transparent_zsorted` are the exceptions: they are not ported from a test but
add the coverage the ported scenes lack — every primitive topology drawn both
indexed and non-indexed, and overlapping translucent draws whose composite
depends on both the z-sort and the blend state.

Every scene is built with the same public `unlit3d` API any caller would use —
nothing in the example reaches into the crates' internals.

## How it runs

The windowed path is the whole frame loop a windowed app needs. The renderer is
spawned once as a resource entity, a scene's meshes and materials are allocated
through its mesh source, and every `RedrawRequested` acquires the swap chain's
next image, renders the ECS world into it and presents it. A resize is handed to
the surface, which reconfigures the swap chain and rebuilds the depth and
multisample attachments the renderer draws with.

A scene that draws 3D content declares a **baseline aspect** — 960×720, the
shape the ported scenes were captured at. The windowed path draws such a scene
through the largest rectangle of that aspect the target fits, and hands the
scene that rectangle's own size, so every scene takes its camera aspect from the
region it is actually drawn into. The target only ever adds bars around the
picture: a wide window shows the same view a narrow one does, larger, rather
than more of it, and nothing is ever stretched. The UI is drawn over the whole
target rather than the letterboxed region, because a panel has no aspect of its
own. The snapshot tests turn the letterbox off and draw at the scene's own size,
so a capture is unchanged.

A readout in the top-right corner reports the smoothed frame rate and frame
time. In a browser, the first press on the window asks for fullscreen — the one
gesture a browser accepts as permission — and a device being held upright is
then locked to landscape, so the picture fills a phone's screen. The lock
follows from the fullscreen: the browser releases it when fullscreen ends, and
it is never applied to a device already held sideways.

The GPU context is requested asynchronously, because the adapter and device
requests are: on the web they resolve on the browser's task queue, so the frame
loop must not block on them. The window is created on the main thread — winit
hands out a window's raw handle only from the thread that owns it — and the
context arrives back through the event loop's proxy, where the scene is built on
the thread that owns the ECS world.

A suspension does not reset any of that. The platform invalidates the render
surface, and on Android destroys the native window under it, but the window
handle, the GPU context and the whole ECS world stay: the swap chain alone is
released and built again on the next resume, so the app comes back to the state
it left — the same spin angle, camera orbit and panel values.

## Running it

```text
cargo run -p unlit3d_examples
```

A window opens with the default scene — a spinning, textured cube and two egui
panels — plus a panel that lists every scene. `Esc` closes it; the cube's
panel's button, checkbox and slider drive the spin, and dragging with the left
button orbits the camera. A scene can be selected from the command line instead:

```text
cargo run -p unlit3d_examples -- --scene ecs_skinned
```

To run the same example in a browser — where WebGPU needs a secure context, so
`localhost` rather than `file://` — use the task runner:

```text
cargo xtask run-wasm
```

The page it serves sizes the canvas to the viewport less a small margin of its
own, rather than to the window the example asks for, so the example fills a
phone's screen and follows a window that is resized. The canvas is not the
picture, though: each 3D scene is drawn into the baseline-aspect region of it,
so the same picture fills whatever shape the page gives the canvas.

It is a library as well as a binary, because Android starts neither a process
nor a command line: the activity loads the shared library and calls its
`android_main`, and the command-line path is the binary. Both end in the same
windowed loop, so the example only has one frame loop to maintain.

## Android

Android starts an *activity*, not a process: the activity loads the shared
library and calls its `android_main` on a thread of its own. `android_main`
builds the event loop with the activity winit requires on that platform and
hands it to the same windowed loop the binary drives, starting with the default
scene.

To build the APK, which cross-compiles the library, stages it in the Gradle
project's `jniLibs` directory and then runs the Gradle wrapper there:

```text
cargo xtask build-android
```

The result is `android/app/build/outputs/apk/debug/app-debug.apk`, installable
with `adb install`. `--release` builds a release APK instead, which this project
leaves unsigned. The project's Gradle setup and the task's own constants have to
agree; see the
[task runner's README](https://github.com/beicause/unlit3d/blob/main/xtask/README.md#cargo-xtask-build-android).

The activity is
[`MainActivity.kt`](https://github.com/beicause/unlit3d/blob/main/android/app/src/main/java/org/unlit3d/example/MainActivity.kt).
It extends `GameActivity`, which loads the library named by the
`android.app.lib_name` manifest entry and calls into it, so the activity itself
only takes the screen over. The whole application — window, GPU context, frame
loop — is this crate's.

## Snapshot tests

`tests/gpu_scenes.rs` is where the example's rendering is checked. It draws every
scene offscreen, with no window and no event loop, advancing it by a fixed
timestep so the same code produces the same picture, and compares each frame the
scene freezes against the scene's stored snapshot:

```text
cargo nextest run -p unlit3d_examples
```

The test is a library test rather than a command line: the same body runs
natively and, because the test binary is what the wasm test page loads, in a
browser too. `cargo xtask test` runs it twice, once per device tier, and
`cargo xtask test-wasm` runs it against a real WebGL2 implementation.

### Options

The options are declared and parsed with
[`argh`](https://docs.rs/argh). Every option is `--name value` or a bare flag;
the value must be the next argument, so `--name=value` is not accepted.

| Option | Default | Meaning |
|--------|---------|---------|
| `--scene <ID>` | `spin_cube` | The scene to start from |
| `--list-scenes` | off | Print the scene table and exit |
| `--size <WxH>` | the scene's own | Initial window size in pixels |

### Playback pace

A snapshot test draws a multi-frame scene's frames back to back, so one run
matches its snapshots frame by frame. The windowed loop instead plays them over
time: a scene whose animation was frozen as a few frames holds each one for
`SEQUENCE_STEP` (currently 0.5 s), so the sequence is watchable rather than
flashing past at the refresh rate. At most one sequence step is taken per
windowed frame, so a stall does not skip frames.

### Choosing a device tier

`UNLIT3D_DEVICE_TIER` sets how much of the WebGPU baseline every offscreen device
a test builds is asked for. Unset, it is `webgpu`: the guaranteed baseline
limits, so the device is no larger than the API promises and the frame runs
wherever WebGPU does. Only the texture resolution is taken from the adapter, so
the device is never smaller than the machine allows. `native` asks for the
adapter's own limits instead, which on a desktop is its full Vulkan, Metal or
DX12 capabilities.

`webgl2` asks for WebGL2's limits — no storage buffers — and the frame is
recorded as WebGL2 would record it, with the shader's arrays read through
`textureLoad` and each mesh's vertex offset baked into its indices. Any other
value is refused rather than guessed at, since running the wrong tier would pass
while testing nothing.

```text
UNLIT3D_DEVICE_TIER=webgl2 cargo nextest run -p unlit3d_examples
```

`WGPU_BACKEND` selects the backend in the usual `wgpu` way and is independent of
the tier; `WGPU_BACKEND=gles` together with the tier is the closest a desktop
machine comes to a browser's WebGL2, since the limits and flags are WebGL2's
even where the GL context is not.

### Snapshots

Each test compares every frame the scene declares a snapshot for against the
snapshot directory. The comparison uses SSIMULACRA2 and fails when the score
falls below the tolerance, which the test states for the scene it draws. A
missing snapshot is written from the frame rather than failed on, so the first
run of a new test records its baseline, and `SNAPSHOT_UPDATE=1` rewrites one that
exists.

The default tolerance suits a frame of whole surfaces, where a regression moves
large regions and so lands far below the line. A frame of thin primitives is
different: its pixels are outlines, and which triangle an outline pixel belongs
to is a rasterizer's tie-breaking rule to decide — one the API leaves to each
implementation. A scatter of such pixels is worth tens of points there, so
`mesh_topologies` is given a tolerance of its own, stated with the measurements
that put its bound above every correct frame and below every broken one.

A scene's own snapshots only describe a run that reproduces the scene's stored
settings, so a test draws at the scene's own size and frame count. The letterbox
fitting the windowed path applies is checked by a test of its own, which draws
the default scene at a target narrower than its baseline and compares the fitted
picture against a capture made for it.

A mismatch is frequently the platform showing through rather than a regression:
the stored images come from one GPU stack, and another driver's rounding can put
a scene below the line with nothing wrong. A frame that fails is therefore
written before the test fails — natively under
`UNLIT3D_SNAPSHOT_MISMATCH_DIR`, and in a browser through the runner, which is
the process that has a filesystem — so the frame can be looked at instead of
guessed at from a score. CI uploads those directories as artifacts when a test
fails.

To re-bless a snapshot after an intentional rendering change, run with
`SNAPSHOT_UPDATE=1`, then review the image diffs in
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
before committing them. That submodule is checked out with
`git submodule update --init --checkout`, which is what gets past its
`update = none`.

## Tests

The crate's own tests cover the command line (`src/cli.rs`) and the fixed-step
playback clock (`src/lib.rs`), and `tests/gpu_scenes.rs` covers the rendering.
Where each test layer sits across the workspace, and what CI runs, is in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
