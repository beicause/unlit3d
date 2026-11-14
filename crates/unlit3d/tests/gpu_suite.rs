//! The integration suite for `unlit3d`: every GPU test the crate has, in one
//! binary.
//!
//! The files below are modules of a single test target, sharing one
//! compilation of [common]; the repository's `TESTING.md`, in its `Test layers`
//! section, explains why a crate's integration tests are one binary.
//!
//! The registry each module builds with `gpu_tests!` is concatenated below and
//! handed to the runner once. The runner itself is `gpu_test_main!`: it drives
//! the registry through libtest-mimic natively, and exports the `run_test`
//! entry point a browser calls for the wasm suite.
//!
//! The manifest states the features this binary needs (`gltf`, `mcp`, `ui`)
//! rather than the modules repeating `#[cfg(feature = ...)]`: a target whose
//! required features are off is skipped whole.

// `pub`, not private: the harness module carries helpers no case here reaches,
// and a private module makes `pub` items inside it dead code.
pub mod common;

mod blit;
mod custom_instances;
mod custom_pipeline;
mod gltf_scene;
mod gpu_ecs;
mod gpu_ui_source;
mod inverted_hull;
mod letterbox;
mod line_source;
mod lit_scene;
mod mcp_server;
mod morphed_scene;
mod passes;
mod skinned_scene;

unlit_wgpu_test_util::gpu_test_main!(
    [
        blit::all_tests(),
        custom_instances::all_tests(),
        custom_pipeline::all_tests(),
        gltf_scene::all_tests(),
        gpu_ecs::all_tests(),
        gpu_ui_source::all_tests(),
        inverted_hull::all_tests(),
        letterbox::all_tests(),
        line_source::all_tests(),
        lit_scene::all_tests(),
        mcp_server::all_tests(),
        morphed_scene::all_tests(),
        passes::all_tests(),
        skinned_scene::all_tests(),
    ]
    .concat()
);
