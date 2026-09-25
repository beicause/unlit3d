//! `cargo xtask run-wasm`: build and serve the web example.
//!
//! The pipeline is the standard one for a `wasm32-unknown-unknown` binary a
//! browser runs: build for the target, hand the wasm to `wasm-bindgen` so it
//! gets a JS loader, then put the page and the wasm behind a static file
//! server — WebGPU is only available in a *secure context*, and `localhost`
//! is one, while a `file://` URL is not. The server is built in
//! ([`super::http`]), so nothing needs to be installed.

use std::path::{Path, PathBuf};

use super::{RunWasmArgs, http, step};

/// The binary crate whose example runs on the web.
const EXAMPLE_CRATE: &str = "unlit3d_examples";
/// The wasm binary `wasm-bindgen` reads and names its output after.
const BINARY_NAME: &str = "unlit3d_examples";
/// The target triple the web build runs on.
const TARGET: &str = "wasm32-unknown-unknown";
/// Where the bindgen output lands, inside `target/`.
const OUT_DIR: &str = "generated";
/// The port the example prefers to be served on. Fixed, so the URL to open
/// is predictable; if it is taken, the server tries the ports after it.
const PORT: u16 = 8000;

/// The web page that imports the bindgen output and starts the example.
///
/// The canvas is sized by this page, not by the window attributes the example
/// asks for: winit writes those into the canvas's inline `style`, and `!important`
/// is what lets the page override them so the canvas follows a phone's viewport
/// instead of staying at the desktop size the example requests.
///
/// `touch-action: none` is what makes a finger drag reach the app at all. The
/// Pointer Events specification has `preventDefault()` on `pointerdown` *not*
/// cancel the browser's own panning, so without it a drag is taken over by the
/// page mid-gesture and the app is sent a `pointercancel` instead of the moves.
/// The overflow and overscroll rules keep the page itself from scrolling under
/// the canvas.
const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover" />
    <title>unlit3d + winit</title>
    <style>
      html,
      body {
        margin: 0;
        padding: 0;
        /* The address bar retracts on scroll, so the dynamic viewport unit is
           what keeps the canvas filling the visible area on a phone. */
        height: 100dvh;
        overflow: hidden;
        overscroll-behavior: none;
        background: #101014;
      }
      canvas {
        display: block;
        /* Override the inline size winit sets from the window attributes, so
           the canvas follows the viewport instead of staying at the size the
           example asks for. */
        width: 100% !important;
        height: 100% !important;
        /* Without this the browser claims a drag as its own pan. */
        touch-action: none;
      }
    </style>
  </head>
  <body>
    <script type="module">
      import init from "./unlit3d_examples.js";
      init();
    </script>
  </body>
</html>
"#;

/// Build and serve the web example.
pub fn run(args: &RunWasmArgs) -> Result<(), String> {
    build(args)?;
    bindgen(args)?;

    if args.no_serve {
        println!(
            "built; serve {} with any static file server binding to localhost",
            out_dir(args).display()
        );
        return Ok(());
    }

    http::serve(&out_dir(args), PORT)
}

/// Compile the example for `wasm32-unknown-unknown`.
///
/// The example is a library as well as a binary — Android packages the library
/// and the binary is what a browser runs — so the bin is named explicitly.
/// Building every target instead would build the cdylib for the web, where it
/// is not what runs, and the two share the `unlit3d_examples.wasm` filename.
fn build(args: &RunWasmArgs) -> Result<(), String> {
    let profile: &[&str] = if args.release { &["--release"] } else { &[] };
    let mut cargo = std::process::Command::new("cargo");
    cargo
        .args(["build", "--target", TARGET, "-p", EXAMPLE_CRATE])
        .args(["--bin", BINARY_NAME])
        .args(profile)
        .args(&args.cargo_args);
    step::run(&mut cargo, "the wasm build")
}

/// Turn the wasm into the JS loader a browser imports, and write the page.
fn bindgen(args: &RunWasmArgs) -> Result<(), String> {
    let mut bindgen = std::process::Command::new("wasm-bindgen");
    bindgen
        .arg(wasm_path(args))
        .args(["--target", "web", "--no-typescript"])
        .arg("--out-dir")
        .arg(out_dir(args))
        .arg("--out-name")
        .arg(BINARY_NAME);
    step::run(&mut bindgen, "wasm-bindgen")?;

    // The loader is imported as `./unlit3d_examples.js`, so the page sits
    // beside it. Written only when absent, so a customized one survives.
    let index = out_dir(args).join("index.html");
    if !index.exists() {
        std::fs::write(&index, INDEX_HTML)
            .map_err(|error| format!("writing {}: {error}", index.display()))?;
    }
    Ok(())
}

/// The directory the bindgen output and the page live in.
fn out_dir(args: &RunWasmArgs) -> PathBuf {
    Path::new("target")
        .join(OUT_DIR)
        .join(if args.release { "release" } else { "debug" })
}

/// The wasm `wasm-bindgen` reads.
fn wasm_path(args: &RunWasmArgs) -> PathBuf {
    Path::new("target")
        .join(TARGET)
        .join(if args.release { "release" } else { "debug" })
        .join(format!("{BINARY_NAME}.wasm"))
}

