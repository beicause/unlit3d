//! The ECS's component-access paths: what one entity's components cost to read,
//! and what resolving a whole archetype's columns once saves.
//!
//! These are the shapes the renderer's resolve pass can read a visible entity
//! through. `get_three` is one [`World::get`] per component per entity, and
//! `rows_per_archetype` is the same reads with each component's column resolved
//! once per archetype group instead of once per entity. Whether the second is
//! worth its extra bookkeeping — or whether `World::get` could be made cheap
//! enough that the first is fine — is the question this target exists to
//! answer, so the two are measured on equal terms rather than one being treated
//! as the goal.
//!
//! The per-entity cases form a ladder, each adding one part of a lookup to the
//! one before it, so `get_three`'s cost can be read apart:
//!
//! - `location` — the entity-table borrow and the metadata read;
//! - `states_per_entity` — adds the archetype lookup and the match itself;
//! - `rows_per_entity` — adds resolving the query's columns in that archetype
//!   and borrowing each row's cell, which is the `TypeId` search and the
//!   downcast.
//!
//! The rungs say where a per-entity lookup's time goes and therefore which part
//! a change to `Scope::query_one` would have to remove to close the gap; a rung
//! that is small is not what stands between the two shapes. `query` is the
//! shape the renderer's resolve pass takes instead: it resolves the columns once
//! per archetype and the entity list comes from the world rather than a cull,
//! so comparing it with `rows_per_entity` is what the per-archetype resolution
//! is worth.
//!
//! Every function returns a sum so the compiler cannot discard the work.
//!
//! ```text
//! cargo bench -p unlit3d_benchmarks --bench ecs
//! cargo bench -p unlit3d_benchmarks --bench ecs -- 'access/rows_per_entity/100000'
//! ```

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use unlit_ecs::bundle;
use unlit_ecs::{Entity, Query, World};

/// The pipeline an entity draws through. A visible entity always has one.
#[derive(Clone, Copy)]
struct Pipeline(u32);

/// The mesh an entity draws. A visible entity always has one.
#[derive(Clone, Copy)]
struct Mesh(u32);

/// What groups a draw and picks its bind group. Optional, as it is in the
/// renderer.
#[derive(Clone, Copy)]
struct Material(u64);

