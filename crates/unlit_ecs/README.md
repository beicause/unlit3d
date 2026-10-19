English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)

# unlit_ecs

A compact archetype ECS. Components live in archetypes, one column per type, and
every component value lives in its own cell — which is what lets a shared
`&World` read *and* write components without any `unsafe`.

The design is deliberately small: no change detection, no events or observers,
and no scheduler. Everything that would need
one of those is done by the caller on the entities themselves. The crate has no
dependencies beyond a hasher and depends on nothing else in the workspace, so it
can be used on its own; it knows nothing about rendering. The one exception is
the optional `reflect` feature, which derives [facet](https://docs.rs/facet) for
the [world information](crate::WorldInfo) structures so a bridge can serialise
them as they are, and exports the [`EntityProxy`] an entity field reflects
through. A reflected field whose type is an entity names that proxy, so the ECS's
own structures and any caller's component spell an entity the same way — as its
[`Entity::to_bits`] integer. Nothing in the ECS reads the reflection.

It is at an **early stage of development** and its API changes freely.

## Model

- **An entity's components are fixed when it is spawned.** Components are only
  read and written, never added or removed; to change the set, despawn the
  entity and spawn a new one. Immutable archetypes are what make the storage
  predictable and remove the leak-prone "component removed" bookkeeping other
  ECS designs need.
- **Reading and writing need only a view.** `&World` is the root view, which
  sees everything; a [`ScopedWorld`] of a scene sees that scene's subtree and
  nothing beside it. Structural changes — spawning and despawning — need a
  `&mut` view, or a [`Commands`] queue applied by the driver.
- **The world is `!Send`.** Components live in `RefCell`s, so the world stays on
  the thread that created it — this is where a renderer and other thread-bound
  state belong. Parallelism is the caller's to arrange, by splitting work over
  plain data rather than by sharing the world.
- **There are no resources.** Nothing in the crate marks or tracks a
  "resource"; an entity a caller wants to reach from anywhere is an ordinary
  entity whose handle the caller keeps, optionally marked with a marker
  component of the caller's own.
- **Every `'static` type is a component.** No derive, no registration: a tuple
  is a component too. That is what makes a *bundle* a tuple of components rather
  than a hierarchy of them, so a nested tuple is one component and not a group
  of them. To flatten nesting, write the bundle with [`bundle!`], which walks
  the syntax while expanding.
- **There are no systems.** Driving behaviour means the caller reading the
  world and calling the method or closure it wants — or running a *behaviour
  component*, a component that holds a closure and is invoked by whatever
  driver the caller writes. A behaviour that needs to wait returns a future and
  the caller decides when to poll it; no executor is built in.
- **The one relation is the scope.** Every entity carries a [`ScopeOf`]
  pointing at its parent, which is what makes a scene a subtree: a view of that
  entity sees it and its descendants, and nobody else does — a sibling's subtree
  is out of reach in both directions, so two scenes side by side share nothing.
  A view also reaches the [`World::shared`] subtree, which is how a resource
  every scene draws through stays put while the scenes are replaced. It is the only
  relation the world maintains, because visibility has to be decided on every
  access and a caller-maintained relation could not be. Any other reference
  between entities is still an [`Entity`] in a component, kept up to date by
  the caller.
- **A borrow conflict is a panic, not a compile error.** Asking for a component
  that is already borrowed, or fetching the same component as `&mut` twice in
  one query, reports the component's name and stops. That is the trade for not
  doing access analysis. An optional component is optional only about the
  component being absent: a cell that cannot be borrowed panics the same way
  instead of reading as `None`.
- **Two entities are scaffolding.** `World::new` creates [`World::root`] — the
  root view's scope, whose `ScopeOf` is itself — and [`World::shared`], whose
  subtree every view sees. Both can carry components and be named as scopes;
  neither is counted, queried, despawned or relinked.

## What is in the box

[`World`], [`Entity`], [`Location`], [`Bundle`] / [`ArchetypeBuilder`] (and the
[`bundle!`] macro for flattening nested tuples), [`ScopeOf`] and the views
[`ScopedWorld`] / [`ScopedWorldMut`] that narrow the world to one scope,
[`OnSpawn`] / [`OnDespawn`] for the callbacks a spawn and a despawn run,
[`Query`] and [`QueryFilter`] ([`With`], [`Without`], [`Or`], tuples),
[`Command`] / [`Commands`] for queued structural changes, [`Archetype`] /
[`Archetypes`] for direct storage inspection, [`World::type_name`] for naming a
component type a type-erased reader only knows by
[`TypeId`](core::any::TypeId), [`WorldInfo`] / [`ArchetypeInfo`] / [`EntityInfo`]
for reporting a world's shape as plain data (with [`EntityProxy`] and
[`EntityVecProxy`] as the reflection an entity field names), and the specialized
hash containers
[`TypeIdHashMap`], [`EntityHashMap`] and friends.

A query resolves the columns it needs once per archetype and then fetches rows
through that state, so its cost is proportional to the number of *component
sets* rather than to the number of entities. [`ScopedWorld::location`] and
[`ScopedWorld::archetype`] expose the same shape to a caller that holds many entities
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
let cube = world.root_view_mut().spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
let clock = world.root_view_mut().spawn((0.016f32,));

