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

// A world on its own thread: a `World` plus a thread, ticked by this closure.
// The spawn returns a `WorldHandle` — the entity the proxy lives on,
// the command queue and the thread's status in one value.
// A clock paces the loop at 60 Hz: each wait sleeps until the next step is
// due, keeping the schedule where a short tick does not shift it.
let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / 60.0));
let simulation = spawn_threaded_world(&mut host, "simulation", 16, move |world, control, _| {
    // ... advance the simulation inside `world` ...
    // Whoever drives the world publishes to its subscribers once the
    // tick has run, so a tick body never publishes by hand.
    clock.wait(control);
    !control.stop_requested()
});

// Jobs run on the world's thread between ticks and answer nothing. A job
// whose result is worth keeping publishes it, or asks for it.
simulation.sender.send(Box::new(|world: &mut World| world.root_scope_mut().spawn(bundle![0u8]))).expect("room in the queue");

// A snapshot is one of the two ways data comes back (the other is a stream,
// for events), and it is opt-in: the world runs the collector this
// subscription carries once per tick and publishes whatever it returns. The payload is the caller's type — the
// library defines none — so the collector is where a scene decides what it
// wants to see.
let subscription = simulation.sender.watch(|world: &World| {
    let bodies: Vec<u64> = world.root_scope().query::<&u64>().map(|(_, id)| *id).collect();
    Some(bodies)
}).expect("room in the queue");
// Queueing the collector is not installing it: the world runs the job on its
// next turn, so `poll` answers `None` until then.
if let Some(Ok(reader)) = subscription.poll()
    && let Some(latest) = reader.latest()
{
    // ... apply `latest` ...
}
// Dropping the subscription unsubscribes: the world stops collecting for it.
```

The feature-gated modules build scenes on top of that core. A **sub-world**
keeps its owner on the main thread but takes jobs like a threaded world:

```rust,ignore
let render = spawn_sub_world(&mut host, "render", 64); // 64 queued jobs
let mut nested = host.root_scope_mut().get_mut::<World>(render.entity).expect("the nested world");
drain_sub_world(&mut nested); // run the jobs other threads posted
tick_sub_world(&mut nested, &mut frame); // or a whole frame: jobs, input, tasks, dispatch
```

The **physics** bridge drains rapier's contacts into a `PhysicsContacts`
component each step, and the **synth** bridge turns note jobs and MIDI files
into soundfont audio. A render bridge is a caller's own build: the
`unlit3dx_examples` crate writes one over `unlit3d`'s `MeshSource` registry
and ties every bridge into one scene.

## Why separate worlds

A `unlit_ecs` world is not `Send`: its components live in `UnsafeCell`s so a
shared `&World` can reach them, and structural changes need `&mut World`. The usual answer is to share one world
across threads behind locks, which turns every system into a contention site
and every access into a borrow argument. `unlit3dx` answers differently: keep
one world per concern, and never share one.

- A **sub-world** is a nested `World` sitting on a host entity, owned by the
  same thread. A drawing world is one: it draws on the main thread, and the
  host reaches into it only between events. It keeps a job inbox too:
  `spawn_sub_world` wires one up, other threads post jobs through the
  `WorldHandle` it returns, and the owner calls `drain_sub_world` between its
  own steps — the same protocol as a threaded world, minus the thread.
  `tick_sub_world` is that step when the owner drives the world's frame: it
  drains, folds the input the world has been fed, runs its tasks and dispatches
  the frame, in the one order those steps depend on each other.
- A **threaded world** is a `World` that belongs to its own thread. Physics,
  audio and gameplay each run one.

The worlds never call each other. A caller sends a **job** — a boxed closure
that receives the target world's `&mut World` — and receives through a
**subscription**: a snapshot for state, a stream for events. That is the
whole protocol.

Whoever drives a world runs one turn at a time: it drains the job queue, runs
the tick, applies the commands the tick queued through the scope, and then
publishes what the subscriptions collected. A tick body holds a shared
`&World`, so structural work it does — spawning an entity, despawning one —
goes through the queue and is committed by the driver after that tick, never
by the tick itself. A world whose host only steps it gets the same deal:
`step_stepped_world` applies as well as publishes, so a change a tick queues
lands by the time the step returns. A world that has already ended runs
nothing: every entry point — that step included — answers
`StepError::Unavailable`, so a caller that switches between stepping a world
and borrowing it cannot read a finished world as still alive.

## Jobs

`Job` is a boxed `FnOnce(&mut World)`. `WorldHandle::sender` queues one and
never blocks: a full queue returns `Err(SendError::Full)` and a stopped
world `Err(SendError::Closed)`, so a world thread that sends to its own queue
cannot deadlock against itself. The world thread drains its queue before
every tick, so a job never races a tick.

The error carries the job back, unrun: `SendErrorKind` says why the queue
refused it and `into_job` gives the work back. A refused job never started,
so sending it again is safe, and a caller that would rather drop it says so
by dropping the error — the job goes with it, along with any state it was
holding. A job that was queued and then lost to a stopping world is the other
case: it may have half-run, so nothing resends it on its own.

A job is also the only way one world changes another, and it answers nothing:
a world that must report something publishes it through a subscription
instead. A reply would have the sender waiting on a world that may be busy,
stopped, or already gone, and what comes back is state, which has a channel
of its own.

One exception is worth its own name. A caller that wants a value one job
produces — the entity a build created, the handle an install returned — asks
for it through a `Pending`: it sends the job under a key and answers on the
caller's next tick, under that key. Nothing waits, and a request the world
never answers is given up on rather than waited out forever.

Every world the framework builds carries a task set as a component, so a task
lasts as long as the world and belongs to it rather than to the call that
started it; whoever drives the world pumps it once per turn — a thread before
its tick, a host before the dispatch. `LocalTasks::pump` runs its tasks until
none can progress and never blocks, which is what lets a fixed-step tick or a
frame behaviour own the await. The GPU bridge drives its device setup that
way.

## Snapshots and streams

Receiving is a subscription: `JobSender::watch` queues the installing job in
the target world and hands back a `Subscription`. Being queued is not being
installed, so the handle answers nothing until the world's next turn runs the
job: `poll` then gives the reader, or the reason the subscription never
arrived. A world that stopped first drops the job, and the handle reports
`InstallError::Ended` rather than leaving a reader nothing would ever fill.

Whoever drives that world — its thread, its host stepping it, or its host
draining a sub-world — publishes at the end of every turn, once the tick has
run, so a tick body publishes nothing itself and cannot publish twice.
Which shape the collector fills is the caller's choice:

- `watch` returns a `SnapshotReader<T>`: state, where only the newest value
  matters. The collector returns `Option<T>`.
- `subscribe` returns a `StreamReader<T>`: events, where every value matters.
  The collector appends to the buffer it is handed, and every value reaches
  every reader's queue.

Asking through a `Pending` is the third shape: one job per request, one answer
per job, published once and named by the key the request was made under. A
request the world never answers, or one whose world ended, is expired instead
of being left hanging.

`request` answers `Asked`, because asking twice is not the same event as
asking once. `Queued` is a job on its way to the world; `Coalesced` is a
request already outstanding under the same key, whose `make` was dropped
unrun and whose answer and timeout still belong to the first one. A caller
that asks once per tick therefore learns that this tick's work was not done,
instead of mistaking the silence for a lost registration.

Which one to reach for follows from the answer the caller needs:

- A value that is only ever *the newest* — the poses of the bodies that moved,
  the playhead's position, what a world reports about itself — is `watch`.
- Every value, in order — a body that spawned, an instance the renderer
  retired, a note that ended — is `subscribe`.
- One value, once, from one job — the entity an install created, the assets
  a build made — is a `Pending` request.

As in the usage example, the payload is the caller's type.

A snapshot reader is a single-slot mailbox: `latest` peeks the newest
snapshot, there is no backlog and no ordering to reason about, and an unread
one is replaced. The slot is an `ArcSwapOption`, so neither side takes a lock
the other contends on: the world swaps the newest snapshot in, and a reader
that already holds one keeps reading that copy to the end. A world that
collects per tick and a consumer that reads per frame meet in the middle
without ever blocking. A collector that returns `None` leaves the newest
snapshot in place, which is how a world that has not changed stays quiet.
A value published before any reader subscribed is kept as well, so a reader
that arrives late still sees it instead of waiting for the next publish.

A stream reader keeps a queue of its own: `pump` once per tick folds
whatever arrived into that tick's batch, and `with_batch` reads it. A reader
that ticks slower than the producer still sees every event, in order, and one
reader's pace cannot starve another's. That is what a collision or a sequence
ending needs, and it is also how input reaches a world: `InputWriter` is a
stream whose producer is the window rather than a world.

Nothing is collected unasked, and a subscription lives exactly as long as its
reader: dropping the last clone of a reader prunes the collector on the next
tick. Subscribing is a job, so it is refused with the queue's own error when
the queue is full; an installation the world ran that could not reach its
outlet is reported as `InstallError::Outlet`.

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
  stops its thread and waits at most a few seconds (`STOP_TIMEOUT`) for it; a tick that will not
  stop is left detached rather than hanging the dropper. The handle reports on
  the world — `is_finished`, and `state`, a `SnapshotReader<WorldState>` that
  reads `Running`, `Stopped` or `Failed(error)` — and `stop` asks it to
  end. `join_timeout` and `detach` are on the `WorldThread` itself, for
  callers that own one directly: a wait answers `JoinOutcome` (`Joined`,
  `TimedOut`, `Detached` or `AlreadyJoined`) rather than a bare `bool`, so a
  handle that was given up is never reported as a thread that exited. The
  operating system refusing to create the thread is a
  `ThreadError::Spawn`, returned by the spawn rather than panicking.
- **`Stepped`** keeps the world where it was spawned.
  `spawn_stepped_world_with_init` puts the nested `World` inside the proxy
  component on the host entity, and
  `step_stepped_world` runs one iteration on the caller's thread: drain the
  queue, then tick, each under `catch_unwind` — the threaded loop's order.
  It answers a `StepOutcome` (`Ran`, `Finished`, `Stopped` or
  `Failed(error)`) so a caller can tell what the step did and why a failed
  one failed, and a `StepError` when the entity names no stepped world —
  stepping a threaded world is refused rather than silently doing nothing. `with_stepped_world` lends the nested `World` out
  for work that needs to reach it directly, such as reading a frame back out
  of a drawing world, and reports the same error instead of answering `None`.

Every failure a world ends on is a `WorldError`, whichever stage raised it:
`Panic { stage, message }` names the stage (setup, a job, a queued command,
the tick, a collector) that panicked, `Command { source }` carries the
`EntityAccessError` a queued command was refused with, and `Step { source }`
carries the `StepError` a driver could not step the world with. `WorldState`
and `StepOutcome` both carry it, so the cause a thread's state reports is the
same one a stepped world's step returns — a driver cannot lose it by choosing
one entry point over another. `drain_all_stepped_worlds` reports each failed
world as a `WorldFailure { name, error }` rather than a bare name, and
`join_failures` prints a set of them as one clause.

Pacing is what the mode changes. `ThreadControl::wait_timeout` blocks a real
thread and returns at once when the control is not self-driven, so the same
tick body serves both: `FixedClock` waits out a wall-clock deadline under
`Threaded`, and under `Stepped` the tick is simply due again on the next
call. A caller stepping a world therefore owns the rate — it decides how many
steps a frame is worth, which is what the `app` skeleton's `Stepping` does
for the physics world of every run it hosts. The audio mix is a separate
ledger: `AudioPull` turns the same frame deltas into fill lengths for the
runs that consume the samples themselves, while a windowed run leaves the
mix to its audio world, whose own tick fills the device's ring.

Both modes turn a measured interval into whole steps with the same rule:
`fixed_steps` carries the fraction of a step a tick did not spend into the
next one, and discards anything past `MAX_CATCH_UP_STEPS` (four) steps, so a
stalled host resumes instead of replaying the stall. A tick receives the
interval in seconds — `spawn_threaded_world` and its `_with_init` form pass
it as the tick's last argument — so a tick body never has to assume the rate
it asked to be woken at.

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
from the `LitPipelineKey` component installed in it; without one they are
derived from the mesh source's device.

The bridges:

- **physics** (on by default): `spawn_physics_world_with::<R>` runs a rapier
  `PhysicsWorld` at a fixed rate (`PHYSICS_HZ`, 60 Hz). A world on a thread of
  its own catches up to the time that really passed, at most four steps a
  tick; a world the host steps takes one step per step it is given. Each step
  drains rapier's contacts into a `PhysicsContacts` component — the collision
  pairs that started, with the speed each pair was closing at, and those that
  stopped.
  It publishes no payload of its own: a caller subscribes with the collector
  that names the state it wants, and whoever drives the world publishes it.
  `spawn_body` (a `BodyDesc` with a `BodyShape`) and `spawn_fixed_cuboid` are
  the building blocks; `init_physics_world` spawns the same world as a
  `Physics` bundle for a caller that steps it inline with `step_once`.
- **audio** (on by default): `spawn_audio_world_with::<R>` opens the output
  device on the audio thread for an `OnRequest` world once the page has asked;
  a `Pulled` world opens nothing, and its host fills with `dispatch_audio_fill`.
  Its `OnAudioFill` components are
  the sources, and one entity may hold several layers in one component, each
  with an `AudioFillOrder` of its own:
  `dispatch_audio_fill` runs every callback once per chunk of a ring buffer sized by
  `AudioConfig`, in that order, mixing their samples and scaling the sum by the
  world's `GlobalAudioGain`; the device callback drains the ring and underruns to
  silence. A fill commits the commands its sources queued, the way a frame
  does, so a caller that fills a world the host does not otherwise drive still
  gets what a source built. The gain is one per world: `GlobalAudioGain` is a
  singleton key, so the dispatcher reads the one the world holds and a second
  spawn panics rather than picking between them by no stated rule. A scene
  shapes a source's loudness in its own fill instead.
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

  Every world follows its `AudioConfig.output`, threaded or stepped alike:
  `Pulled` opens no device at all — there is nothing to play to and no
  callback to pull the ring — and publishes no format either, so a caller
  that wants sound installs its sources up front and calls
  `dispatch_audio_fill` itself when it wants a chunk, which is what the
  demo's headless run does, rendering the mix to a file. `OnRequest` waits
  for a `request_audio_output` job and then keeps trying every tick until a
  device appears. The demo sends the request from the handler of the
  panel's Play button on every platform: a browser allows a Web Audio
  context to start only inside a user gesture, and starting the tracks on
  the same click keeps the sequencer's clock and the context in step
  everywhere else.

- **synth** (on by default, implies audio): the pieces to install, built on
  [rustysynth-ext](https://docs.rs/rustysynth-ext) — `load_soundfont` parses
  the bytes (typically an `include_bytes!`, so no filesystem is involved), and
  `SynthVoice::from_font` spawns a voice with the `OnAudioFill` source that
  renders it. The voice builds its synthesizer on the first fill, from the
  sample rate that fill names, so a voice can be spawned before the output
  device has said which rate it runs at; messages sent before then wait in the
  voice and are played by that first fill. `midi_job` sends one
  `MidiEventKind` to the voice it names, optionally after a delay in seconds.
  The voices are the whole of the audio world's synth surface, and each is
  bound to one soundfont: `load_soundfont` parses the bytes once, and
  `from_font` can spawn several voices off that one `Arc<SoundFont>`, each
  with its own sixteen MIDI channels and its own synthesizer. One voice is
  the usual shape — a voice plays every instrument the font holds, chosen
  per channel by ProgramChange — so what shares a voice shares channels: a
  channel sounds with whatever instrument its last ProgramChange named, and
  files playing into one voice play on disjoint channels (the demo's
  `music.rs` carves three for the background loop and one for the one-shot
  sequence). A second voice is the shape for a second font, not for a
  second instrument.

  Sequencing belongs to the gameplay world, which owns the clock:
  `MidiSequencer` holds a file, the time reached, the index of the next event
  and its loop point, and `MidiSequencer::advance` sends the events a frame
  made due as `midi_job`s to a voice. Playing, pausing, rewinding and looping
  are fields of that component, read and written where it lives — no job and
  no mailbox stand between a panel and its transport. A voice is shared, so
  whatever cuts a pass short — a pause, a rewind, a loop back to the marker —
  releases the notes that pass left sounding instead of letting them ring into
  the next one, and only those: a sequence keeps the notes it pressed and
  releases them by name, never with a channel-wide all-notes-off that would cut
  another sequence's music. Files come from `load_midi_file_with_loop_type` (an SMF
  file, with the loop convention the caller names) or
  `midi_file` (timed `MidiEventKind`s, sorted here so a caller's list need not
  be). A file's loop markers set the loop point; a passage a file never marked
  can still repeat by writing `loop_point` by hand. The demo's
  `install_synth` shows the whole install as one function, queued to the audio
  world as a job that answers the voice's entity.
- **egui** (on with `render`): [`egui::EguiPanelHost`] lays a world's
  `unlit3d::egui::EguiPanel`s out for a `unlit3d::egui::EguiSource` in another
  world. A panel can only be built where its egui context runs (the why is in
  [unlit3d](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md)'s
  egui section), so the panel's world lays the frame out and tessellates it,
  and the drawing world uploads and records what comes back. Spawn a [`egui::EguiPanelHost`] beside the panels;
  it holds a sender to the drawing world and the
  [`unlit3d::relay::SnapshotReader`] the target comes back through, and the
  source is set up with [`unlit3d::egui::EguiSource::publish_target`] to keep the
  slots frames land in. A frame travels as a job; a tick that could not
  change the picture — a settled panel, no input, the same
  target, no repaint due — sends nothing, so a still egui costs nothing, and the
  frame already there is drawn until a newer one replaces it. A frame the
  drawing world's queue refused is sent again rather than counted as
  delivered, so a full queue delays the picture instead of losing it. The
  clock egui animates against is the frames' own time, summed from each
  frame's duration rather than read off the wall clock: an animation is at the
  same point at the same frame however long the run took, so a frame captured
  for comparison does not depend on how busy the machine was. The two sides
  keep separate contexts, and texture ids are never renumbered (the why is in
  [unlit3d](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md)'s
  egui section).
- **mcp** (off by default): `serve_mcp` serves the host world and every
  world it carries — a nested sub-world, or one on a thread of its own —
  over the
  [Model Context Protocol](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md)
  over stdio, and the `mcp-http` feature adds `serve_http_mcp`: the same
  world set on a TCP port, so a network client reaches a live scene —
  inspected and screenshotted from a terminal while the window, or the
  headless loop, stays up. Each spawn leaves a `WorldSender` and a `WorldName` on the proxy
  entity beside the proxy: the sender is what lets the transport post a
  command into a world it cannot borrow, and the name is what `list_worlds`
  reports. The crate registers recording tools — `start_audio_recording`,
  `stop_audio_recording`, `read_audio_recording`, `cancel_audio_recording`
  and `audio_recording_status` — which drive a recording in the named world
  and either write the mix out to a file or hand it back in memory, so a test
  can assert on the samples without touching the disk. `frame_clock` reports a
  world's frame index, delta time, and the instantaneous and window-averaged
  frame rate, so a client reads the pacing off one call instead of sampling;
  `frame_clocks` does the same for every world the host lists, so the
  scene, physics and audio worlds are compared in one answer.
  Both answers carry the
  same samples in the format the caller names, the way the image tools do: a
  file name's extension picks it, and a tool that answers inline takes a
  `format` argument, so `.flac` — lossless, and the smaller of the two — is
  what a caller gets when it names none, with `.wav` beside it. `request_exit`
  asks a host carrying `ExitRequest` to stop, and `pump_mcp` dispatches
  commands when no window event loop runs, which is what lets a host without
  winit serve MCP at all. A recording is an `OnAudioPostFill` behaviour, so it
  observes the mix the output stream would receive, after every other source
  and the world's `GlobalAudioGain`. The recorder is compiled only for these tools, so `mcp`
  turns `audio` on itself.

## Best practices

Three rules keep a scene's worlds independent, and they are what make one
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
   spawned and despawned through the physics queue; notes go to the audio
   world as jobs, and a whole install — see the demo's `install_synth` — is
   one function wrapped in one job. A snapshot carries
   state *out* of a world, and the world that reads it decides what
   that means; what another world's content *becomes* is a job, so a pose
   gameplay derived from a physics snapshot is sent to the render world
   rather than left in a mailbox for it to apply. The test is whether a line
   of setup could belong to another scene: if it names a thing the game
   itself defines, it belongs in a job.
3. **A scene holds one port per world it talks to.** A world's address is a
   `JobSender` and the subscriptions it made, so the scene keeps those in one
   value of its own — the demo's `Scene` carries the render, physics and audio
   senders and the physics snapshot — and passes it to whatever needs to reach
   a world: a tick body, a spawn function, a control panel. What would
   otherwise be a field per channel on every object is one argument, and an
   object that never sends to the audio world never holds a sender for it.
   The port stays in the caller's crate: the runtime has no scene to describe,
   and a bridge of the caller's own gets the same treatment.

## The application skeleton

The window-facing half of a multi-world app is app-agnostic too, so it
ships with the runtime. The **gpu** module starts the `Gpu` request as a
task of the world it was asked to set up, driven by the pump that opens each
frame, and runs `ready` as a queued command once the answer lands — nothing
blocks, wasm-safe:

```rust,ignore
world.root_scope_mut().spawn(bundle![request_gpu(app, |world, gpu| {
    // ... build the scene where the request started ...
}, |world, error| {
    log::error!("GPU setup failed: {error}");
    // The runtime requests exit after the failed callback returns.
})]);
```

The type itself is `unlit_wgpu::context::GpuContext`, the bootstrap every entry
point here shares: it requests an adapter and then a device whose limits are the
device tier's. `Gpu::present_to` takes the caller's instance and probes the
adapter with a window surface it drops again, so a context outlives the window it
was requested for; `Gpu::headless` is the same request without a window, for tests
and tools. Each names the device it asks for, so a validation message from a
run says which entry point opened it — `unlit3dx` for the requests this crate
makes on an app's behalf. The **present** module is a thin wrapper over `unlit3d::winit::presenter`
that knows how to reach into a sub-world: `setup_presenter` by default has the
scene's pass draw straight into the swap chain (`PresentationPath::Surface`),
and with its offscreen path on builds
`PresentationPath::Intermediate` instead — a persistent target plus a second
pass in the same world that copies it over the surface, which keeps the frame
long enough to read back, so screenshots and snapshot tests ask for it. Both
paths take a `SurfaceOptions`: the present mode and how many frames may be in
flight, which decide whether the swap chain waits for the display.
`present_turn` is the per-frame transaction that ticks the sub-world —
drains its jobs, folds the input it has been fed, runs its tasks, dispatches
the frame — and presents it; a sub-world has no turn of its own, so the
behaviour that drives its frame is also what keeps its input queue from
filling. `pump_presented` keeps frames coming while the window lives, and
`resize_presented` follows the window around. The `app` module's windowed
entry is the assembled version of that behaviour, and the one to reach for
unless a caller is assembling the window itself. The render
bridge a drawing world needs — the asset server and the mesh and camera
helpers around it — is the caller's to build; `unlit3dx_examples` ships one
as `src/render.rs`.

These primitives are the parts an application assembles itself. The **app**
module (feature `app`) is the assembly done once: an application implements
the `App` trait — `spawn_scene` builds its worlds, `setup_render` builds its
pipeline inside the GPU callback — and one of `windowed`, `headless`,
`interactive` or `serve` runs it, wiring the window, the input feed, the
stepped pacing (`Stepping`), the offscreen target and the MCP server. The
run's own switches (`--headless`, `--interactive`, `--mcp-stdio`, `--mcp-http`,
`--stepped`, `--max-frames`, `--capture-frames`, `--capture-out`) — the frame
count ends the run wherever it runs, and says nothing about pacing:
`--interactive` hands the frames to a client and the run paces itself
otherwise. A capture names the frames to keep and the directory they go to;
on a windowed run it also asks for the offscreen path, since a swap chain
image cannot be read back — and the application's `App::Args` are read by one
clap parser, so a single `--help` describes both; the parsed arguments reach
`App::configure` as a structure, unknown names are refused. Logging and the
capture's write-out (`frame_<index>.png` per captured frame, and `audio.flac`
for a headless or interactive run that played anything) live in the module
too, so an application supplies a scene and its soundfont, not a `main`.

Every entry point answers a `RunError`, so a caller branches on what went
wrong rather than reading a message: the command line (`Config`), a scene world
that ended (`Scene(WorldError)`), the render sub-world or its resources
(`RenderWorld`, `Resolve`, `Offscreen`, `Capture`), a world that could not be
stepped (`Step`), the worlds a drain found failed (`Jobs`), or a transport that
would not start (`Stdio`, `Http`). Each keeps its cause reachable through
`std::error::Error::source`, so the `WorldError` behind a `Scene` failure reads
the same whether it arrived through a thread's state, a step's outcome, an MCP
answer or the run's own error. `App::configure` answers `AppConfigError` — its
`Read` variant for a file the arguments named, its `Rejected` variant for a
reason only the application knows — and `ConfigError` folds that together with
the switches the run parses itself.

A windowed capture writes its frames and nothing else. Its frames are named
by the frame loop, whose count a client may drive one step at a time, while
the mix a windowed run plays is filled by the audio device on the device's
own clock; the two have no time base in common, so a `--capture-frames`
selection of a window cannot say which samples belong to frame N. A run that
wants its mix kept on disk is a headless or interactive one, where the frame
loop pulls the samples itself.

## Tests

The integration tests are one binary, and it requires `audio`, `physics` and
`synth`. Running `--no-default-features` leaves the suite out of the run
rather than failing it: Cargo skips a target whose required features are off.
(No case takes a GPU, so `render` is not required.)

`cargo nextest run -p unlit3dx` covers the core: jobs and their queues,
subscriptions — several readers each getting their own snapshot, a
collector that answers nothing leaving the newest in place, a dropped reader
pruning its subscription, and a full queue refusing one — thread stop and
panic reporting, a threaded world's sent jobs running, and the same for a
stepped world — its jobs run on the step, a tick past the end finishes it,
and stepping an entity that names no stepped world, or one that drives
itself on a thread, is refused with a `StepError` rather than skipped. The
integration tests add the bridges: a physics world dropping a ball onto a
floor and reporting the contact, a subscriber queueing the contacts of two
separate landings that a mailbox would have collapsed into one, the audio
world running jobs and reporting its output opening, multi-world job and
snapshot traffic, and a panicking job ending its world with the jobs behind
it left unrun.
Recording is covered too: it captures the mix the device would play, and both
encoders are checked against what they wrote — the WAV byte by byte, the FLAC
stream by decoding it back to the samples it was given, which also catches a
stream that declares a block size it does not use. The audio MCP tools drive
both through the dispatch coverage below. With the `mcp` feature on, the MCP command dispatch
is covered too: world listing, routing to a nested world, and routing to one
on a thread of its own or one the host steps — plus a readable failure for
an entity that carries no world, a world that has stopped, and a queue that
refuses the job — and so are the audio recording tools, from starting a
recording to refusing a second one and writing the captured mix out.
