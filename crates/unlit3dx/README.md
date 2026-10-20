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
    // The world publishes to its subscribers once its state is current.
    publish_subscribed(world);
    control.wait_timeout(Duration::from_secs_f64(1.0 / 64.0));
    !control.stop_requested()
});

// Jobs run on the remote thread between ticks; a request brings its
// answer back whenever it arrives.
simulation.send(Box::new(|world: &mut World| world.root_scope_mut().spawn((0u8,)))).expect("room in the queue");
let spawned = simulation.request(Box::new(|world: &mut World| world.root_scope_mut().spawn((0u8,))));
// `spawned.poll()` is `Some(entity)` once the job has run.

// Snapshots are the only data that flows back, and they are opt-in: the
// world runs the collector this subscription carries once per tick and
// publishes whatever it returns. The payload is the caller's type — the
// library defines none — so the collector is where a scene decides what it
// wants to see.
let reader = simulation.sender().subscribe(|world: &World| {
    let bodies: Vec<u64> = world.root_scope().query::<&u64>().map(|(_, id)| *id).collect();
    Some(bodies)
}).expect("room in the queue");
if let Some(latest) = reader.latest() {
    // ... apply `latest` ...
}
// Dropping the reader unsubscribes: the world stops collecting for it.
```

The feature-gated modules build scenes on top of that core. A **sub-world**
keeps its owner on the main thread but takes jobs like a remote world:

```rust,ignore
let render = spawn_sub_world(&mut host, "render", 64); // 64 queued jobs
let mut nested = host.root_scope_mut().get_mut::<World>(render.entity()).expect("the nested world");
drain_sub_world(&mut nested); // run the jobs other threads posted
```

The **physics** bridge drains rapier's contacts into a `PhysicsContacts`
component each step, and the **synth** bridge turns note jobs and MIDI files
into soundfont audio. A render bridge is a caller's own build: the
`unlit3dx_examples` crate writes one over `unlit3d`'s `MeshSource` registry
and ties every bridge into one scene.

## Why separate worlds

A `unlit_ecs` world is not `Send`: its components live behind `RefCell`s, and
structural changes need `&mut World`. The usual answer is to share one world
across threads behind locks, which turns every system into a contention site
and every access into a borrow argument. `unlit3dx` answers differently: keep
one world per concern, and never share one.

- A **sub-world** is a nested `World` sitting on a host entity, owned by the
  same thread. A drawing world is one: it draws on the main thread, and the
  host reaches into it only between events. It keeps a job inbox too:
  `spawn_sub_world` wires one up, other threads post jobs through the
  `WorldHandle` it returns, and the owner calls `drain_sub_world` between its
  own steps — the same protocol as a remote world, minus the thread.
- A **remote world** is a `World` that belongs to its own thread. Physics,
  audio and gameplay each run one.

The worlds never call each other. A caller sends a **job** — a boxed closure
that receives the remote world's `&mut World` — or **subscribes** to a
snapshot, which installs a collector in that world and hands back a reader.
That is the whole protocol.

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
RequestError>` in request order, and `executor::LocalTasks` drives it. Every
world the framework builds carries a task set as a component, so a task
lasts as long as the world and belongs to it rather than to the call that
started it; whoever drives the world pumps it once per turn — a thread
before its tick, a host before the dispatch. `LocalTasks::pump` runs its
tasks until none can progress and never blocks, which is what lets a
fixed-step tick or a frame behaviour own the await. The demo's scene build
is that shape.

## Snapshots

A snapshot is state handed to whoever asked for it. `JobSender::subscribe`
installs a collector in the target world and returns a `SnapshotReader<T>`;
the world runs every collector it carries once per tick, after its state is
current, and publishes what each returns. The payload is the caller's type —
the library defines none — so a scene collects exactly the data it wants and
pays for nothing else.

A reader is a single-slot mailbox: `latest` peeks the newest snapshot, there
is no backlog and no ordering to reason about, and an unread one is replaced.
The slot is an `ArcSwapOption`, so neither side takes a lock the other
contends on: the world swaps the newest snapshot in, and a reader that already
holds one keeps reading that copy to the end. A world that collects per tick
and a consumer that reads per frame meet in the middle without ever blocking.
A collector that returns `None` leaves the newest snapshot in place, which is
how a world that has not changed stays quiet.

Nothing is collected unasked, and a subscription lives exactly as long as its
reader: dropping the last clone of a `SnapshotReader` prunes the collector on
the next tick. Subscribing is a job, so it is refused with the queue's own
error when the world is full or has stopped.

