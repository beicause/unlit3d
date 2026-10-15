English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit3dx/README.zh-CN.md)

# unlit3dx

A multi-world runtime for games: rendering, physics, audio and gameplay each
run in their own world, joined by jobs going out and snapshots coming back.
The package is `unlit3dx`. It is at an **early stage** along with the crates
it uses.

## Usage

The core is three moves: spawn a world, send it jobs, read its snapshots.

```rust,ignore
use std::time::Duration;

use unlit3dx::prelude::*;
use unlit_ecs::World;

let mut host = World::new();

// A remote world: a `World` plus a thread, ticked by this closure.
// The spawn returns a `WorldHandle` — the entity the proxy lives on,
// the command queue and the thread's status in one value.
let simulation = spawn_remote_world(&mut host, "simulation", 16, |world, control| {
    // ... advance the simulation inside `world` ...
    control.wait_timeout(Duration::from_secs_f64(1.0 / 64.0));
    !control.stop_requested()
});

// Jobs run on the remote thread between ticks; a request brings its
// answer back whenever it arrives.
simulation.send(Box::new(|world: &mut World| world.spawn((0u8,)))).expect("room in the queue");
let spawned = simulation.request(Box::new(|world: &mut World| world.spawn((0u8,))));
// `spawned.poll()` is `Some(entity)` once the job has run.

// Snapshots are the only data that flows back: publish on the world's
// thread, read the newest one from anywhere, never block. What a snapshot
// holds is the caller's type, and so is reading it: a behaviour that owns
// the reader writes the payload into the world it runs in.
let (writer, reader) = snapshot_pair::<Vec<u64>>();
writer.publish(Vec::new());
if let Some(latest) = reader.latest() {
    // ... apply `latest` ...
}
```

The feature-gated modules build scenes on top of that core. A **sub-world**
keeps its owner on the main thread but takes jobs like a remote world:

```rust,ignore
let render = spawn_sub_world(&mut host, "render", 64); // 64 queued jobs
let mut nested = host.get_mut::<World>(render.entity()).expect("the nested world");
drain_sub_world(&mut nested); // run the jobs other threads posted
```

The **render** bridge turns that into a resource server: `create_mesh`,
`create_material_from_texture` and `spawn_instance` jobs run in the render
world and answer with the `Entity` they spawned, which a remote gameplay
world holds as a handle. The
**physics** bridge
publishes a `PhysicsSnapshot` of body transforms and collision events, and
the **synth** bridge turns note jobs and MIDI files into soundfont audio. The
`unlit3dx_examples` crate ties every bridge into one scene.

## Why separate worlds

A `unlit_ecs` world is not `Send`: its components live behind `RefCell`s, and
structural changes need `&mut World`. The usual answer is to share one world
across threads behind locks, which turns every system into a contention site
and every access into a borrow argument. `unlit3dx` answers differently: keep
one world per concern, and never share one.

- A **sub-world** is a nested `World` sitting on a host entity, owned by the
  same thread. The render world is one: it draws on the main thread, and the
  host reaches into it only between events. It keeps a job inbox too:
  `spawn_sub_world` wires one up, other threads post jobs through the
  `WorldHandle` it returns, and the owner calls `drain_sub_world` between its
  own steps — the same protocol as a remote world, minus the thread.
- A **remote world** is a `World` that belongs to its own thread. Physics,
  audio and gameplay each run one.

The worlds never call each other. A caller sends a **job** — a boxed closure
that receives the remote world's `&mut World` — and a **snapshot** comes back
through a single-slot mailbox. That is the whole protocol.

## Jobs and requests

