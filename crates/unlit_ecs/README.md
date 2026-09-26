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
- **The two worlds differ only in their cells.** [`LocalWorld`] stores
  components in `RefCell`s and is `!Send`, so it stays on the thread that
  created it — this is where a renderer and other thread-bound state belong.
  [`SendWorld`] stores them in `RwLock`s and is `Send + Sync`. Everything else
  is shared code.
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

[`World`] (and the [`LocalWorld`] / [`SendWorld`] aliases), [`Entity`],
[`Bundle`] / [`ArchetypeBuilder`], [`Query`] and [`QueryFilter`] ([`With`],
[`Without`], [`Or`], tuples), [`Command`] / [`Commands`] for queued structural
changes, [`Archetype`] / [`Archetypes`] for direct storage inspection, and the
specialized hash containers [`TypeIdHashMap`], [`EntityHashMap`] and friends.

A [`Commands`] queue exists because structural changes need `&mut World`: a
callback that only holds a shared world queues a spawn or despawn instead, and
the driver applies it with [`World::apply`]. [`Commands::spawn`] reserves an
[`Entity`] immediately, so the handle is usable — storable in a component,
passable to another callback — before the queue is applied.

Iteration is deterministic: archetypes are visited in creation order, rows in
storage order. Archetypes are created on demand and never removed.

## Usage

```rust
use unlit_ecs::{LocalWorld, Query, Without};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = LocalWorld::new();
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
use unlit_ecs::LocalWorld;

let mut world = LocalWorld::new();
let entity = {
    let commands = world.queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.contains(entity));
world.apply();
assert!(world.contains(entity));
```

## Tests

```text
cargo nextest run -p unlit_ecs   # this crate's unit and integration tests
cargo xtask test                 # the whole workspace
```

The tests cover world and query behaviour, deferred commands, and that
[`SendWorld`] can be shared across threads. None of them needs a GPU, so they
run anywhere.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
