//! The integration suite for `unlit3d`: every GPU test the crate has, in one
//! binary.
//!
//! Cargo links one binary per test target, and each of these files used to be a
//! target of its own. They share the same harness ([common]), so every one of
//! them paid for the harness and for a link of the whole GPU stack. Here they
//! are modules of a single target: [common] is compiled once, and the files
//! below are compiled into one binary.
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

#[path = "blit.rs"]
mod blit;
#[path = "custom_instances.rs"]
mod custom_instances;
#[path = "custom_pipeline.rs"]
mod custom_pipeline;
#[path = "gltf_scene.rs"]
mod gltf_scene;
#[path = "gpu_ecs.rs"]
mod gpu_ecs;
#[path = "gpu_ui_source.rs"]
mod gpu_ui_source;
#[path = "letterbox.rs"]
mod letterbox;
#[path = "lit_scene.rs"]
mod lit_scene;
#[path = "mcp_server/main.rs"]
mod mcp_server;
#[path = "morphed_scene.rs"]
mod morphed_scene;
#[path = "passes.rs"]
mod passes;
#[path = "skinned_scene.rs"]
mod skinned_scene;

unlit_wgpu_test_util::gpu_test_main!(
    [
        blit::all_tests(),
        custom_instances::all_tests(),
        custom_pipeline::all_tests(),
        gltf_scene::all_tests(),
        gpu_ecs::all_tests(),
        gpu_ui_source::all_tests(),
        letterbox::all_tests(),
        lit_scene::all_tests(),
        mcp_server::all_tests(),
        morphed_scene::all_tests(),
        passes::all_tests(),
        skinned_scene::all_tests(),
    ]
    .concat()
);