Because a mailbox keeps only the newest snapshot, it carries state and not
events. A contact that began and ended between two reads is gone; a caller
that must see every one of them — bullet damage, say — has its collector
push them into a queue of its own and return `None`. The collector is the
only place that can see them: it runs on the producing world's thread, on
every tick, before the next one overwrites the buffer.

## Threads and stepping

A world is spawned through a *runner*, which decides who calls its tick loop.
`WorldRunner` is that choice made generic, and it is the only difference
between the two modes: the bounded job queue, the subscriptions, the proxy
entity on the host and the handle the spawn returns are all the same, so the
host's entities and components do not change with the runner and nothing in a
tick body has to know which one it is under.

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
  reach it directly, such as reading a frame back out of a drawing world.

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
convenience layer over the core, not a privilege: each wraps an external
library — rapier, cpal, rustysynth-ext — in the same job-and-snapshot shape,
and a caller that wants another library builds that shape itself. The
`unlit3dx_examples` crate is the worked example: its render bridge is an
asset server written in the example crate over `unlit3d`'s generic
`MeshSource` registry, registering its own pipeline family and allocating
through the same API a caller would. A render world's pipeline options come
from the `UnlitPipelineKey` component installed in it; without one they are
derived from the mesh source's device.

The bridges:

- **physics** (on by default): `spawn_physics_world_with::<R>` runs a rapier
  `PhysicsWorld` at a fixed rate (`PHYSICS_HZ`, 64). Each step drains rapier's
  contacts into a `PhysicsContacts` component — the collision pairs that
  started, with the speed each pair was closing at, and those that stopped —
  and then runs the world's subscribers. It publishes no payload of its own:
  a caller subscribes with the collector that names the state it wants.
  `spawn_body` (a `BodyDesc` with a `BodyShape`) and `spawn_fixed_cuboid` are
  the building blocks; `init_physics_world` builds the same world for a
  caller that steps it inline with `step_once`.
- **audio** (on by default): `spawn_audio_world_with::<R>` opens the output
  device and owns its stream on the audio thread. Its `OnAudioFill` components are
  the sources, and one entity may hold several layers in one component, each
  with an `AudioFillOrder` of its own:
  `dispatch_audio_fill` runs every callback once per chunk of a ring buffer sized by
  `AudioConfig`, in that order, mixing their samples and scaling the sum by the
  world's `AudioGain`; the device callback drains the ring and underruns to
  silence.
  A world that carries no source plays nothing.
  Gameplay never touches audio directly — it sends jobs. The world spawns an
  `AudioFormat` component the moment its stream opens — a source that renders
  at a fixed rate has to be built at the rate the device really uses, and that
  is not known until then — and an `AudioOutputOpen` marker with it, so a
  subscriber can tell a world with somewhere to play from one still waiting
  for a device and drop stale notes instead of queueing them behind a
  soundfont load. Installing sources is therefore ordinary work for that
  world: subscribe to the format, then
  queue the job that spawns them.

  A stepped audio world opens no device at all: there is nothing to play to
  and no callback to pull the ring. It publishes no format either, so a
  caller that wants sound installs its sources up front and calls
  `dispatch_audio_fill` itself when it wants a chunk — which is what the demo's
  headless run does, rendering the mix to a file. `dispatch_audio_fill` is the
  whole of the audio thread's per-chunk work, so stepping it directly is the
  same mixing, minus the ring.

