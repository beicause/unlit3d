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
pub const EXAMPLE_CRATE: &str = "unlit3d_examples";
/// The wasm binary `wasm-bindgen` reads and names its output after.
pub const BINARY_NAME: &str = "unlit3d-examples";
/// The target triple the web build runs on.
pub const TARGET: &str = "wasm32-unknown-unknown";
/// Where the bindgen output lands, inside `target/`.
const OUT_DIR: &str = "generated";
/// The port the example prefers to be served on. Fixed, so the URL to open
/// is predictable; if it is taken, the server tries the ports after it.
const PORT: u16 = 8000;

/// The web page that imports the bindgen output and starts the example.
///
/// Public because `cargo xtask test-wasm` serves this same page: it is what
/// starts the example, and a test that used a different one would not be
/// testing what `run-wasm` ships.
///
/// The canvas is sized by this page, not by the window attributes the example
/// asks for: winit writes those into the canvas's inline `style`, and `!important`
/// is what lets the page override them so the canvas follows a phone's viewport
/// instead of staying at the desktop size the example requests. The body's
/// padding is the margin around the canvas, and `box-sizing` keeps that margin
/// inside the viewport rather than pushing the page past it; the safe-area
/// maxima keep the canvas clear of a phone's notch and home indicator.
///
/// The canvas therefore follows the viewport and not its own aspect ratio, so
/// the example's scenes take their camera aspect from the render target's size
/// each frame. A window that changes shape re-aims the projection and shows
/// more or less of the scene; nothing is stretched.
///
/// The corner link leads to the API docs the Pages workflow puts beside the
/// page under `docs/`. A server that has only the example — what `run-wasm`
/// starts — leaves that link without a target.
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
      :root {
        /* The gap between the canvas and the window's edge. */
        --margin: 16px;
      }
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
      body {
        /* The margin around the canvas. `border-box` keeps the padding inside
           the viewport, so the canvas's own size is the viewport less this
           margin. */
        box-sizing: border-box;
        padding:
          max(var(--margin), env(safe-area-inset-top))
          max(var(--margin), env(safe-area-inset-right))
          max(var(--margin), env(safe-area-inset-bottom))
          max(var(--margin), env(safe-area-inset-left));
      }
      canvas {
        display: block;
        /* Override the inline size winit sets from the window attributes, so
           the canvas follows the viewport — less the margin above — instead of
           staying at the size the example asks for. */
        width: 100% !important;
        height: 100% !important;
        /* Without this the browser claims a drag as its own pan. */
        touch-action: none;
      }
      /* The way from the example to the API docs, which the Pages workflow
         publishes beside the page under `docs/`. Fixed to the corner so it takes
         no space from the canvas. */
      .docs {
        position: fixed;
        top: max(var(--margin), env(safe-area-inset-top));
        right: max(var(--margin), env(safe-area-inset-right));
        padding: 6px 10px;
        border-radius: 6px;
        background: rgb(255 255 255 / 8%);
        color: #d0d0d8;
        font: 14px/1 system-ui, sans-serif;
        text-decoration: none;
      }
      .docs:hover {
        background: rgb(255 255 255 / 16%);
      }
    </style>
  </head>
  <body>
    <a class="docs" href="./docs/index.html">API docs</a>
    <script type="module">
      import init from "./unlit3d-examples.js";
      init();
    </script>
  </body>
</html>
"#;

/// Build and serve the web example.
pub fn run(args: &RunWasmArgs) -> Result<(), String> {
    let out = build_example(args.release, &args.cargo_args)?;

    if args.no_serve {
        println!(
            "built; serve {} with any static file server binding to localhost",
            out.display()
        );
        return Ok(());
    }

    http::serve(&out, PORT)
}

/// Compile the example for `wasm32-unknown-unknown`, and bindgen it.
///
/// The example is a library as well as a binary — Android packages the library
/// and the binary is what a browser runs — so the bin is named explicitly.
/// Building every target instead would build the cdylib for the web, where it
/// is not what runs.
///
/// Shared with `cargo xtask test-wasm`, which loads the same page: writing it
/// here keeps the page under test identical to the one `run-wasm` serves.
/// Returns the directory the bindgen output and the page were written to.
pub fn build_example(release: bool, cargo_args: &[String]) -> Result<PathBuf, String> {
    let profile: &[&str] = if release { &["--release"] } else { &[] };
    let mut cargo = std::process::Command::new("cargo");
    cargo
        .args(["build", "--target", TARGET, "-p", EXAMPLE_CRATE])
        .args(["--bin", BINARY_NAME])
        .args(profile)
        .args(cargo_args);
    step::run(&mut cargo, "the wasm build")?;

    let profile_dir = if release { "release" } else { "debug" };
    let out = Path::new("target").join(OUT_DIR).join(profile_dir);
    let wasm = Path::new("target")
        .join(TARGET)
        .join(profile_dir)
        .join(format!("{BINARY_NAME}.wasm"));

    let mut bindgen = std::process::Command::new("wasm-bindgen");
    bindgen
        .arg(&wasm)
        .args(["--target", "web", "--no-typescript"])
        .arg("--out-dir")
        .arg(&out)
        .arg("--out-name")
        .arg(BINARY_NAME);
    step::run(&mut bindgen, "wasm-bindgen")?;

    // The loader is imported as `./unlit3d-examples.js`, so the page sits
    // beside it. The page is this build's, so it is written every time — a
    // stale one restored from a cache would otherwise be served.
    let index = out.join("index.html");
    std::fs::write(&index, INDEX_HTML)
        .map_err(|error| format!("writing {}: {error}", index.display()))?;
    Ok(out)
}