`Job<R>` is a boxed `FnOnce(&mut World) -> R`. `WorldHandle::send` queues one
and never blocks: a full queue returns `Err(SendError::Full)` and a stopped
world `Err(SendError::Closed)`, so a world thread that sends to its own queue
cannot deadlock against itself. The remote thread drains its queue before
every tick, so a job never races a tick. `WorldHandle::request` wraps a job
with a reply channel and returns a `Request<R>` the caller may `poll`,
`recv` or await: `poll` is `Option<R>`, `recv` is `Result<R, RequestError>`,
and a request whose job was dropped (refused, or the world died, or the job
panicked) reports `Err(RequestError::Dropped)` instead of blocking forever.
`Request` is a `Future`, so a batch is joined rather than polled:
`executor::try_join_all` turns one into a future of `Result<Vec<R>,
RequestError>` in request order, and `executor::LocalPool` drives it on the
caller's thread — `run_until_stalled` polls every task it holds until none
can progress and never blocks, which is what lets a fixed-step tick or a
frame behaviour own the await. The demo's scene build is that shape.

## Snapshots

`SnapshotWriter<T>` and `SnapshotReader<T>` are a single-slot mailbox: the
writer `publish`es the newest state and the reader `latest` peeks it. There is
no backlog and no ordering to reason about — the newest snapshot wins, and an
unread one is replaced. A world that produces snapshots per tick and a
consumer that reads per frame meet in the middle without ever blocking.

## Threads and stepping

A world is spawned through a *runner*, which decides who calls its tick loop.
`WorldRunner` is that choice made generic, and it is the only difference
between the two modes: the bounded job queue, the snapshot mailboxes, the
proxy entity on the host and the handle the spawn returns are all the same,
so the host's entities and components do not change with the runner and
nothing in a tick body has to know which one it is under.

- **`Threaded`** gives the world a thread of its own. `WorldThread::spawn_with`
  owns the loop: it runs a one-time `init` closure before the first job
  drains, then checks `ThreadControl::stop_requested` and runs the tick inside
  `catch_unwind`, reporting a panic before the thread ends so the host learns
  why its world died rather than joining a corpse. Despawning the proxy entity
  stops its thread and waits at most two seconds for it; a tick that will not
  stop is left detached rather than hanging the dropper. The handle reports on
  the thread — `is_finished`, `panic_message` — and `stop` asks it to end;
  `join_timeout` and `detach` are on the `WorldThread` itself, for callers
  that own one directly.
- **`Stepped`** keeps the world where it was spawned.
  `spawn_stepped_world_with_init` puts the nested `World` inside the proxy
  component on the host entity, and
  `step_stepped_world` runs one iteration on the caller's thread: drain the
  queue, then tick, each under `catch_unwind` — the threaded loop's order.
  A step that ticks past the end of its world marks it finished, and
  `with_stepped_world` lends the nested `World` out for work that needs to
  reach it directly, such as reading a frame back out of the render world.

Pacing is what the mode changes. `ThreadControl::wait_timeout` blocks a real
thread and returns at once when the control is not self-driven, so the same
tick body serves both: `FixedClock` waits out a wall-clock deadline under
`Threaded`, and under `Stepped` the tick is simply due again on the next
call. A caller stepping a world therefore owns the rate — it decides how many
steps a frame is worth, which is what the demo's `SceneStepper` does.

The stepped mode is what a browser and a headless run use.
`wasm32-unknown-unknown` has no thread to spawn, and a snapshot test wants a
frame count it controls rather than one a clock chose.

## Bridges

The feature-gated modules are the bridges between worlds. They are a
convenience layer over the core, not a privilege: `unlit3d`'s
`MeshSource` is the generic registry the render bridge builds on, and a
caller can register its own pipeline family and allocate through it the
same way. A caller that installs an `UnlitPipelineKey` component in the
render world chooses the options every server job builds with; without
one the standard options are derived from the mesh source's device.

The bridges:

- **render** (on by default): `create_mesh`, `create_material_from_texture`
  and `spawn_instance` run in the render world and answer with the `Entity`
  they spawned, so a remote gameplay world can build a scene it never touches
  directly — a `GpuMesh` holds an `Rc`, so the entity is the handle that
  crosses threads. Moving those entities is the caller's own job: a pose
  the scene derived from a physics snapshot is sent to the render world,
  not left in a mailbox for it to apply.
