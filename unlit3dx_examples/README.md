English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.zh-CN.md)

# unlit3dx_examples

A multi-world demo: one scene where rendering, physics and audio each run in
their own world, joined by jobs and snapshots, built on
[`unlit3dx`](../crates/unlit3dx/README.md). The package is `unlit3dx_examples`;
the binary is `unlit3dx-examples`. It runs in a window, in a browser and
behind an Android activity, and the same scene also runs headless for its
snapshot tests.

## What you see

A checkered ground plane and a pool of falling shapes — balls, cubes,
cylinders and cones, each wearing its own patterned material. Space drops a
shape from above at a random position; shapes fall and bounce off the ground
and each other. Shapes also drop on their own, about one every 0.7 seconds.
The camera orbits the scene slowly.

A background loop plays on its own track of the audio world from the
moment the world can sound. A second track holds a one-shot MIDI sequence —
the pentatonic scale walked up and back down on a music box — which stays
idle until its row's Play button starts it. That control panel — a small
egui window — offers each track the buttons its state calls for, drops the
next shape, and puts every live shape back in the pool: an idle sequence
shows Play alone, a started track shows Pause or Resume beside Rewind, and a
sequence that plays to its end goes back to showing Play. Nothing about the
music depends on the shapes: clearing the scene leaves the tracks playing.

## How to run

```sh
cargo run -p unlit3dx_examples
```

