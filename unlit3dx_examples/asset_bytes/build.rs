//! Wires the soundfont at the workspace root into the binary.
//!
//! The build resolves `assets/FluidR3Mono_GM.sf3` next to the workspace root
//! and exports its path as `SOUNDFONT_FILE`, which `src/lib.rs` embeds with
//! `include_bytes!`. The file is gitignored, so a fresh clone has none: the
//! build substitutes an empty stub and the demo runs silent, unless
//! `UNLIT3D_REQUIRE_SOUNDFONT` is set, which fails it instead.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let workspace = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    )
    .join("../../assets/FluidR3Mono_GM.sf3");
    println!("cargo:rerun-if-changed={}", workspace.display());
    println!("cargo:rerun-if-env-changed=UNLIT3D_REQUIRE_SOUNDFONT");
    if workspace.is_file() {
        println!("cargo:rustc-env=SOUNDFONT_FILE={}", workspace.display());
        return;
    }
    // Publishing work sets this: the Pages site and the APK carry the demo, and
    // one that lost its soundfont has no sound and no track controls, which is
    // not something a warning should let through.
    if env::var_os("UNLIT3D_REQUIRE_SOUNDFONT").is_some() {
        panic!(
            "{} is missing and UNLIT3D_REQUIRE_SOUNDFONT is set; fetch the soundfont, \
             or unset the variable to build the demo silent",
            workspace.display(),
        );
    }
    // A soundfont parser rejects an empty file, so the demo reports the miss
    // at startup instead of at build time.
    let stub = out_dir.join("missing.sf3");
    fs::write(&stub, []).expect("the OUT_DIR stub is writable");
    println!("cargo:rustc-env=SOUNDFONT_FILE={}", stub.display());
    println!("cargo:warning=assets/FluidR3Mono_GM.sf3 is missing; the demo will run silent");
}
