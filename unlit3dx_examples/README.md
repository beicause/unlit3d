English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.zh-CN.md)

# unlit3dx_examples

A multi-world demo: one scene where rendering, physics and audio each run in
their own world, joined by jobs and snapshots, built on
[`unlit3dx`](../crates/unlit3dx/README.md). The package is `unlit3dx_examples`;
the binary is `unlit3dx-examples`. It runs in a window, in a browser and
behind an Android activity, and the same scene also runs headless for its
snapshot tests.

## What you see

A checkered ground plane and a stream of falling balls — spheres, cubes,
cylinders and cones, each wearing its own patterned material. Space drops a
ball from above at a random position; balls fall and bounce off the ground
and each other. Balls also drop on their own, about one every 0.7 seconds.
The camera orbits the scene slowly.

A background loop plays on its own track of the audio world from the
moment the world can sound — that is the panel's first click on Play,
which unlocks the audio output everywhere and, in a browser, the page's
audio with it. A second track holds a one-shot MIDI sequence — the
pentatonic scale walked up and back down on a music box — which stays
idle until its row's Play button starts it. That control panel — a small
egui window — offers each track the buttons its state calls for, drops the
next ball, and puts every live ball away: an idle sequence
shows Play alone, a started track shows Pause or Resume beside Rewind, and a
sequence that plays to its end goes back to showing Play. Nothing about the
music depends on the balls: clearing the scene leaves the tracks playing.

## How to run

```sh
cargo run -p unlit3dx_examples
```

