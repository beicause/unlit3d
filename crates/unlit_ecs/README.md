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
them as they are, and exports the `EntityProxy` an entity field reflects
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
- **Reading and writing need only a scope.** `&World` can open the root
  scope, which sees everything; a [`Scope`] of a scene sees that scene's
  subtree and nothing beside it. Structural changes — spawning and despawning —
  need a `&mut` [`ScopeMut`], or a [`Commands`] queue applied by the
  driver.
- **The world is `!Send`.** Components live in `RefCell`s, so the world stays on
  the thread that created it — this is where a renderer and other thread-bound
  state belong. Parallelism is the caller's to arrange, by splitting work over
  plain data rather than by sharing the world.
- **There are no resources.** Nothing in the crate marks or tracks a
  "resource"; an entity a caller wants to reach from anywhere is an ordinary
  entity whose handle the caller keeps, optionally marked with a marker
  component of the caller's own.
- **Every `'static` type is a component.** No derive, no registration: a tuple
  is a component too. A *bundle* is therefore not a tuple of components but the
  list [`bundle!`] builds or an [`ArchetypeBuilder`] gathers, and a plain
  `(a, b)` handed to a spawn is one component, the pair. The macro walks the
  syntax while expanding, so `bundle!(a, b)` is two components and a
  parenthesised item is one component, tuples included.
- **There are no systems.** Driving behaviour means the caller reading the
  world and calling the method or closure it wants — or running a *behaviour
  component*, a component that holds a closure and is invoked by whatever
  driver the caller writes. A behaviour that needs to wait returns a future and
  the caller decides when to poll it; no executor is built in.
- **The one relation is the scope.** Every entity carries a [`ScopeOf`]
  pointing at its parent, which is what makes a scene a subtree: the scene's
  scope sees it and its descendants, and nobody else does — a sibling's subtree
  is out of reach in both directions, so two scenes side by side share nothing.
  Any scope also reaches the [`World::shared`] subtree, which is how a
  resource every scene draws through stays put while the scenes are replaced. It
  is the only relation the world maintains, because visibility has to be decided
  on every access and a caller-maintained relation could not be. Any other
  reference between entities is still an [`Entity`] in a component, kept up to
  date by the caller.
- **A borrow conflict is a panic, not a compile error.** Asking for a component
  that is already borrowed, or fetching the same component as `&mut` twice in
  one query, reports the component's name and stops. That is the trade for not
  doing access analysis. An optional component is optional only about the
  component being absent: a cell that cannot be borrowed panics the same way
  instead of reading as `None`.
- **Two entities are scaffolding.** `World::new` creates [`World::root`] — a
  scope whose `ScopeOf` is itself, and the one the root scope opens — and
  [`World::shared`], whose subtree every scope sees. Both can carry components
  and be opened as scopes; neither is counted, queried, despawned or relinked.
  They keep the first two rows of their archetype, so a query steps over them
  once per archetype instead of testing the identity of every row.

## Scopes

A scope is an entity. Every entity carries a [`ScopeOf`] naming its parent, and
that one relation is the whole tree: a scene is a subtree, and what a caller
holds is [`Scope`], an accessor narrowed to one entity's place in it.

```rust
use unlit_ecs::{Entity, World, bundle};

let mut world = World::new();
let scene = world.root_scope_mut().spawn_empty();
let cube = world.scope_mut(scene).unwrap().spawn(bundle!(0u32));
let camera = world.scope_mut(scene).unwrap().spawn(bundle!(0f32));

// The scene's scope sees the scene and everything below it, and nothing beside.
let scope = world.scope(scene).unwrap();
assert_eq!(scope.entity(), scene);
assert_eq!(scope.len(), 3);
assert!(scope.contains(cube) && scope.contains(camera));
assert_eq!(scope.children(scene).len(), 2);
assert!(scope.query::<(&u32, Entity)>().any(|(_, entity)| entity == cube));

// Root is a scope too, and it sees every scope.
assert_eq!(world.root_scope().len(), 3);
```

Two entities are scaffolding rather than content. [`World::root`] is the scope
the root scope opens, and its `ScopeOf` is itself; [`World::shared`] is its
child, and every scope reaches that subtree. Both can carry components and be
opened as scopes, so a resource belongs on either one — but neither is counted,
queried, despawned or relinked, which is why an empty world counts zero rather
than two.

**What a scope sees.** A scope of `caller` sees `candidate` when `candidate`
is alive and one of these holds:

| The candidate is | Seen by |
| --- | --- |
| the shared subtree | every scope |
| anything | the root scope |
| the scope's own entity | that scope |
| below the scope's entity | that scope |

