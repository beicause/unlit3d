//! A frame's phase breakdown, printed from the `profiling` scopes.
//!
//! The `frame` benchmark answers "how fast"; this answers "where did it go". It
//! builds the same worlds and runs a fixed handful of frames, so the output
//! stays readable — a line per phase per frame, not one per sample of a
//! benchmark.
//!
//! ```text
//! cargo bench -p unlit3d_benchmarks --features profile-tracing \
//!     --bench profile -- [entities] [frames]
//! ```
//!
//! Without `--features profile-tracing` the scopes compile away and the run
//! reports nothing: there is no backend to print them.

// Printing is this target's whole purpose, and the workspace lints deny it
// everywhere else.
#![expect(
    clippy::print_stdout,
    reason = "the target's output is the point of running it"
)]

#[path = "../common/scene.rs"]
mod scene;

use scene::Frame;

/// The entity counts a run walks, matching the `frame` benchmark's.
const COUNTS: [u32; 4] = [1_000, 10_000, 50_000, 100_000];

/// How many frames each case builds. A handful is enough to warm the frame's
/// caches and still leave output that fits on a screen.
const FRAMES: u32 = 2;

/// Install the subscriber that turns the frame path's `profiling` scopes into
/// printed lines.
///
/// `tracing-subscriber`'s formatter reports every span's close with the time it
/// spent busy, which is the number wanted here; `FmtSpan::CLOSE` is what asks
/// for it. The span's full path is printed, so the `profiling::scope!` names —
/// `mesh_source.assemble.handles`, say — read as the phase tree they describe.
///
/// Only this workspace's own spans are recorded: wgpu opens scopes of its own,
/// and a device being dropped emits thousands of them.
#[cfg(feature = "profile-tracing")]
fn install_subscriber() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let targets = Targets::new()
        .with_default(LevelFilter::OFF)
        .with_target("unlit3d", LevelFilter::INFO);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_span_events(FmtSpan::CLOSE)
                .with_timer(tracing_subscriber::fmt::time::uptime())
                .with_filter(targets),
        )
        .init();
}

#[cfg(not(feature = "profile-tracing"))]
fn install_subscriber() {}

/// One case: `count` entities, built `FRAMES` times.
fn case(label: &str, mut frame: Frame, frames: u32) {
    println!("\n== {label} ==");
    for _ in 0..frames {
        frame.build();
    }
    if !cfg!(feature = "profile-tracing") {
        println!("  (rebuild with --features profile-tracing for the phase timings)");
    }
}

fn main() {
    install_subscriber();

    // A size and a frame count may be given positionally, so a single case can
    // be re-run without building every world.
    let mut args = std::env::args().skip(1);
    let entities: Option<u32> = args.next().and_then(|value| value.parse().ok());
    let frames: u32 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(FRAMES);

    let counts: Vec<u32> = match entities {
        Some(count) => vec![count],
        None => COUNTS.to_vec(),
    };
    for count in counts {
        case(&format!("visible {count}"), Frame::visible(count), frames);
        case(&format!("culled {count}"), Frame::culled(count), frames);
    }
}