// Drive every `Spin`. The caller picks which entities to touch and in what
// order; the library has no built-in notion of a scene graph.
let delta = world.root_view().get::<f32>(clock).unwrap().to_owned();
for (_, mut spin) in world.root_view().query::<&mut Spin>() {
    spin.angle += spin.radians_per_second * delta;
}
assert_ne!(world.root_view().get::<Spin>(cube).unwrap().angle, 0.0);

// Filters narrow the archetypes a query visits, without fetching anything.
let _ = world
    .root_view()
    .query_filtered::<&Spin, Without<f32>>()
    .map(|(_, spin)| spin.angle)
    .count();
```

Spawning and despawning need a `&mut` view, or a queued command:

```rust
use unlit_ecs::World;

let mut world = World::new();
let entity = {
    let commands = world.root_view().queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.root_view().contains(entity));
world.apply();
assert!(world.root_view().contains(entity));
```

A tuple bundle does not flatten, because a tuple is itself a component. Reach for
`bundle!` when the components are written nested, or when there are more than
sixteen of them. Its width is limited by the compiler's recursion limit — about a
hundred components at the default — and a wider one is built by raising
`#![recursion_limit]`:

```rust
use unlit_ecs::{bundle, World};

let mut world = World::new();
let entity = world.root_view_mut().spawn(bundle!((1u32, 2.0f32), (true, ())));

// The macro flattened the nesting: three components, not a nested tuple.
assert!(world.root_view().has::<u32>(entity));
assert!(world.root_view().has::<f32>(entity));
assert!(world.root_view().has::<bool>(entity));
assert!(!world.root_view().has::<(u32, f32)>(entity));
```

## The MCP server

The `mcp` feature (off by default, native only) turns the crate into a
[Model Context Protocol](https://modelcontextprotocol.io) server that a client
drives over stdio. It lives here because the protocol's transport side has to
name a [`World`] and hand it work: `McpCommand` carries a job, and the thread
that owns the world runs it. Nothing in the bridge borrows across that
boundary, and no other crate owns a type the transport needs.

The crate still keeps its one-dependency shape. The module is gated off on
`wasm32`, where the protocol's transport cannot build, and every dependency it
adds is optional and target-gated, so a browser build carries none of them.

Tools come from the crates that define what they act on, not from this one.
A crate submits a [`ToolSource`](crate::mcp::ToolSource) to a link-time table —
the same `inventory` mechanism the [`reflect`] registry uses — and
`McpServer::new` walks the table to register each one:

```rust,ignore
unlit_ecs::mcp::inventory::submit! {
    ToolSource { extend: register }
}

fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool("my_tool", "What it does.", my_tool));
}
```

`unlit_ecs` itself registers the entity and component tools —
`world_summary`, `list_components`, `list_archetypes`, `list_entities`,
`get_entity`, `get_component`, `set_component`, `spawn_entity`,
`despawn_entity` — which need nothing but the ECS; `unlit3d` registers the
rendering ones and `unlit3dx` the audio ones. None of those crates is a
dependency of this one, and the server a host builds carries all of them.

Every entity tool takes an optional `scope` argument naming the scope to act in,
defaulting to the root. A client that names a scene sees only that scene's subtree, so
one MCP session can inspect a world's private parts without the host exposing them.
An operation the scope does not own — a spawn outside its subtree, a despawn of an
ancestor — is refused rather than silently redirected.

Components are addressed by name through the [`reflect`] registry: a
component's crate submits a [`ComponentEntry`](crate::reflect::ComponentEntry)
to the same kind of link-time table, so `get_component` and `set_component`
can encode and decode a value the ECS itself has never heard of.

## The MCP server, and what it cannot reach

The transport thread is not the world's thread. It reads the protocol, turns a
call into a [`McpCommand`](crate::mcp::McpCommand), and posts it through a
channel the host drains; whoever owns the world runs the job and answers on
the command's own channel.
A world is `!Send`, and this is the shape that keeps it that way.

Which world a tool names is the client's to say:
[`WorldId`](crate::mcp::WorldId) is `"Host"` for the world the server was built
around, or `{"Handle": bits}` for a world a host
entity carries. A host that wants its other worlds addressable answers
`list_worlds` itself, because only it knows which entities carry one.


## Why the ECS is this small

<details>
<summary>The omissions are deliberate, and each has a substitute</summary>

- **No change detection.** In OOP an object maintains its own state internally;
  tracking what changed is the caller's business.
- **One relation, the scope, and no other.** A `Children` or `ChildOf` component would be a
  reference the caller keeps up to date; the scope is the one that cannot be,
  because visibility is decided on every access. Anything else stays a plain
  entity in a component.
- **Lifecycle callbacks run on the entity, not on the world.** `OnSpawn` and
  `OnDespawn` are ordinary components holding callbacks, so a world with no
  lifecycle components pays nothing for the feature and a caller can write its own
  runner instead.
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

## Tests

```text
cargo nextest run -p unlit_ecs   # this crate's unit and integration tests
cargo xtask test                 # the whole workspace
```

The tests cover world and query behaviour and deferred commands. The test
layers as a whole, and what CI runs, are described in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

This crate's access paths are benchmarked by
[`unlit3d_benchmarks`](../../unlit3d_benchmarks/README.md)'s `ecs` target, which
is where the cost of a `World::get` against resolving a whole archetype's
columns once is measured:

```text
cargo bench -p unlit3d_benchmarks --bench ecs
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