Everything else is out of reach: a scope does not see its ancestors, and it does
not see its siblings or their subtrees. The relation is deliberately asymmetric
— a scene's scope sees the shared resources, and a shared resource's scope does
not reach back into the scene — so "A sees B" never implies "B sees A". A handle
that is not alive is visible from nowhere, the root included: seeing a place is
not the same as there being something in it.

**What a scope may change.** Ownership is stricter than visibility, and it
covers only structural changes: a scope may despawn or relink an entity when the
entity is its own or below it, or when the scope is the root. The shared subtree
is readable from everywhere, but only the root may move or remove what is in it;
the shared entity itself is the root's child and belongs to no scope, so no
scope despawns or relinks it, the root included. Opening a nested scope is a
visibility test again, so a scene scope can open the shared one and spawn a
resource there; despawning that resource afterwards is the root's call, not the
scene's.

**There is no way up.** [`World::scope`] answers `None` for an entity that is
not a scope, and [`Scope::scope`] only descends — into a visible child or into
the shared subtree. A scope cannot name the world it came from, so holding one
is not a way to reach the rest of it.

**Changing the tree.** [`ScopeMut::spawn`] puts the new entity in the scope
itself, and `spawn_at` puts it on a handle reserved by
[`World::reserve_entity`]. [`ScopeMut::despawn`] removes the entity and its
whole subtree, children first, so a child's [`OnDespawn`] still sees its living
ancestors. [`ScopeMut::link`] and `unlink` re-parent an entity inside the
same scope, refusing to move a scope into its own descendant or to touch the
scaffolding; they are pure structure changes and run no lifecycle callback. A
read-only scope queues the same changes through [`Scope::queue`], and the
driver applies them with [`World::apply`].

**Where callbacks land.** A lifecycle callback receives the scope of the
entity's parent, so an `OnSpawn` on a scene's child sees the scene. It cannot
change the tree either — it queues that, the same way a read-only scope does.
The component is borrowed while its callback runs, so a callback that reaches
for its own lifecycle component panics on the borrow; a callback that panics
stays on the row it was spawned with.

**Cost.** Visibility is decided on every access by walking the parent chain, and
the walk is bounded by the number of nodes, so a malformed tree cannot hang it.
The index behind [`World::children`] and [`World::is_descendant`] is written by
the world alongside the `ScopeOf` component, so the two cannot disagree. The
component is the world's: its field is private, a bundle cannot carry it, and a
`ScopeOf` written through the reflection registry is refused, because moving an
entity is [`ScopeMut::link`]'s job. Queries and counts skip the rows a scope
cannot see; [`Scope::archetype`] answers `None` for an empty archetype and for
one the scope only partly sees, [`Scope::archetypes`] yields only the non-empty
archetypes the scope sees in full, and [`Scope::archetype_infos`] reports the
visible length of each.

## What is in the box

[`World`], [`Entity`], [`Location`], [`Bundle`] / [`ArchetypeBuilder`] (and the
[`bundle!`] macro that writes one), [`ScopeOf`] and the
[`Scope`] / [`ScopeMut`] that narrow the world to one scope,
[`OnSpawn`] / [`OnDespawn`] for the callbacks a spawn and a despawn run,
[`Query`] (the type parameter naming what a row yields, with [`Entity`] and
[`Has<C>`] as items and [`QueryIter::by`] as the archetype predicate),
[`Command`] / [`Commands`] for queued structural changes, [`Archetype`] /
[`Archetypes`] for direct storage inspection, [`World::type_name`] for naming a
component type a type-erased reader only knows by
[`TypeId`](core::any::TypeId), [`WorldInfo`] / [`ArchetypeInfo`] / [`EntityInfo`]
for reporting a world's shape as plain data (with `EntityProxy` and
`EntityVecProxy` as the reflection an entity field names), and the specialized
hash containers
[`TypeIdHashMap`], [`EntityHashMap`] and friends.

A query is a type parameter. [`Scope::query`] and [`Scope::query_one`] name what
to fetch, and `Q` is `&C`, `&mut C`, `Option<&C>`, `Option<&mut C>`, [`Entity`],
[`Has<C>`], or a tuple of those, in that order. Each archetype resolves its
columns once, so a yielded row costs no lookup. A borrow lives as long as the
row, so reading two rows at once is allowed and writing the same component twice
without dropping the first borrow panics. `Has<C>` reads presence without
borrowing the cell, and [`QueryIter::by`] narrows the archetypes a query visits
with a closure over [`Archetype`] before any cell is borrowed. [`Scope::location`] and
[`Scope::archetype`] expose the same shape to a caller that holds many entities
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
use unlit_ecs::{Entity, Has, World, bundle};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = World::new();
let cube = world.root_scope_mut().spawn(bundle!(Spin { radians_per_second: 1.0, angle: 0.0 }));
let clock = world.root_scope_mut().spawn(bundle!(0.016f32));

