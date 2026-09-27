English | [简体中文](README.zh-CN.md)

# unlit3d_benchmarks

The frame path's benchmarks: how fast a frame is, and where the time went. Both
targets drive the same scenes through [`unlit3d`](../crates/unlit3d/README.md).
A third target covers [`unlit_ecs`](../crates/unlit_ecs/README.md) on its own,
because the shapes the renderer reads a scene through are the ECS's to provide
and are worth measuring without a device in the way.

The three targets are `frame` for throughput, `ecs` for what one component read
costs, and `profile` for a frame's phase breakdown.

The crate is not part of the test run: a benchmark binary has no test harness, so
`cargo xtask test` excludes it and `cargo bench` drives it instead. What the
workspace's tests cover is in the [root README](../README.md#tests-and-benchmarks).

## `frame` — how fast

[Criterion](https://docs.rs/criterion) over the frame path, reported as entities
per second. It has two groups:

- **`frame`** — building one frame of a scene, at 100 through 100 000 entities,
  in both a `visible` and a `culled` variant, so a change in either the draw path
  or the culling path shows up on its own.
- **`spawn world`** — constructing the world itself, which is setup rather than
  per-frame work and is measured separately for that reason.

```text
cargo bench -p unlit3d_benchmarks
cargo bench -p unlit3d_benchmarks --bench frame -- 'frame/visible/100000'
```

Name the target (`--bench frame`) when filtering: a filter only reaches the
target it is passed to, and the `profile` target takes positional arguments of
its own, so `cargo bench -p unlit3d_benchmarks -- <filter>` runs both.

Criterion writes its reports under `target/criterion`, and compares each run
against the previous one. Treat a difference of a few percent as noise: this is
a frame benchmark on a shared machine, and the numbers move with the load on it.
Compare runs taken under similar load, and prefer a repeat over a single sample.

## `ecs` — what a component read costs

Criterion over `unlit_ecs`'s component-access paths, with no device and no frame
around them. It exists because the renderer's resolve pass reads a visible
entity through exactly these shapes, and the difference between them is the
ECS's to explain rather than the renderer's.

The `ecs access` group walks a ladder, each rung adding one part of a lookup to
the one before it, so what a per-entity `World::get` costs can be read apart:

| case | what it adds |
|------|--------------|
| `location` | the entity-table borrow and the metadata read |
| `states_per_entity` | resolving each component's column in the archetype — the `TypeId` search and the downcast |
| `rows_per_entity` | borrowing and reading each row's cell |
| `rows_per_archetype` | the same reads, with the columns resolved once per archetype group |

`get_one` and `get_three` are one and three `World::get` calls per entity — the
shape the resolve pass used to have — and `query` is a walk over the whole
world, for scale. The `ecs grouping` group then runs the grouped shape against
the same work in an order that defeats the per-archetype cache, which is what
makes the grouping, rather than the shape, the thing being measured.

```text
cargo bench -p unlit3d_benchmarks --bench ecs
cargo bench -p unlit3d_benchmarks --bench ecs -- 'access/rows_per_entity/100000'
```

## `profile` — where it went

The same worlds under the `profiling` scopes, printing one line per phase per
frame. This is the target to reach for when a benchmark says a change is slower
and the question becomes which phase it was.

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- [entities] [frames]
```

Both arguments are optional and positional: an entity count runs only that case,
and a frame count overrides the default of two. Without `--features
profile-tracing` the scopes compile away — `profiling::scope!` is a no-op with no
backend — so the run reports no timings at all.

<details>
<summary>What the printed phase tree looks like</summary>

Each phase is named `module.phase`, outer to inner, so the printed span paths
read as the tree they describe. `renderer.frame` is the root; a frame of a scene
with many entities breaks down like this:

```text
renderer.frame
├── renderer.frame.build_sources
│   └── mesh_source.build
│       ├── mesh_source.metadata.upload
│       ├── mesh_source.uniforms.upload
│       ├── mesh_source.global_groups.rebuild
│       ├── scene.cull
│       ├── scene.resolve
│       │   └── scene.resolve.family
│       ├── scene.sort
│       ├── mesh_source.poses.pack
│       ├── mesh_source.poses.upload
│       ├── mesh_source.instances.upload
│       └── mesh_source.assemble
│           ├── mesh_source.assemble.handles
│           └── mesh_source.assemble.draws
├── renderer.frame.resolve_order
├── renderer.frame.record_passes
└── renderer.frame.submit
```

Read them as a breakdown, not as a promise: the scopes cover the frame's own CPU
work, so time spent inside the GPU driver or waiting on the device is not
attributed to any of them.

To pull just the phases of the `visible` cases out of a run:

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- 100000 3 \
  | awk '/^== visible/{f=1} /^== culled/{f=0} f' \
  | grep time.busy
```

</details>

## License

Dual-licensed under MIT or Apache-2.0, at your option.
