English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.zh-CN.md)

# unlit3dx_examples

A multi-world demo: one scene where rendering, physics and audio each run in
their own world, joined by jobs and snapshots, built on
[`unlit3dx`](../crates/unlit3dx/README.md). The package is `unlit3dx_examples`;
the binary is `unlit3dx-examples`.

## What you see

A checkered ground plane and a pool of colored balls. Space drops a ball from above at a
random position; balls fall, bounce off the ground and each other, and every
contact hard enough plays a pentatonic note through a soundfont synthesizer.
Balls also drop on their own, about one every one and a half seconds. The
camera orbits the scene slowly.

## How to run

```sh
cargo run -p unlit3dx_examples
```

The synthesizer needs a soundfont. The first positional argument is its path,
defaulting to `assets/FluidR3Mono_GM.sf3`, which the repository keeps out of
version control; any SF3 soundfont works, for example
[FluidR3Mono_GM.sf3](https://github.com/beicause/soundfont/blob/main/FluidR3Mono_GM.sf3).

The `--mcp` switch serves the worlds over the [Model Context
Protocol](../crates/unlit3d_mcp/README.md) instead of exiting with the
window: tools can then inspect every world and save screenshots of the
render world.

## The headless mode

```sh
cargo run -p unlit3dx_examples -- --headless --frames 300 --out headless
```

The `--headless` switch runs the very same scene without a window, threads
or an audio device: the worlds step inline on one thread at their fixed
rates — gameplay at 120 Hz, physics at 64 Hz, render at 60 Hz — for
`--frames` frames, then the run's last frame and its whole audio mix land
in `--out` as `screenshot.png` and `audio.wav`. Because the stepping is
fixed-rate and the spawn rolls are seeded, a run is reproducible, which is
what the snapshot tests ([English](tests/headless.rs),
[简体中文](tests/headless.rs)) build on: rendered frames compare against
webp baselines and the mix against a wav baseline, both kept in the
[`unlit3d_asset_files`](../unlit3d_asset_files) submodule;
`SNAPSHOT_UPDATE=1` re-stores them.

## How the worlds are wired

Five worlds, per the `unlit3dx` protocol: jobs go out, snapshots come back.
The other worlds act as servers; gameplay is the client that drives them.

- The **host** owns the window and drives frames. It requests the GPU, builds
  the render world, drains the render world's job inbox every frame and
  presents the result — and knows nothing about physics or audio. Space is
  forwarded to gameplay as a job.
- The **render world** is a sub-world on the main thread that behaves like a
  Godot-style resource server: gameplay requests meshes, materials and
  instances with jobs, and the entity bits of the spawned instances come
  back as the handles. It also holds the orbiting camera and one behaviour
  that applies the latest transform snapshot before anything draws.
- The **physics world** steps rapier at 64 Hz and publishes body transforms
  and collision events. The ground is its first job.
- The **audio world** owns the output stream and the synthesizer; it receives
  note jobs and mixes, underrunning to silence.
- The **gameplay world** is the only place that knows about all three. It
  builds the scene by requesting assets and instances from the render
  server, requests bodies from physics, maps them to the instance handles,
  publishes the transform snapshot, books a note per contact pair and fades
  it out. It ticks at a fixed 120 Hz, independent of the frame rate.
