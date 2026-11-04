//! Build-time WESL handling: bundle and validate the `shaders/*.wesl` modules
//! into the `unlit_wgpu` WESL package.
//!
//! The generated Rust artifact (`OUT_DIR/unlit_wgpu.rs`, a `'static`
//! [`wesl::StaticPackage`]) is exposed by the library through
//! `wesl::wesl_pkg!`, so runtime shader composition can import the built-in
//! modules (`import unlit_wgpu::mesh_compression;`).
//!
//! Every module the package carries is part of the core: the built-in entry
//! shaders (`lit`, `line`, `blit`, …) sit alongside the modules that mirror the
//! Rust types a caller binds (the camera and frame globals, the mesh decode
//! parameters). A caller's own pipeline composes any of them the same way the
//! built-in pipelines do.

fn main() {
    println!("cargo:rerun-if-changed=shaders");

    let package = wesl::PackageBuilder::new("unlit_wgpu")
        .scan_root("shaders")
        .expect("failed to scan WESL files")
        .validate()
        .unwrap_or_else(|error| panic!("WESL validation failed: {error}"));

    package
        .build_artifact()
        .expect("failed to build the WESL package artifact");
}
