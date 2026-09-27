English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)

# unlit_ecs

A compact archetype ECS. Components live in archetypes, one column per type, and
every component value lives in its own cell — which is what lets a shared
`&World` read *and* write components without any `unsafe`.

The design is deliberately small: no change detection, no component hooks, no
events or observers, no relations, and no scheduler. Everything that would need
one of those is done by the caller on the entities themselves. The crate has no
dependencies beyond a hasher and depends on nothing else in the workspace, so it
can be used on its own; it knows nothing about rendering.

It is at an **early stage of development** and its API changes freely.

## Model

- **An entity's components are fixed when it is spawned.** Components are only
  read and written, never added or removed; to change the set, despawn the
  entity and spawn a new one. Immutable archetypes are what make the storage
  predictable and remove the leak-prone "component removed" bookkeeping other
  ECS designs need.
- **Reading and writing need only `&World`.** Structural changes — spawning and
  despawning — need `&mut World`, or a [`Commands`] queue applied by the driver.
- **The world is `!Send`.** Components live in `RefCell`s, so the world stays on
  the thread that created it — this is where a renderer and other thread-bound
  state belong. Parallelism is the caller's to arrange, by splitting work over
  plain data rather than by sharing the world.
- **There are no resources.** Nothing in the crate marks or tracks a
  "resource"; an entity a caller wants to reach from anywhere is an ordinary
  entity whose handle the caller keeps, optionally marked with a marker
  component of the caller's own.
- **There are no systems.** Driving behaviour means the caller reading the
  world and calling the method or closure it wants — or running a *behaviour
  component*, a component that holds a closure and is invoked by whatever
  driver the caller writes. A behaviour that needs to wait returns a future and
  the caller decides when to poll it; no executor is built in.
- **No entity relations.** When one entity points at another, the [`Entity`]
  handle goes in a component and the caller keeps it up to date; the world does
  not track or clean up the reference.
- **A borrow conflict is a panic, not a compile error.** Asking for a component
  that is already borrowed, or fetching the same component as `&mut` twice in
  one query, reports the component's name and stops. That is the trade for not
  doing access analysis.

## What is in the box

[`World`], [`Entity`], [`Location`], [`Bundle`] / [`ArchetypeBuilder`],
[`Query`] and [`QueryFilter`] ([`With`], [`Without`], [`Or`], tuples),
[`Command`] / [`Commands`] for queued structural changes, [`Archetype`] /
[`Archetypes`] for direct storage inspection, and the specialized hash
containers [`TypeIdHashMap`], [`EntityHashMap`] and friends.

A query resolves the columns it needs once per archetype and then fetches rows
through that state, so its cost is proportional to the number of *component
sets* rather than to the number of entities. [`World::location`] and
[`World::archetype`] expose the same shape to a caller that holds many entities
— for instance one that culled a scene into a list — so it can group them by
archetype and resolve each archetype's columns once for all of them instead of
re-looking up every entity.

A [`Commands`] queue exists because structural changes need `&mut World`: a
callback that only holds a shared world queues a spawn or despawn instead, and
the driver applies it with [`World::apply`]. [`Commands::spawn`] reserves an
[`Entity`] immediately, so the handle is usable — storable in a component,
passable to another callback — before the queue is applied.

Iteration is deterministic: archetypes are visited in creation order, rows in
storage order. Archetypes are created on demand and never removed.

## Usage

```rust
use unlit_ecs::{Query, Without, World};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = World::new();
let cube = world.spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
let clock = world.spawn((0.016f32,));

// Drive every `Spin`. The caller picks which entities to touch and in what
// order; the library has no built-in notion of a scene graph.
let delta = world.get::<f32>(clock).unwrap().to_owned();
for (_, mut spin) in world.query::<&mut Spin>() {
    spin.angle += spin.radians_per_second * delta;
}
assert_ne!(world.get::<Spin>(cube).unwrap().angle, 0.0);

// Filters narrow the archetypes a query visits, without fetching anything.
let _ = world
    .query_filtered::<&Spin, Without<f32>>()
    .map(|(_, spin)| spin.angle)
    .count();
```

Spawning and despawning need `&mut World`, or a queued command:

```rust
use unlit_ecs::World;

let mut world = World::new();
let entity = {
    let commands = world.queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.contains(entity));
world.apply();
assert!(world.contains(entity));
```

## Why the ECS is this small

<details>
<summary>The omissions are deliberate, and each has a substitute</summary>