// Drive every `Spin`. The caller picks which entities to touch and in what
// order; the library has no built-in notion of a scene graph.
let delta = *world.root_scope().query_one::<&f32>(clock).unwrap();
for mut spin in world.root_scope().query::<&mut Spin>() {
    spin.angle += spin.radians_per_second * delta;
}
assert_ne!(world.root_scope().query_one::<&Spin>(cube).unwrap().angle, 0.0);

// A predicate narrows the archetypes a query visits, before any cell is
// borrowed: here, everything that is not a clock.
let _ = world
    .root_scope()
    .query::<&Spin>()
    .by(|archetype: &unlit_ecs::Archetype| !archetype.has::<f32>())
    .count();

// `Entity` and `Has<C>` are query items too: presence reads do not borrow.
for (entity, has_spin) in world.root_scope().query::<(Entity, Has<Spin>)>() {
    let _ = (entity, has_spin);
}
```

A query is a type, not a value: `&C` reads a component, `&mut C` writes one,
`Option<&C>` and `Option<&mut C>` make it optional, [`Entity`] yields the
entity, and [`Has<C>`] yields whether `C` is there. `Has<C>` narrows nothing, so
it reports a `false` for an entity that lacks `C` rather than skipping it; a
query that should visit only the carriers pairs it with [`QueryIter::by`],
`by(|archetype| archetype.has::<C>())`. Items do not nest —
`&(a, b)` is one tuple component and `(&a, &b)` is two — and a query with more
components than a tuple impl covers is written by implementing [`Query`]
yourself. [`Scope::query_one`] runs the same query over one entity, so a lookup
by handle and a pass over a scope share one vocabulary.

Spawning and despawning need a `&mut` scope, or a queued command:

```rust
use unlit_ecs::{World, bundle};

let mut world = World::new();
let entity = {
    let commands = world.root_scope().queue();
    commands.spawn(bundle!("entity"))
};
// Nothing has happened yet.
assert!(!world.root_scope().contains(entity));
world.apply();
assert!(world.root_scope().contains(entity));
```

A bundle is what [`bundle!`] writes or what an [`ArchetypeBuilder`] gathers; a
tuple is not one, because a tuple is itself a component. Every item of the list
is one component — a parenthesised tuple stays a tuple, so `bundle!((a, b), c)`
is a tuple component and `c`, and `bundle!(())` is an empty tuple component. Its
width is limited by the compiler's recursion limit, about a hundred components
at the default, and a wider one is built by raising `#![recursion_limit]`:

```rust
use unlit_ecs::{Has, World, bundle};

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle!((1u32, 2.0f32), (true, ())));

// Two components: one tuple, and a nested one.
let scope = world.root_scope();
assert!(scope.query_one::<Has<(u32, f32)>>(entity).unwrap());
assert!(scope.query_one::<Has<(bool, ())>>(entity).unwrap());
assert!(!scope.query_one::<Has<u32>>(entity).unwrap());
```

A bundle written as `..value` is spread into its own components, so a bundle
built elsewhere joins the spawn as itself. The syntax is needed because the
macro walks the source and cannot tell a bundle from a component — written
plainly, a bundle is one component:

```rust
use unlit_ecs::{ArchetypeBuilder, Has, World, bundle};

let mut gathered = ArchetypeBuilder::new();
gathered.push_component(1u32);

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle!(..gathered, 2.0f32));

let scope = world.root_scope();
assert_eq!(scope.query_one::<Has<u32>>(entity), Some(true));
assert_eq!(scope.query_one::<Has<f32>>(entity), Some(true));
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
A crate submits a `crate::mcp::ToolSource` to a link-time table — the same
`inventory` mechanism the `reflect` registry uses — and `McpServer::new` walks
the table to register each one:

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

Components are addressed by name through the `reflect` registry: a component's
crate submits a `crate::reflect::ComponentEntry` to the same kind of link-time
table, so `get_component` and `set_component` can encode and decode a value the
ECS itself has never heard of.

## The MCP server, and what it cannot reach

The transport thread is not the world's thread. It reads the protocol, turns a
call into a `crate::mcp::McpCommand`, and posts it through a channel the host
drains; whoever owns the world runs the job and answers on the command's own
channel. A world is `!Send`, and this is the shape that keeps it that way.

Which world a tool names is the client's to say: `crate::mcp::WorldId` is
`"Host"` for the world the server was built around, or `{"Handle": bits}` for a
world a host entity carries. A host that wants its other worlds addressable
answers `list_worlds` itself, because only it knows which entities carry one.


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
  `OnDespawn` are ordinary components holding a callback, so a world with no
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