- **synth** (on by default, implies audio): the pieces to install, built on
  [rustysynth-ext](https://docs.rs/rustysynth-ext) — `load_soundfont` parses
  the bytes (typically an `include_bytes!`, so no filesystem is involved),
  `SynthVoice::from_font` spawns a live voice and `MidiPlayer::from_font`
  one sequenced track, each with the `OnAudioFill` source that renders it; the
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
  function, queued to the audio world as a job. `MidiPlayer::ends` counts
  how many times a track has played to its end: a looping play never
  counts, and a reader that remembers the count it last saw sees every
  end, even when it polled nothing in between. The count is a plain fact
  about a player, so whoever needs it asks the audio world for it — a
  mailbox would only hold the newest value and drop an end that came and
  went between two frames, and an event that must not be missed belongs in
  a queue the collector fills.
- **ui** (on with `render`): [`ui::UiPanelHost`] lays a world's
  `unlit3d::ui::UiPanel`s out for a `unlit3d::ui::UiSource` in another
  world. A panel can only be built where its egui context runs — its callback
  is handed a `&mut egui::Ui` from the pass building it — so the panel's
  world lays the frame out and tessellates it, and the drawing world uploads
  and records what comes back. Spawn a [`ui::UiPanelHost`] beside the panels;
  it holds the slots and the screen the drawing side draws at, and those go to
  the drawing side's source with [`unlit3d::ui::UiSource::draw_published`].
  The handover is one way: the
  panel world publishes on its own tick and the drawing side draws whatever a
  slot holds. A tick that could not change the picture — a settled panel, no
  input, the same target, no repaint due — publishes nothing, so a still UI
  costs nothing, and the frame already there is drawn until a newer one
  replaces it. They keep separate contexts: sharing
  one would have both worlds writing the same memory and font atlas, and egui
  treats one `run_ui` call as one whole frame. Their texture ids both start
  at zero, so a frame keeps the ids it was laid out with and the GPU
  integration keys textures by origin and id — nothing renumbers them.
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
  write the mix out as a WAV file. A recording source mounts at the end of
  the fill order, so it captures the mix after every other source and the
  world's `AudioGain`, and the samples become a RIFF/WAVE file with no extra
  dependency. The recorder is compiled only for these tools, so `mcp` turns
  `audio` on itself.

## Best practices

Two rules keep a scene's worlds independent, and they are what make one
portable: the same scene runs on threads or stepped, in a window or
headless, because no mode ever reaches into another world's data.

1. **A world's setup is configuration only.** Spawning a physics or audio
   world — or a drawing world of the caller's own — passes the configuration
   its runtime needs — the device, queue and capabilities, the tick rate,
   the gravity, the buffer size — and builds the generic resources that
   runtime owns: a resource graph and a renderer, a mesh source, a rapier
   world, an output stream. Nothing in it names a thing the game has: no
   ball, no track, no camera angle, no control panel. A world that comes up
   empty is also a world that can come up early — a drawing sub-world exists
   before the GPU does, so gameplay holds its handle from the first tick and
   fills it later.
2. **Every game-specific change to a world arrives as a job.** Meshes,
   materials and instances are requested from the render bridge; bodies are
   spawned and despawned through the physics queue; sources and transport go
   to the audio world as jobs, and a whole install — see the demo's
   `install_synth` — is one function wrapped in one job. A snapshot carries
   state *out* of a world, and the world that reads it decides what
   that means; what another world's content *becomes* is a job, so a pose
   gameplay derived from a physics snapshot is sent to the render world
   rather than left in a mailbox for it to apply.

The test is whether a line of setup could belong to another scene: if it
names something the game calls a thing, it belongs in a job.

## The application skeleton

The window-facing half of a multi-world app is app-agnostic too, so it
ships with the runtime. The **gpu** module starts the `Gpu` request as a
task of the world it was asked to set up, driven by the pump that opens each
frame, and runs `ready` as a queued command once the answer lands — nothing
blocks, wasm-safe:

```rust,ignore
world.root_scope_mut().spawn((request_gpu(app, |world, gpu| {
    // ... build the scene where the request started ...
}),));
```

`Gpu::headless` is the same request without a window, for tests and
tools. The **present** module owns the plumbing from a sub-world to a
window: `setup_presenter` by default has the scene's pass draw straight into
the swap chain, and with its offscreen path on adds a second pass to the same
world that copies a persistent target over the surface — which keeps the
frame long enough to read back, so screenshots and snapshot tests ask for
it. `draw_presented`
is the per-frame behaviour that drains the sub-world, dispatches the
frame and presents it, `pump_presented` keeps frames coming while the
window lives, and `resize_presented` follows the window around. The render
bridge a drawing world needs — the asset server and the mesh and camera
helpers around it — is the caller's to build; `unlit3dx_examples` ships one
as `src/render.rs`.

## Tests

`cargo nextest run -p unlit3dx` covers the core: job round trips, request
replies, subscriptions — several readers each getting their own snapshot, a
collector that answers nothing leaving the newest in place, a dropped reader
pruning its subscription, and a full queue refusing one — thread stop and
panic reporting, a remote world's sent jobs running, and the same for a
stepped world — its jobs run on the step, a tick past the end finishes it,
and stepping an entity that carries no stepped world does nothing. The
integration tests add the bridges: a physics world dropping a ball onto a
floor and reporting the contact, a subscriber queueing the contacts of two
separate landings that a mailbox would have collapsed into one, the audio
world running jobs and reporting its output opening, multi-world job and
snapshot traffic, and a job that panics failing only its own request.
Recording is covered too: it captures the mix the device would play and lays
the samples out as a RIFF/WAVE file, and the audio MCP tools drive both
through the dispatch coverage below. With the `mcp` feature on, the MCP command dispatch
is covered too: world listing, routing to a nested world, and routing to one
on a thread of its own or one the host steps — plus a readable failure for
an entity that carries no world, a world that has stopped, and a queue that
refuses the job — and so are the audio recording tools, from starting a
recording to refusing a second one and writing the captured mix out.
