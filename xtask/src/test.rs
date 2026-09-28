//! `cargo xtask test`: nextest over the workspace, then the doctests.

use crate::step;

/// The environment variable that narrows a test device to WebGL2's shape.
///
/// Defined by `unlit_wgpu::capabilities::DeviceTier`, and repeated here as a
/// literal because this is a separate crate that does not depend on it.
const TIER_ENV: &str = "UNLIT3D_DEVICE_TIER";

/// The value that asks for the WebGL2 tier.
const WEBGL2_TIER: &str = "webgl2";

/// Run the workspace's tests.
///
/// `cargo nextest run` is the test runner: it gives every test its own
/// process, so one test's device, logger or panic cannot reach another's.
/// Nextest does not run doctests, so those get a `cargo test --doc` pass of
/// their own — the two commands together are what CI runs.
///
/// The targets are named one by one rather than with `--all-targets`, which
/// would also sweep in the benchmarks. A benchmark binary is not a test
/// binary: it has no test harness and its `main` is the benchmark, so nextest
/// cannot list it — `cargo bench` is what runs those.
///
/// The suite runs twice: once against whatever the machine's adapter offers,
/// and once with `UNLIT3D_DEVICE_TIER=webgl2`, which narrows every headless
/// device to the limits and missing downlevel capabilities WebGL2 has. That
/// second pass is what exercises the paths a browser takes — the shader's
/// arrays read through textures instead of storage buffers, and a draw's
/// vertex offset baked into its indices instead of carried in `base_vertex` —
/// on a machine whose native backend is Vulkan, Metal or DX12. A real WebGL2
/// device is not required for it, and cannot be, since none is reachable from
/// a headless test.
pub fn run(release: bool) -> Result<(), String> {
    let profile: &[&str] = if release { &["--release"] } else { &[] };

    nextest(profile, None)?;

    // The tier pass is the one that can fail on a path the plain pass never
    // reaches, so it reports its own name rather than looking like a rerun.
    nextest(profile, Some(WEBGL2_TIER))?;

    let mut doctests = std::process::Command::new("cargo");
    doctests
        .args(["test", "--workspace", "--all-features", "--doc"])
        .args(profile);
    step::run(&mut doctests, "doctests")?;

    Ok(())
}

/// Run the workspace's tests through nextest, optionally under a device tier.
fn nextest(profile: &[&str], tier: Option<&str>) -> Result<(), String> {
    let mut command = std::process::Command::new("cargo");
    command
        .args([
            "nextest",
            "run",
            "--workspace",
            "--lib",
            "--bins",
            "--tests",
            "--all-features",
        ])
        .args(profile);
    let named = match tier {
        Some(tier) => {
            command.env(TIER_ENV, tier);
            format!("nextest ({tier})")
        }
        None => "nextest".to_owned(),
    };
    step::run(&mut command, &named)
}
