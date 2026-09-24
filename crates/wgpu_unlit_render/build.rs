//! Build-time WESL handling: bundle and validate the `shaders/*.wesl` modules
//! into the `wgpu_unlit_render` WESL package.
//!
//! The generated Rust artifact (`OUT_DIR/wgpu_unlit_render.rs`, a `'static`
//! [`wesl::StaticPackage`]) is exposed by the library through
//! `wesl::wesl_pkg!`, so runtime shader composition can import the built-in
//! modules (`import wgpu_unlit_render::mesh_compression;`).
//!
//! The `unlit` module belongs to the built-in pipeline, so it is bundled only
//! when that feature is on; everything else the package carries mirrors the
//! Rust types a caller binds (the camera and frame globals, the mesh decode
//! parameters) and is part of the core.

fn main() {
    println!("cargo:rerun-if-changed=shaders");
    // The feature set selects which modules the package holds, so a change to
    // it has to be seen as a change to the inputs.
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_UNLIT");

    let mut package = wesl::PackageBuilder::new("wgpu_unlit_render")
        .scan_root("shaders")
        .expect("failed to scan WESL files")
        .validate()
        .unwrap_or_else(|error| panic!("WESL validation failed: {error}"));

    if std::env::var_os("CARGO_FEATURE_UNLIT").is_none() {
        package
            .root
            .submodules
            .retain(|module| module.name != UNLIT_MODULE);
    }

    package
        .build_artifact()
        .expect("failed to build the WESL package artifact");
}

/// Name of the WESL module holding the built-in unlit shader.
const UNLIT_MODULE: &str = "unlit";