The synthesizer plays a soundfont embedded into the binary at build time:
`build.rs` reads `assets/FluidR3Mono_GM.sf3`, which the repository keeps out
of version control, and `include_bytes!` carries it in, so the same bytes
run everywhere, wasm included. When the file is missing the build warns and
the demo runs silent. Drop any SF3 soundfont at that path and rebuild — for
example
[FluidR3Mono_GM.sf3](https://github.com/beicause/soundfont/blob/main/FluidR3Mono_GM.sf3).

The `--mcp` switch serves the worlds over the [Model Context
Protocol](../crates/unlit_ecs/README.md) instead of exiting with the
window: tools can then inspect every world, save screenshots of the render
world, and record the audio world's mix to a WAV file.

On the web it is one of the two pages `cargo xtask run-wasm` builds and the
Pages workflow publishes. A browser has no thread to put a world on, so the
library's stepped runner drives them all from the frame loop instead. The
same library is what the Android APK loads: `cargo xtask build-android`
builds it into a `.so`, and the floating tab bar switches between the
`unlit3d` and `unlit3dx` activities.

## The headless mode

```sh
cargo run -p unlit3dx_examples -- --headless --frames 300 --out headless
```

The `--headless` switch runs the very same scene without a window, threads
or an audio device. It is the browser's path with the frames handed over by
the test instead of by a redraw: `spawn_scene::<Stepped>` puts every world
in the host's own world, and `SceneStepper` advances them at their fixed
rates — gameplay at 120 Hz, physics at 64 Hz, render at 60 Hz — for
`--frames` frames, then the run's last frame and its whole audio mix land
in `--out` as `screenshot.png` and `audio.flac`. Because the stepping is
fixed-rate and the spawn rolls are seeded, a run is reproducible, which is
what the snapshot tests ([English](tests/headless.rs),
[简体中文](tests/headless.rs)) build on: rendered frames compare against
webp baselines and the mix against a FLAC baseline, both kept in the
[`unlit3d_asset_files`](../unlit3d_asset_files) submodule;
`SNAPSHOT_UPDATE=1` re-stores them.

## How the worlds are wired

Five worlds, per the `unlit3dx` protocol: jobs go out, snapshots come back.
The other worlds act as servers; the gameplay world holds the objects that
drive them. `spawn_scene` builds them through one runner — `Threaded` in a
window, `Stepped` in a browser or a headless run — so the host carries the
same entities and components either way, and only who calls a tick and how
it is paced differs.

- The **host** owns the window and drives frames. It requests the GPU, and
  once the GPU answers, the library's behaviours take over: `draw_presented`
  drains the render world's job inbox every frame, dispatches the frame and
  presents it, `pump_presented` keeps the frames coming while the window
  lives, and `resize_presented` follows the window around — the host knows
  nothing about physics or audio. Space is forwarded to the gameplay world
  as a job.
- The **render world** is a sub-world on the main thread that behaves like a
  Godot-style resource server: gameplay requests meshes, materials and
  instances with jobs, and the entity of each spawned instance comes back
  as its handle. Everything the scene puts in it arrives the same way —
  its orbiting camera, the source that draws its control panel, and the
  poses its bodies carry are jobs too, drained before anything draws. The
  world's own setup is
  only the resource graph, the renderer and the mesh source. The server
  itself — `create_mesh`, `create_material_from_texture`, `spawn_instance`
  and the mesh and camera helpers — is this crate's own `src/render.rs`,
  built over `unlit3d`'s generic `MeshSource` registry; `unlit3dx` ships no
  render bridge.
- The **physics world** steps rapier at 64 Hz and drains its contacts each
  step. The shapes subscribe to it with a collector of their own — the
  `GamePhysics` payload lives in this crate, not in the library — and the
  collector answers nothing while the poses are unchanged, so a scene whose
  bodies are asleep publishes no snapshot at all. The ground is its first job.
- The **audio world** owns the output stream and mixes whatever sources it
  carries, underrunning to silence. Its own sample rate is not known until
  the stream opens, so it spawns an `AudioFormat` component then and the demo
  answers with a job that spawns one synthesizer and one MIDI player per
  track at that rate. Its readiness — the `AudioOutputOpen` marker plus the
  sources the collector sees — is what the music waits on, since a transport
  job sent before the players exist would find no track to
  address.
- The **gameplay world** is the only place that knows about all three, and it
  is a set of objects rather than one state blob. Each object is a component
  carrying what it owns plus a `Tick` that advances it, and each ticks itself
  as an `OnFrame` behaviour at an order of its own:

  - `SceneState` (tick 100) builds the shape pool — meshes, patterned
    materials and the forty-eight instances — as one async task of its own,
    and hands slots out.
  - `Floor` (tick 50) queues the fixed ground body before the first drop can
    happen, then builds the checker instance that draws it.
  - `Balls` (tick 200) asks the pool for a slot, asks physics for a body,
    maps the handles the snapshot names onto its slots, and sends their poses
    back as a job.
  - `SceneAudio` (tick 300) installs the synthesizer's sources once the audio
    world reports an output, then starts the background loop once it reports
    it can sound. The panel starts the sequence and drives both tracks
    afterwards, without going through the gameplay world.

  The world ticks at a fixed 120 Hz, independent of the frame rate.

`Controls` is the fifth object, and it lives in the **gameplay world** with the
rest. A panel can only be built where its egui context runs, so this world
lays it out and publishes the frame, and the render world draws whatever the
slots hold: the panel's entity carries the `UiPanel`, the rows it shows, the
`UiHost` holding the context they are built in, and the `PanelHost` that
publishes for it. It has a tick of its own, late enough in the frame that
everything which could change what it shows has already run. The render world
holds only a `UiSource` with those slots attached, which uploads and records
what it finds there.

The two sides keep separate egui contexts. Sharing one would have both worlds
writing the same memory and font atlas, and egui treats one `run_ui` call as
one whole frame, so two worlds running it against one context would interleave
two frames into one. Their texture ids both start at zero, so a frame keeps the
ids it was laid out with and the GPU integration keys textures by origin and
id — nothing renumbers them.

A frame that could not differ from the one the drawing side holds is not laid
out at all: one is laid out only when there is none yet, the target the drawing
side reports has moved, input arrived, or the last frame asked to be laid out
again — at once, or once a delay it named has elapsed. A panel nobody is
touching therefore costs nothing on the wire, and the frame it last published
keeps being drawn until a newer one replaces it.