- **No change detection, no component hooks.** In OOP an object maintains its
  own state internally; tracking what changed is the caller's business.
- **No entity relations** such as `Children` or `ChildOf`: users manage
  references between entities as they need.
- **No events or observers.** As a substitute, call a component's method
  directly or use a behaviour component. A *behaviour component* holds a
  function pointer or closure that can run accesses to the world inside it, just
  like a system or an observer, and the function inside it can also be async.
  Callbacks like a Godot node's per-frame update, fixed-interval update and
  keyboard/mouse/touch input are then behaviour components on entities, and an
  external caller visits the world and calls them — such features are
  implemented compositionally.
- **No resources.** An entity that wants to be accessed from anywhere is an
  ordinary entity and the caller keeps its `Entity` handle; a resource reference
  is an entity reference, and the library provides no dedicated marker or
  mechanism.
- **An entity's archetype is immutable.** In OOP a class/object's state and
  behavior are immutable, and an immutable archetype also gives state retention
  a mandatory character, avoiding the `RemovedComponents` and component-leak
  pitfalls of change-tracking designs.
- **No systems** (borrowed from [`hecs`](https://docs.rs/hecs)). A system is
  only an external caller's access to the world, or a component holding behavior
  (a closure). So there is no need for complex analysis of system parallelism —
  dependencies, access conflicts — or a multi-threaded scheduler: the external
  caller decides which thread to run on.
- **The world is `!Send`.** GPU resources and the renderer are `!Send`, and on
  `wasm32` they are not even `Send` behind a feature flag, so a world that could
  cross threads would buy nothing there. A `Send` world was tried and removed;
  the storage a world shares is not the thing that makes its work
  partitionable.
- **Nothing is added until something uses it.** A feature that is not currently
  needed does not go in.

</details>

### What other engines do

<details>
<summary>Why not an ECS with change detection and render extraction, or an OOP scene tree</summary>

**bevy**, an ECS architecture. The good: a high degree of modularity, good
plugin extensibility, and an ECS that can make full use of multi-threading. The
bad, mainly from the ECS's drawbacks and complexity: dependencies between
components make errors easy, inheritance and reuse are not obvious enough,
managing complex object state is hard, and representing a scene graph with
components is less intuitive than a tree data structure and objects directly.

1. The component change-detection mechanism is complex, easy to break and easy
   to get wrong, and mechanisms for component dependency updates and derivation
   are almost absent. Synchronizing every frame without keeping components may
   be expensive, while keeping them drags in complex change detection and
   dependency updates. For example, `bevy_render`'s resource-extraction pattern
   is error-prone, especially when components are added and removed: derived
   components leak easily, updates are easily missed, and change detection
   easily goes stale and causes over-updating. The complexity shows in bevy's
   rendering systems often being huge, potentially handling many components with
   many `Changed` and `RemovedComponents` queries. In OOP, an object maintains
   its own state internally.
2. Extensibility shows in adding systems, not in components, and components lack
   OOP's inheritance. For example, extending bevy materials has users register a
   plugin for each custom material, which is tedious. The GPU handles a bevy
   material needs are often tied to the `Handle`s of types like `ShaderBuffer`,
   `Mesh` and `Image`, so users cannot use their own resource handles for
   materials, whereas in OOP extending from a base class is very easy.

Because render resource types are singular, bevy's `RenderAssetUsages` mechanism
is also error-prone: once the main world's data has been extracted as a render
resource, accessing it again errors.

**Godot, Three.js and the like**, using OOP, a scene tree/graph and a
centralized renderer. The good: ease of use is high, and Godot's scene tree
composes and decomposes easily; the API can be very high-level (Godot's node
tree) or fairly low-level (Godot's server mode). The bad:

1. OOP and inheritance are inconvenient for Rust, and simulating OOP by force is
   awkward.
2. It is not good at handling many entities, and traversing the node tree is
   expensive.
3. It does not automatically and fully use parallelism the way an ECS does.
   Parallelism is coarse-grained and needs manual tuning: Godot's parallelism
   shows in servers, and a node's per-frame update logic can choose a background
   thread, but the logic inside servers and nodes cannot run in parallel unless
   the thread pool is called by hand.

</details>

## Tests

```text
cargo nextest run -p unlit_ecs   # this crate's unit and integration tests
cargo xtask test                 # the whole workspace
```

The tests cover world and query behaviour and deferred commands. The test
layers as a whole, and what CI runs, are described in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