The synthesizer plays a soundfont embedded into the binary at build time. It
lives in [`asset_bytes`](asset_bytes/README.md), the crate that carries it:
`build.rs` reads `assets/FluidR3Mono_GM.sf3`, which the repository keeps out
of version control, and `include_bytes!` in that crate's `soundfont()` carries
it in, so the same bytes run everywhere, wasm included. The bytes sit outside
this crate because a constant holding them would copy fourteen megabytes into
this crate's metadata and slow every rebuild. When the file is missing the
build warns and the demo runs silent — unless `UNLIT3D_REQUIRE_SOUNDFONT` is
set, which the workflows that publish the site and the APK do, and which turns
that warning into a build failure. Drop any SF3 soundfont at that path and
rebuild — for example
[FluidR3Mono_GM.sf3](https://github.com/beicause/soundfont/blob/main/FluidR3Mono_GM.sf3).

The `--mcp` switch serves the worlds over the [Model Context
Protocol](../crates/unlit_ecs/README.md) instead of exiting with the
window: tools can then inspect every world, save screenshots of the render
world, and record the audio world's mix to a file — FLAC, the smaller of the
two, unless the file name asks for a WAV.

On the web it is one of the two pages `cargo xtask run-wasm` builds and the
Pages workflow publishes. A browser has no thread to put a world on, so the
library's stepped runner drives them all from the frame loop instead. The
Android APK loads a `.so` built from the `unlit3dx_examples_android` crate —
that crate holds the export alone, because a `cdylib` crate type here would
link a shared library on the desktop and in the browser as well —
`cargo xtask build-android` builds it, and the floating tab bar switches
between the `unlit3d` and `unlit3dx` activities.

## The headless mode

```sh
cargo run -p unlit3dx_examples -- --headless --frames 300 --out headless
```

The `--headless` switch runs the very same scene without a window, threads
or an audio device. It is the browser's path with the frames handed over by
the test instead of by a redraw: `spawn_scene::<Stepped>` puts every world
in the host's own world, and `SceneStepper` advances them one frame at a
time: physics steps as its own rate (`PHYSICS_HZ`, 60 Hz) comes due, the
synth fills as its sample rate does, and gameplay ticks once per frame with
the duration the run gives it (`HEADLESS_HZ`). After `--frames` frames the
run's last frame and its whole audio mix land in `--out` as
`screenshot.png` and `audio.flac`. Because every frame is the same length
and the spawn rolls are seeded, a run is reproducible, which is
what the snapshot tests ([English](tests/headless.rs),
[简体中文](tests/headless.rs)) build on: rendered frames compare against
webp baselines and the mix against a FLAC baseline, both kept in the
[`unlit3d_asset_files`](../unlit3d_asset_files) submodule;
`SNAPSHOT_UPDATE=1` re-stores them.

`--headless --mcp` goes one step further: instead of running a fixed number
of frames it waits for MCP commands, and only the `step_frames` tool advances
time. The scene renders into an offscreen texture the client reads with
`read_texture_as_image`, `scene_status` reports the frame, the ball count,
the MIDI sequence and the centre of every panel button, and `request_exit`
stops the process. Automatic ball drops and the background track's autoplay
are off in this mode, so every visible and audible change comes from the
client. The `mcp_input` test ([English](tests/mcp_input/main.rs),
[简体中文](tests/mcp_input/main.rs)) starts that binary and never touches its
internals: it clicks the panel through `send_input`, captures three image
snapshots, compares the mix it recorded while the sequence played against a
fourth baseline, asserts that the mix recorded after the pause is silent
without storing it, and asks the process to exit.

The same story runs twice from one description ([tests/mcp_input/shared.rs](tests/mcp_input/shared.rs)).
The [native driver](tests/mcp_input/native.rs) speaks MCP to that child
process; the [wasm driver](tests/mcp_input/wasm.rs) assembles the identical
host in-process through `Interactive` and pushes the same input events and
frame counts itself, so the browser suite exercises the interactive run
without a JSON-RPC transport, a socket or a second thread. Both compare
against the same four baselines.

Both snapshot tests are modules of one test target,
[tests/gpu_suite.rs](tests/gpu_suite.rs), so the GPU stack links once.

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
  itself — the mesh, material and instance allocations, the sweep that
  retires what the camera cannot see, and the camera helpers — is this crate's own
  built over `unlit3d`'s generic `MeshSource` registry; `unlit3dx` ships no
  render bridge.
- The **physics world** steps rapier at a fixed rate (`PHYSICS_HZ`, 60 Hz)
  and drains its contacts each step. The balls subscribe to it with a
  collector of their own — the
  `GamePhysics` payload lives in this crate, not in the library — and the
  collector answers nothing while the poses are unchanged, so a scene whose
  bodies are asleep publishes no snapshot at all. The ground is its first job.
- The **audio world** owns the output stream and mixes whatever sources it
  carries, underrunning to silence. It holds one synthesizer, whose voice is
  installed by a job that answers with its entity; the voice builds its
  synthesizer on the first fill it renders, at the rate that fill names, so it
  can be installed before the stream names a rate at all. Every note the music
  plays is a job addressed to that entity. What the world reports is its
  `AudioFormat`, which the demo reads to answer the install, and the
  `AudioOutputOpen` marker its stream carries.
- The **gameplay world** is the only place that knows about all three, and it
  is a set of objects rather than one state blob. Each object is spawned as a
  bundle holding the component that carries what it owns and the `OnFrame`
  behaviour that advances it. Most of them state no order and tick in the
  default band, where the dispatch promises nothing about which of them goes
  first; the two that read what another object wrote state one — the balls after
  the assets they read, the panel's publish after every object it shows.

  - `SceneState` builds the scene's assets — meshes and patterned
    materials — as one job of its own, before the objects that read them are
    spawned. Every drop asks the render world for an instance of its own.
  - `spawn_floor` queues the fixed ground body and builds the checker
    instance that draws it, before the first drop can happen.
  - `Balls` spawns a `Ball` entity for every drop and asks the
    render world for an instance and physics for a body. Both answers name the
    ball, so the ball itself is the identity: it is shown once both halves
    arrive and the render world confirms the show, and an answer that no longer
    finds its ball cleans itself up. The render world despawns an instance the
    camera can no longer see and reports the ball back, and the ball goes away
    with it.
  - `SceneAudio` asks the audio world for the synthesizer's voice,
    and on the answer spawns the scene's two tracks in this world: each is a
    `MidiSequencer` with a clock and the voice it plays into, and it takes
    the audio world's sender from the `Scene` it is ticked with. The
    background loop starts playing at once; the demo sequence waits, paused,
    for the panel.

  Each object reaches the others' worlds through one `Scene` value: the
  render, physics and audio senders, the physics snapshot, and what each of
  those three worlds reports about itself, handed to every tick. An object
  that never talks to the audio world keeps no sender for it.


  The world ticks once per frame and takes that frame's measured duration,
  so gameplay runs as fast as the display drives it; the display's refresh
  rate only bounds how often its thread wakes, and never how far time
  advances.

The control panel is the fifth object, and it lives in the **gameplay world**
with the rest. A panel can only be built where its egui context runs, so this
world lays it out and publishes the frame, and the render world draws whatever
the slots hold: the panel's entity carries the `UiPanel`, while the
`UiPanelHost` spawned beside it holds the context they are built in and
publishes for it. Its transport buttons write the sequences this world owns —
a play, a pause, a rewind is a field write on the `MidiSequencer` of the row
that was clicked. The render world holds only a `UiSource` with those slots
attached, which uploads and records what it finds there.

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
