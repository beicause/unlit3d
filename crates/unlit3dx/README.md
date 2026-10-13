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
let spawned = simulation.request(Box::new(|world: &mut World| world.spawn((0u8,)).to_bits()));
// `spawned.poll()` is `Some(entity_bits)` once the job has run.

// Snapshots are the only data that flows back: publish on the world's
// thread, read the newest one from anywhere, never block.
let (writer, reader) = snapshot_pair::<TransformFrame>();
writer.publish(TransformFrame { transforms: Vec::new() });
if let Some(frame) = reader.latest() {
    // ... apply `frame.transforms` ...
}
```

The feature-gated modules build scenes on top of that core. A **sub-world**
keeps its owner on the main thread but takes jobs like a remote world:

```rust,ignore
let render = spawn_sub_world(&mut host, 64); // 64 queued jobs
let mut nested = host.get_mut::<World>(render.entity()).expect("the nested world");
drain_sub_world(&mut nested); // run the jobs other threads posted
```

The **render** bridge turns that into a resource server: `create_mesh`,
`create_material_from_texture` and `spawn_instance` jobs run in the render
world and return entity bits a remote gameplay world can hold as handles. The
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
`Request` is a `Future`, so a batch can be joined instead of polled:
`poll_all` answers a batch synchronously, and `executor::JoinAll` is the
same join as a future, driven on the caller's thread by `executor::LocalPool`
— the shape a scene build takes in `unlit3dx_examples`.

## Snapshots

`SnapshotWriter<T>` and `SnapshotReader<T>` are a single-slot mailbox: the
writer `publish`es the newest state and the reader `latest` peeks it. There is
no backlog and no ordering to reason about — the newest snapshot wins, and an
unread one is replaced. A world that produces snapshots per tick and a
consumer that reads per frame meet in the middle without ever blocking.

## Threads

`WorldThread::spawn_with` owns the loop: it runs a one-time `init` closure
before the first job drains, then checks `ThreadControl::stop_requested` and
runs the tick inside `catch_unwind`, reporting a panic before the thread ends
so the host learns why its world died rather than joining a corpse.
`ThreadControl::wait_timeout` paces a tick, and `FixedClock` builds on it for
worlds that want a steady rate independent of the frame rate. Despawning the
proxy entity stops its thread and waits at most two seconds for it; a tick
that will not stop is left detached rather than hanging the dropper. The
handle reports on the thread — `is_finished`, `panic_message` — and `stop`
asks it to end; `join_timeout` and `detach` are on the `WorldThread` itself,
for callers that own one directly.

## Bridges

The feature-gated modules are the bridges between worlds. They are a
convenience layer over the core, not a privilege: `unlit3d`'s
`MeshSource` is the generic registry the render bridge builds on, and a
caller can register its own pipeline family and allocate through it the
same way. A caller that installs an `UnlitPipelineKey` component in the
render world chooses the options every server job builds with; without
one the standard options are derived from the mesh source's device.

The bridges:

- **render** (on by default): `apply_transform_frames` turns a
  `TransformFrame` snapshot into render-world transforms every frame, before
  anything draws. Jobs like `create_mesh`, `create_material_from_texture` and
  `spawn_instance` run in the render world and return entity bits, so a remote
  gameplay world can build a scene it never touches directly — a `GpuMesh`
  holds an `Rc`, so entity bits are the handles that cross threads.
- **physics** (on by default): `spawn_physics_world` runs a rapier
  `PhysicsWorld` at a fixed rate (`PHYSICS_HZ`, 64) and publishes a
  `PhysicsSnapshot` — the transforms of every moving body, plus the collision
  pairs that started and stopped this step, with the speed of each new
  contact. `spawn_body` (a `BodyDesc` with a `BodyShape`) and
  `spawn_fixed_cuboid` are the building blocks; `init_physics_world` builds
  the same world for a caller that steps it inline with `step_once`.
- **audio** (on by default): `spawn_audio_world` opens the output device and
  owns its stream on the audio thread. Its `OnFill` components are the sources:
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
- **mcp** (off by default): `serve_mcp` serves the host world and its
  sub-worlds over the [Model Context
  Protocol](../crates/unlit3d_mcp/README.md), so a live scene can be
  inspected and screenshotted from a terminal while it runs.

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
replies, snapshot replacement, thread stop and panic reporting, and a remote
world's sent jobs running. The integration tests add the bridges: a physics
world dropping a ball onto a floor and reporting the contact, the audio
world running jobs and publishing its readiness, multi-world job and
snapshot traffic, and the render bridge's failure paths — a job that panics
fails only its own request, and transform frames move live entities while
stale ones are skipped. With the `mcp` feature on, the MCP command dispatch
is covered too: world listing, routing to a nested world, and a readable
failure for an entity that carries no world.