- **physics** (on by default): `spawn_physics_world_with::<R>` runs a rapier
  `PhysicsWorld` at a fixed rate (`PHYSICS_HZ`, 64) and publishes a
  `PhysicsSnapshot` — the transforms of every moving body, plus the collision
  pairs that started and stopped this step, with the speed of each new
  contact. `spawn_body` (a `BodyDesc` with a `BodyShape`) and
  `spawn_fixed_cuboid` are the building blocks; `init_physics_world` builds
  the same world for a caller that steps it inline with `step_once`.
- **audio** (on by default): `spawn_audio_world_with::<R>` opens the output
  device and owns its stream on the audio thread. Its `OnFill` components are
  the sources:
  `dispatch_fill` runs them once per chunk of a ring buffer sized by
  `AudioConfig`, mixing their samples and scaling the sum by the world's
  `AudioGain`; the device callback drains the ring and underruns to silence.
  A world that carries no source plays nothing.
  Gameplay never touches audio directly — it sends jobs. The proxy entity
  carries two inboxes: an `AudioFormatInbox`, which the thread fills the
  moment its stream opens — a source that renders at a fixed rate has to be
  built at the rate the device really uses, and that is not known until
  then — and an `AudioReadyInbox`, which says when the world can actually
  sound, so a caller can tell a loading soundfont from a broken device and
  drop stale notes instead of queueing them behind the load. Installing
  sources is therefore ordinary work for that world: wait for the format,
  queue the job that spawns them.

  A stepped audio world opens no device at all: there is nothing to play to
  and no callback to pull the ring. It publishes no format either, so a
  caller that wants sound installs its sources up front and calls
  `dispatch_fill` itself when it wants a chunk — which is what the demo's
  headless run does, rendering the mix to a file. `dispatch_fill` is the
  whole of the audio thread's per-chunk work, so stepping it directly is the
  same mixing, minus the ring.

  Recording is ordinary work too: `start_recording` mounts a source at the
  end of the fill order, so what it captures is the mix after every other
  source and the world's `AudioGain`. The `RecordingHandle` reports the
  format it learned from the first fill, the `RecordedAudio` it hands back
  carries the samples, and `write_wav` turns them into a RIFF/WAVE file with
  no extra dependency.
