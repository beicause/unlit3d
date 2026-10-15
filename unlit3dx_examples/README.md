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
the pentatonic scale walked up and back down on a music box — which the
panel starts by hand. That control panel — a small egui window — pauses,
resumes and rewinds each track independently, drops the next shape, and puts
every live shape back in the pool. Nothing about the music depends on the
shapes: clearing the scene leaves the tracks playing.

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
The other worlds act as servers; gameplay is the client that drives them.
`spawn_scene` builds them through one runner — `Threaded` in a window,
`Stepped` in a browser or a headless run — so the host carries the same
entities and components either way, and only who calls a tick and how it is
paced differs.

- The **host** owns the window and drives frames. It requests the GPU, and
  once the GPU answers, the library's behaviours take over: `draw_presented`
  drains the render world's job inbox every frame, dispatches the frame and
  presents it, `pump_presented` keeps the frames coming while the window
  lives, and `resize_presented` follows the window around — the host knows
  nothing about physics or audio. Space is forwarded to gameplay as a job.
- The **render world** is a sub-world on the main thread that behaves like a
  Godot-style resource server: gameplay requests meshes, materials and
  instances with jobs, and the entity of each spawned instance comes back
  as its handle. Everything the scene puts in it arrives the same way —
  its orbiting camera, its control panel, and the poses its bodies carry
  are jobs too, drained before anything draws. The world's own setup is
  only the resource graph, the renderer and the mesh source.
- The **physics world** steps rapier at 64 Hz and publishes body transforms
  and collision events. The ground is its first job.
- The **audio world** owns the output stream and mixes whatever sources it
  carries, underrunning to silence. Its own sample rate is not known until
  the stream opens, so it publishes an `AudioFormat` then and the demo
  answers with a job that spawns one synthesizer and one MIDI player per
  track at that rate. While the soundfont loads it publishes its readiness,
  since a transport job sent before the players exist would find no track to
  address.
- The **gameplay world** is the only place that knows about all three. It
  builds the scene by awaiting assets and instances from the render server
  as one async future on a `LocalPool`, requests bodies from physics, and
  maps them to the instance handles, sending their poses back as a job.
  Once
  the audio world reports it can sound, gameplay starts the background
  loop; the panel starts the sequence and drives both tracks afterwards
  without going through gameplay. It ticks at
  a fixed 120 Hz, independent of the frame rate.
