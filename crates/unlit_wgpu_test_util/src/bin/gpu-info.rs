//! Print the device a test run draws with, as JSON.
//!
//! Run by `cargo xtask print-gpu`, which cannot link wgpu itself: the task
//! runner builds no graphics code, so the description is produced here and
//! printed by the task that wraps it. `UNLIT3D_DEVICE_TIER` picks the tier, the
//! same variable a test run reads.

// Printing it is the whole job, and only a native build has a device to
// print.
#![cfg_attr(
    not(target_arch = "wasm32"),
    expect(
        clippy::print_stdout,
        reason = "printing the description is this binary's job"
    )
)]

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let ctx = unlit_wgpu_test_util::block_on(unlit_wgpu_test_util::Ctx::headless());
        // Nothing reads this but a human and the task that wraps it, so a
        // failure to serialize would be a bug in wgpu's own serde rather than
        // anything a caller could act on.
        let text = serde_json::to_string_pretty(&unlit_wgpu_test_util::GpuInfo::read(&ctx))
            .expect("the device description is serializable");
        println!("{text}");
    }
}