/// What one visible entity is resolved from: exactly the query the resolve pass
/// reads.
///
/// It is a type rather than a value, so every shape below names the same one and
/// the compiler builds one state per shape instead of looking a descriptor up.
type Resolve = (&'static Pipeline, &'static Mesh, Option<&'static Material>);

/// The sum one row contributes: the three components the resolve pass reads.
fn read(pipeline: &Pipeline, mesh: &Mesh, material: Option<&Material>) -> u64 {
    u64::from(pipeline.0) + u64::from(mesh.0) + material.map_or(0, |m| m.0)
}

/// One access shape: a named pass over a world's candidates, returning a sum so
/// the work cannot be discarded.
type Shape = fn(&World, &[Entity]) -> u64;

/// The entity counts a case walks, from one that fits in cache to one where
/// per-entity bookkeeping dominates.
const COUNTS: [u32; 4] = [100, 1_000, 10_000, 100_000];

/// A world of drawable entities and the candidate lists the access shapes read.
///
/// One entity in eight has no material, so the world holds two archetypes, the
/// `Option` of the resolve tuple actually varies, and the resolve pass has
/// something to group.
struct Fixture {
    world: World,
    /// The candidates in the order culling yields them: grouped by archetype.
    visible: Vec<Entity>,
    /// The same candidates, alternating between the two archetypes, so the same
    /// work runs in an order the per-archetype cache cannot help.
    interleaved: Vec<Entity>,
}

impl Fixture {
    fn new(count: u32) -> Self {
        let mut world = World::new();
        let with_material = count - count / 8;
        for index in 0..count {
            if index < with_material {
                world.root_scope_mut().spawn(bundle![
                    Pipeline(index),
                    Mesh(index),
                    Material(u64::from(index)),
                ]);
            } else {
                world
                    .root_scope_mut()
                    .spawn(bundle![Pipeline(index), Mesh(index)]);
            }
        }

        // Archetypes are visited in creation order and rows in storage order,
        // which is the order culling produces; the empty archetype contributes
        // nothing.
        let visible: Vec<Entity> = world
            .root_scope()
            .archetypes()
            .flat_map(|archetype| archetype.entities().iter().copied())
            .collect();
        let interleaved = interleave(&world, &visible);

        Self {
            world,
            visible,
            interleaved,
        }
    }
}

/// The same entities, round-robin between the archetype runs they came in.
///
/// Only the order changes: the set of candidates and the components they carry
/// do not.
fn interleave(world: &World, entities: &[Entity]) -> Vec<Entity> {
    let mut runs: Vec<(u32, Vec<Entity>)> = Vec::new();
    for &entity in entities {
        let archetype = world
            .root_scope()
            .location(entity)
            .expect("a candidate is alive")
            .archetype();
        match runs.last_mut() {
            Some(run) if run.0 == archetype => run.1.push(entity),
            _ => runs.push((archetype, vec![entity])),
        }
    }

    let mut out = Vec::with_capacity(entities.len());
    for rank in 0..runs.iter().map(|(_, run)| run.len()).max().unwrap_or(0) {
        for (_, run) in &runs {
            if let Some(&entity) = run.get(rank) {
                out.push(entity);
            }
        }
    }
    out
}

/// One [`World::get`] per entity: what a single component costs, entity-table
/// borrow and column resolution included.
fn get_one(world: &World, candidates: &[Entity]) -> u64 {
    let mut sum = 0u64;
    for &entity in candidates {
        let mesh = world
            .root_scope()
            .query_one::<&Mesh>(entity)
            .expect("a candidate has a mesh");
        sum += u64::from(mesh.0);
    }
    sum
}

/// Three [`World::get`] per entity: the shape the resolve pass used to have.
fn get_three(world: &World, candidates: &[Entity]) -> u64 {
    let mut sum = 0u64;
    for &entity in candidates {
        let pipeline = world
            .root_scope()
            .query_one::<&Pipeline>(entity)
            .expect("a candidate has one");
        let mesh = world
            .root_scope()
            .query_one::<&Mesh>(entity)
            .expect("a candidate has a mesh");
        let material = world.root_scope().query_one::<&Material>(entity);
        sum += u64::from(pipeline.0) + u64::from(mesh.0) + material.map_or(0, |m| m.0);
    }
    sum
}

/// The entity-table borrow and the metadata read alone: what every
/// [`World::get`] pays before it touches an archetype.
fn location(world: &World, candidates: &[Entity]) -> u64 {
    let mut sum = 0u64;
    for &entity in candidates {
        let location = world
            .root_scope()
            .location(entity)
            .expect("a candidate is alive");
        sum += u64::from(location.archetype()) + location.row() as u64;
    }
    sum
}

/// `location` plus matching the archetype and resolving the query's columns in
/// it, without fetching a row.
fn states_per_entity(world: &World, candidates: &[Entity]) -> u64 {
    let root = world.root_scope();
    let mut sum = 0u64;
    for &entity in candidates {
        let location = root.location(entity).expect("a candidate is alive");
        let archetype = root
            .archetype(location.archetype())
            .expect("a live entity's archetype exists");
        if !<Resolve as Query>::matches(archetype) {
            continue;
        }
        let state = <Resolve as Query>::fetch_state(archetype);
        black_box(&state);
        sum += location.row() as u64;
    }
    sum
}

/// `states_per_entity` plus borrowing and reading each row's cell.
fn rows_per_entity(world: &World, candidates: &[Entity]) -> u64 {
    let root = world.root_scope();
    let mut sum = 0u64;
    for &entity in candidates {
        let location = root.location(entity).expect("a candidate is alive");
        let archetype = root
            .archetype(location.archetype())
            .expect("a live entity's archetype exists");
        if !<Resolve as Query>::matches(archetype) {
            continue;
        }
        let state = <Resolve as Query>::fetch_state(archetype);
        // SAFETY: nothing else borrows the fixture world while this shape runs,
        // and `Resolve` reads its three components shared.
        let (pipeline, mesh, material) =
            unsafe { <Resolve as Query>::fetch(state, location.row()) };
        sum += read(pipeline, mesh, material);
    }
    sum
}

/// The resolve pass's shape: one query state per archetype run, then every row
/// through that state. Falls back to re-resolving on a miss, so the order of
/// the candidates only changes what it costs, never what it reads.
fn rows_per_archetype(world: &World, candidates: &[Entity]) -> u64 {
    let root = world.root_scope();
    let mut sum = 0u64;
    let mut cached = None;
    let mut state = None;
    for &entity in candidates {
        let location = root.location(entity).expect("a candidate is alive");
        if cached != Some(location.archetype()) {
            cached = Some(location.archetype());
            let archetype = root
                .archetype(location.archetype())
                .expect("a live entity's archetype exists");
            state = <Resolve as Query>::matches(archetype)
                .then(|| <Resolve as Query>::fetch_state(archetype));
        }
        let Some(state) = state.as_ref() else {
            continue;
        };
        // SAFETY: as in `rows_per_entity`: the fixture world is not otherwise
        // borrowed, and `Resolve` only reads.
        let (pipeline, mesh, material) =
            unsafe { <Resolve as Query>::fetch(*state, location.row()) };
        sum += read(pipeline, mesh, material);
    }
    sum
}

/// One `Scope::query_one` per candidate: the shape a resolve pass takes when it
/// looks each candidate up on its own.
fn query_one_three(world: &World, candidates: &[Entity]) -> u64 {
    let root = world.root_scope();
    let mut sum = 0u64;
    for &entity in candidates {
        let Ok(mut row) = root.query_one::<Resolve>(entity) else {
            continue;
        };
        let (pipeline, mesh, material) = row.get();
        sum += read(pipeline, mesh, material);
    }
    sum
}

/// A query walk over every matching entity: the same columns, resolved once per
/// archetype, with the entity list coming from the world rather than a cull.
fn query(world: &World) -> u64 {
    let mut sum = 0u64;
    for (pipeline, mesh, material) in world.root_scope().query::<Resolve>().iter() {
        sum += read(pipeline, mesh, material);
    }
    sum
}

/// The access shapes, side by side, over worlds of several sizes.
fn access(c: &mut Criterion) {
    let mut group = c.benchmark_group("ecs access");
    group.sample_size(50);
    for count in COUNTS {
        let fixture = Fixture::new(count);
        // An entity is the unit of work, so entities per second is the number
        // to compare across sizes.
        group.throughput(Throughput::Elements(u64::from(count)));

        let shapes: [(&str, Shape); 7] = [
            ("location", location),
            ("states_per_entity", states_per_entity),
            ("rows_per_entity", rows_per_entity),
            ("rows_per_archetype", rows_per_archetype),
            ("query_one_three", query_one_three),
            ("get_one", get_one),
            ("get_three", get_three),
        ];
        for (name, shape) in shapes {
            group.bench_with_input(BenchmarkId::new(name, count), &(), |b, ()| {
                b.iter(|| black_box(shape(&fixture.world, &fixture.visible)));
            });
        }
        group.bench_with_input(BenchmarkId::new("query", count), &(), |b, ()| {
            b.iter(|| black_box(query(&fixture.world)));
        });
    }
    group.finish();
}

/// The grouped shape against the same work in an order that defeats the
/// per-archetype cache.
fn grouping(c: &mut Criterion) {
    let count = COUNTS[COUNTS.len() - 1];
    let fixture = Fixture::new(count);

    let mut group = c.benchmark_group("ecs grouping");
    group.sample_size(50);
    group.throughput(Throughput::Elements(u64::from(count)));
    group.bench_with_input(BenchmarkId::from_parameter("grouped"), &(), |b, ()| {
        b.iter(|| black_box(rows_per_archetype(&fixture.world, &fixture.visible)));
    });
    group.bench_with_input(BenchmarkId::from_parameter("interleaved"), &(), |b, ()| {
        b.iter(|| black_box(rows_per_archetype(&fixture.world, &fixture.interleaved)));
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = access, grouping
}
criterion_main!(benches);
