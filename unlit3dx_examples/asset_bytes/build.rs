//! Wires the soundfont at the workspace root into the binary.
//!
//! The build resolves `assets/FluidR3Mono_GM.sf3` next to the workspace root
//! and exports its path as `SOUNDFONT_FILE`, which `src/lib.rs` embeds with
//! `include_bytes!`. When the file is missing — it is gitignored, so a fresh
//! clone has none — a stub is written instead and the demo runs silent, the
//! same way the soundfont tests skip.

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
    let (file, missing) = if workspace.is_file() {
        (workspace, false)
    } else {
        // A soundfont parser rejects an empty file, so the demo reports the
        // miss at startup instead of at build time.
        let stub = out_dir.join("missing.sf3");
        fs::write(&stub, []).expect("the OUT_DIR stub is writable");
        (stub, true)
    };
    println!("cargo:rustc-env=SOUNDFONT_FILE={}", file.display());
    if missing {
        println!("cargo:warning=assets/FluidR3Mono_GM.sf3 is missing; the demo will run silent");
    }
}