- **synth** (on by default, implies audio): the pieces to install, built on
  [rustysynth-ext](https://docs.rs/rustysynth-ext) — `load_soundfont` parses
  the bytes (typically an `include_bytes!`, so no filesystem is involved),
  `SynthVoice::from_font` spawns a live voice and `MidiPlayer::from_font`
  one sequenced track, each with the `OnFill` source that renders it; the
  caller spawns the world's `AudioGain`, for the headroom soundfont presets
  leave. `note_on_job`/`note_off_job` are ready-made jobs against the voice.
  The transport jobs address a track by name —
  `play_track_job` starts a file, looping or not, `pause_track_job` holds
  its sequencer clock so the resume continues where the pause stopped,
  `rewind_track_job` returns it to the start, `stop_track_job` puts the
  file down. Tracks are independent, so a background loop and a one-shot
  sequence coexist on one audio world and pause separately. Files come from
  `load_midi_file` (an SMF file) or `midi_file` (a list of timed
  messages). The demo's `install_synth` shows the whole install as one
  function, queued to the audio world as a job.
- **mcp** (off by default): `serve_mcp` serves the host world and every
  world it carries — a nested sub-world, or one on a thread of its own —
  over the [Model Context Protocol](../unlit_ecs/README.md), so a live scene
  can be
  inspected and screenshotted from a terminal while it runs. Each spawn
  leaves a `WorldSender` and a `WorldName` on the proxy entity beside the
  proxy: the sender is what lets the transport post a command into a world
  it cannot borrow, and the name is what `list_worlds` reports. The crate
  registers four tools of its own — `start_audio_recording`,
  `stop_audio_recording`, `cancel_audio_recording` and
  `audio_recording_status` — which drive a recording in the named world and
  write the mix out as a WAV file.

## Best practices

Two rules keep a scene's worlds independent, and they are what make one
portable: the same scene runs on threads or stepped, in a window or
headless, because no mode ever reaches into another world's data.

1. **A world's setup is configuration only.** Spawning a render, physics or
   audio world passes the configuration its runtime needs — the device,
   queue and capabilities, the tick rate, the gravity, the buffer size —
   and builds the generic resources that runtime owns: a resource graph and
   a renderer, a mesh source, a rapier world, an output stream. Nothing in
   it names a thing the game has: no ball, no track, no camera angle, no
   control panel. A world that comes up empty is also a world that can come
   up early — the render sub-world exists before the GPU does, so gameplay
   holds its handle from the first tick and fills it later.
2. **Every game-specific change to a world arrives as a job.** Meshes,
   materials and instances are requested from the render server; bodies are
   spawned and despawned through the physics queue; sources and transport go
   to the audio world as jobs, and a whole install — see the demo's
   `install_synth` — is one function wrapped in one job. A snapshot mailbox
   carries state *out* of a world, and the world that reads it decides what
   that means; what another world's content *becomes* is a job, so a pose
   gameplay derived from a physics snapshot is sent to the render world
   rather than left in a mailbox for it to apply.

The test is whether a line of setup could belong to another scene: if it
names something the game calls a thing, it belongs in a job.

## The application skeleton

The window-facing half of a multi-world app is app-agnostic too, so it
ships with the runtime. The **gpu** module starts the `Gpu` request as a
future the frame behaviour drives on a `LocalPool`, and runs `ready` as a
queued command once the answer lands — nothing blocks, wasm-safe:

```rust,ignore
world.spawn_frame_behaviour(request_gpu(app, |world, gpu| {
    // ... build the scene where the request started ...
}));
```

`Gpu::headless` is the same request without a window, for tests and
tools. The **present** module owns the plumbing from a sub-world to a
window: `setup_presenter` by default has the scene draw straight into the
swap chain, and with its offscreen path on renders each frame into a
persistent target that a second world blits over the surface — which keeps
the frame long enough to read back, so screenshots and snapshot tests ask
for it. `draw_presented`
is the per-frame behaviour that drains the sub-world, dispatches the
frame and presents it, `pump_presented` keeps frames coming while the
window lives, and `resize_presented` follows the window around. Mesh and
camera helpers — `uv_sphere`, `cuboid_mesh`, `camera_looking_at`,
`linear_sampler` — round the skeleton out.

## Tests

`cargo nextest run -p unlit3dx` covers the core: job round trips, request
replies, snapshot replacement, thread stop and panic reporting, a remote
world's sent jobs running, and the same for a stepped world — its jobs run on
the step, a tick past the end finishes it, and stepping an entity that
carries no stepped world does nothing. The integration tests add the bridges:
a physics world dropping a ball onto a floor and reporting the contact, the
audio
world running jobs and publishing its readiness, multi-world job and
snapshot traffic, and the render bridge's failure paths — a job that panics
fails only its own request, and one that names an asset the world never built
fails the instance that asked for it. So is recording: `start_recording`
captures the mix the device would play, `write_wav` lays the samples out as a
RIFF/WAVE file, and the audio MCP tools drive both through the dispatch
coverage below. With the `mcp` feature on, the MCP command dispatch
is covered too: world listing, routing to a nested world, and routing to one
on a thread of its own or one the host steps — plus a readable failure for
an entity that carries no world, a world that has stopped, and a queue that
refuses the job — and so are the audio recording tools, from starting a
recording to refusing a second one and writing the captured mix out.
