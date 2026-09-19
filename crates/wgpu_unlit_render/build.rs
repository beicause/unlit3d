//! Build-time WESL handling: bundle and validate the `shaders/*.wesl` modules
//! into the `wgpu_unlit_render` WESL package.
//!
//! The generated Rust artifact (`OUT_DIR/wgpu_unlit_render.rs`, a `'static`
//! [`wesl::StaticPackage`]) is exposed by the library through
//! `wesl::wesl_pkg!`, so runtime shader composition can import the built-in
//! modules (`import wgpu_unlit_render::unlit;`).

fn main() {
    println!("cargo:rerun-if-changed=shaders");

    wesl::PackageBuilder::new("wgpu_unlit_render")
        .scan_root("shaders")
        .expect("failed to scan WESL files")
        .validate()
        .inspect_err(|e| eprintln!("{e}"))
        .expect("WESL validation failed")
        .build_artifact()
        .expect("failed to build the WESL package artifact");
}
