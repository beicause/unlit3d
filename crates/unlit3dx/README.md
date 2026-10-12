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
let simulation = spawn_remote_world(&mut host, "simulation", 16, |world, control| {
    // ... advance the simulation inside `world` ...
    control.wait_timeout(Duration::from_secs_f64(1.0 / 64.0));
    !control.stop_requested()
});

// Jobs run on the remote thread between ticks; a request brings its
// answer back whenever it arrives.
let proxy = host.get::<RemoteWorld>(simulation).expect("the proxy exists");
proxy.send(Box::new(|world: &mut World| world.spawn((0u8,))));
let spawned = proxy.request(Box::new(|world: &mut World| world.spawn((0u8,)).to_bits()));
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
let mut nested = host.get_mut::<World>(render).expect("the nested world");
drain_sub_world(&mut nested); // run the jobs other threads posted
```

The **render** bridge's `RenderServer` turns that into a resource server:
`create_mesh`, `create_material` and `spawn_instance` jobs return entity
bits a remote gameplay world can hold as handles. The **physics** bridge
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
  `SubWorld` proxy the host entity carries, and the owner calls
  `drain_sub_world` between its own steps — the same protocol as a remote
  world, minus the thread.
- A **remote world** is a `World` that belongs to its own thread. Physics,
  audio and gameplay each run one.

The worlds never call each other. A caller sends a **job** — a boxed closure
that receives the remote world's `&mut World` — and a **snapshot** comes back
through a single-slot mailbox. That is the whole protocol.

## Jobs and requests

`Job<R>` is a boxed `FnOnce(&mut World) -> R`. `RemoteWorld::send` queues one;
the remote thread drains its queue before every tick, so a job never races a
tick. `RemoteWorld::request` wraps a job with a bounded reply channel and
returns a `Request<R>` the caller may `poll` or `recv` later: the send is
fire-and-forget, and the answer arrives whenever it arrives. `poll_all`
answers a batch of requests only once every answer is in — a batch polled
too early stays intact instead of losing the requests that did answer. A dropped
`Request` costs nothing; a dropped job's result is simply never read.

## Snapshots

`SnapshotWriter<T>` and `SnapshotReader<T>` are a single-slot mailbox: the
writer `publish`es the newest state and the reader `latest` peeks it. There is
no backlog and no ordering to reason about — the newest snapshot wins, and an
unread one is replaced. A world that produces snapshots per tick and a
consumer that reads per frame meet in the middle without ever blocking.

## Threads

`WorldThread::spawn` owns the loop: it checks
`ThreadControl::stop_requested`, runs the tick inside `catch_unwind`, and
reports a panic through a channel before the thread ends, so the host learns
why its world died rather than joining a corpse. `ThreadControl::wait_timeout`
paces a tick, and `FixedClock` builds on it for worlds that want a steady rate
independent of the frame rate. Dropping a `RemoteWorld` stops its thread and
joins it.

## Bridges

The feature-gated modules are the bridges between worlds:

- **render** (on by default): `apply_transform_frames` turns a
  `TransformFrame` snapshot into render-world transforms every frame, before
  anything draws; `RenderEntity` marks the render entity a simulation drives.
  `RenderServer` is the resource-server half: jobs like `create_mesh`,
  `create_material` and `spawn_instance` run in the render world and return
  entity bits, so a remote gameplay world can build a scene it never touches
  directly — a `GpuMesh` holds an `Rc`, so entity bits are the handles that
  cross threads.
- **physics** (on by default): `spawn_physics_world` runs a rapier
  `PhysicsWorld` at a fixed rate (`PHYSICS_HZ`, 64) and publishes a
  `PhysicsSnapshot` — the transforms of every moving body, plus the collision
  pairs that started and stopped this step, with the speed of each new
  contact. `spawn_body` (a `BodyDesc` with a `BodyShape`) and
  `spawn_fixed_cuboid` are the building blocks, and `PhysicsBody` marks the
  body an entity owns.
- **audio** (on by default): `spawn_audio_world` opens the output device and
  owns its stream on the audio thread. A `PcmSource` fills a ring buffer sized
  by `AudioConfig`; the device callback drains it and underruns to silence.
  Gameplay never touches audio directly — it sends jobs. The proxy also
  carries an `AudioReadyInbox`: the world publishes when it can actually
  sound, so a caller can tell a loading soundfont from a broken setup and
  drop stale notes instead of queueing them behind the load.
- **synth** (on by default, implies audio): `synth_source_bytes` builds a
  mixer from soundfont bytes — typically an `include_bytes!`, so no
  filesystem is involved — loading a
  [rustysynth-ext](https://docs.rs/rustysynth-ext) synthesizer on the audio
  thread, and `note_on_job`/`note_off_job`/`notes_off_all_job` are
  ready-made jobs against it. `play_midi_job` and `stop_midi_job` drive a
  `MidiPlayer`: a MIDI file sequencer fed from `load_midi_file` (an SMF
  file) or `midi_file` (a list of timed messages). The `gain` argument
  scales the mix, for the headroom soundfont presets leave.
- **mcp** (off by default): `serve_mcp` serves the host world and its
  sub-worlds over the [Model Context
  Protocol](../crates/unlit3d_mcp/README.md), so a live scene can be
  inspected and screenshotted from a terminal while it runs.

## The application skeleton

The window-facing half of a multi-world app is app-agnostic too, so it
ships with the runtime. The **gpu** module asks for a `Gpu` off the
main thread and answers back into a closure on it — wasm-safe, no
blocking:

```rust,ignore
world.spawn_frame_behaviour(request_gpu(app, |world, gpu| {
    // ... build the scene where the request started ...
}));
```

`Gpu::headless` is the same request without a window, for tests and
tools. The **present** module owns the plumbing from a sub-world to a
window: `setup_presenter` renders the scene into an offscreen target and
blits it to the swap chain (which also keeps the frame long enough to
read back — screenshots and snapshot tests rely on it), `draw_presented`
is the per-frame behaviour that drains the sub-world, dispatches the
frame and presents it, `pump_presented` keeps frames coming while the
window lives, and `resize_presented` follows the window around. Mesh and
camera helpers — `uv_sphere`, `cuboid_mesh`, `camera_looking_at`,
`linear_sampler` — round the skeleton out.

## Tests

`cargo nextest run -p unlit3dx` covers the core: job round trips, request
replies, snapshot replacement, thread stop and panic reporting, and a remote
world's sent jobs running.
