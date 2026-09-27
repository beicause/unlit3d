//! The workspace's frame benchmarks: how much CPU a frame of the high-level
//! rendering API costs.
//!
//! The benchmark walks the built-in mesh source's build path over worlds of
//! several sizes, both fully visible and fully culled, so the two halves of the
//! frame — the culling walk and the visible-set work behind it — are separated
//! by their own cases.
//!
//! ```text
//! cargo bench -p unlit3d_benchmarks
//! ```
//!
//! For a breakdown of where a frame's time goes, use the `profile` target
//! instead: with `--features profile-tracing` it prints the frame path's scopes
//! as a timing tree.

use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[path = "../common/scene.rs"]
mod scene;

use scene::Frame;

/// How many entities a benchmark world holds, from one that fits in cache to
/// one where per-entity bookkeeping dominates.
const COUNTS: [u32; 5] = [100, 1_000, 10_000, 50_000, 100_000];

/// Benchmark one frame over worlds of several sizes and shapes.
fn frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");
    group.sample_size(50);
    for count in COUNTS {
        // An entity is the unit of work, so entities per second is the number
        // to compare across sizes.
        group.throughput(Throughput::Elements(u64::from(count)));

        // Every entity is drawn. The frame's own caches are warm after the
        // first build, so the world is built outside the timed closure.
        let mut visible = Frame::visible(count);
        group.bench_with_input(BenchmarkId::new("visible", count), &(), |b, ()| {
            b.iter(|| visible.build());
        });

        // Nothing is drawn: the culling walk without the visible-set work.
        let mut culled = Frame::culled(count);
        group.bench_with_input(BenchmarkId::new("culled", count), &(), |b, ()| {
            b.iter(|| culled.build());
        });
    }
    group.finish();
}

/// Benchmark building a world of `count` entities, so a frame's numbers can be
/// read against the setup they exclude.
fn spawn_world(c: &mut Criterion) {
    let mut group = c.benchmark_group("spawn world");
    group.sample_size(20);
    for count in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(u64::from(count)));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            // Building a world allocates; the state is dropped outside the
            // timed region rather than mixed into it.
            b.iter_batched(|| (), |()| Frame::visible(count), BatchSize::LargeInput);
        });
    }
    group.finish();
}

/// A few seconds per case: the small worlds are over in microseconds, so the
/// default sample count would only lengthen the run.
fn configure() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3))
}

criterion_group! {
    name = benches;
    config = configure();
    targets = frame, spawn_world
}
criterion_main!(benches);
