//! `cargo xtask run-wasm`: build and serve the web examples.
//!
//! The pipeline is the standard one for a `wasm32-unknown-unknown` binary a
//! browser runs: build for the target, hand the wasm to `wasm-bindgen` so it
//! gets a JS loader, then put the pages and the wasm behind a static file
//! server — WebGPU is only available in a *secure context*, and `localhost`
//! is one, while a `file://` URL is not. The server is built in
//! ([`super::http`]), so nothing needs to be installed.
//!
//! The output directory is the one the Pages workflow publishes: a landing
//! page at the root, one directory per example beside it, and the API docs
//! under `docs/` — which only the workflow builds. `--app` picks which
//! examples are built; the landing page links the ones that are there.

use std::path::{Path, PathBuf};

use super::{RunWasmArgs, WebApp, http, step};

/// The target triple the web build runs on.
pub const TARGET: &str = "wasm32-unknown-unknown";
/// Where the bindgen output lands, inside `target/`.
pub const OUT_DIR: &str = "generated";
/// The cargo profile every wasm build of this repository uses, defined in the
/// workspace manifest: release's settings at a size-oriented `opt-level`. A
/// browser downloads and links a smaller wasm than `--release` produces, and no
/// wasm build is a shipped artifact of a release, so none of them pay for one.
pub const PROFILE: &str = "wasm-dev";
/// The port the site prefers to be served on. Fixed, so the URL to open
/// is predictable; if it is taken, the server tries the ports after it.
const PORT: u16 = 8000;

/// One example the site can show.
pub struct Example {
    /// The binary crate the example lives in.
    pub crate_name: &'static str,
    /// The binary `wasm-bindgen` reads and names its output after.
    pub binary: &'static str,
    /// The directory the example is published under.
    pub dir: &'static str,
    /// The page's title.
    pub title: &'static str,
    /// The landing page's link label.
    pub label: &'static str,
    /// The landing page's one-line description.
    pub description: &'static str,
}

/// Every example the site can show, in the order the landing page lists them.
pub const EXAMPLES: &[Example] = &[
    Example {
        crate_name: "unlit3d_examples",
        binary: "unlit3d-examples",
        dir: "unlit3d",
        title: "unlit3d + winit",
        label: "unlit3d",
        description: "The single-world example: glTF scenes, a mesh selector and the built-in UI.",
    },
    Example {
        crate_name: "unlit3dx_examples",
        binary: "unlit3dx-examples",
        dir: "unlit3dx",
        title: "unlit3dx multi-world",
        label: "unlit3dx",
        description: "The multi-world example: gameplay, physics and audio in worlds of their own.",
    },
];

/// The examples `app` asks for.
fn examples(app: WebApp) -> &'static [Example] {
    match app {
        WebApp::Unlit3d => &EXAMPLES[..1],
        WebApp::Unlit3dx => &EXAMPLES[1..],
        WebApp::All => EXAMPLES,
    }
}

/// Build and serve the web site.
pub fn run(args: &RunWasmArgs) -> Result<(), String> {
    let out = build(args.app, &args.cargo_args)?;

    if args.no_serve {
        println!(
            "built; serve {} with any static file server binding to localhost",
            out.display()
        );
        return Ok(());
    }

    http::serve(&out, PORT)
}

/// Build the selected examples and the landing page into one directory.
///
/// Returns the directory to serve: it is the site's root, so the landing page
/// is at `index.html` and each example under its own name.
pub fn build(app: WebApp, cargo_args: &[String]) -> Result<PathBuf, String> {
    let out = Path::new("target").join(OUT_DIR).join(PROFILE);

    // Each run writes only the examples it was asked for, so what an earlier
    // run left behind would otherwise stay on the site — a page linking an
    // example this build did not make, or one built from older sources. The
    // directory is this build's, so it starts empty.
    match std::fs::remove_dir_all(&out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("clearing {}: {error}", out.display())),
    }
    std::fs::create_dir_all(&out)
        .map_err(|error| format!("creating {}: {error}", out.display()))?;

    for example in examples(app) {
        build_example(example, cargo_args, &out.join(example.dir))?;
    }

    // The page is this build's, so it is written every time — a stale one
    // restored from a cache would otherwise list examples that are gone.
    let landing = out.join("index.html");
    std::fs::write(&landing, landing_page(examples(app)))
        .map_err(|error| format!("writing {}: {error}", landing.display()))?;
    Ok(out)
}

/// Compile one example for `wasm32-unknown-unknown`, and bindgen it.
///
/// The build goes through [`PROFILE`], so a browser is handed a small wasm
/// rather than a debug one that takes minutes to compile and several times the
/// size to fetch.
///
/// Shared with `cargo xtask test-wasm`, which loads the same pages: writing
/// them here keeps the pages under test identical to the ones `run-wasm`
/// serves.
pub fn build_example(example: &Example, cargo_args: &[String], out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|error| format!("creating {}: {error}", out.display()))?;

    let mut cargo = std::process::Command::new("cargo");
    cargo
        .args(["build", "--target", TARGET, "-p", example.crate_name])
        .args(["--bin", example.binary])
        .args(["--profile", PROFILE])
        .args(cargo_args);
    step::run(&mut cargo, &format!("the {} wasm build", example.label))?;

    let wasm = Path::new("target")
        .join(TARGET)
        .join(PROFILE)
        .join(format!("{}.wasm", example.binary));

    let mut bindgen = std::process::Command::new("wasm-bindgen");
    bindgen
        .arg(&wasm)
        .args(["--target", "web", "--no-typescript"])
        .arg("--out-dir")
        .arg(out)
        .arg("--out-name")
        .arg(example.binary);
    step::run(&mut bindgen, &format!("wasm-bindgen for {}", example.label))?;

    // The loader is imported as `./<binary>.js`, so the page sits beside it.
    let index = out.join("index.html");
    std::fs::write(&index, example_page(example))
        .map_err(|error| format!("writing {}: {error}", index.display()))?;
    Ok(())
}

/// The page that starts one example.
fn example_page(example: &Example) -> String {
    EXAMPLE_HTML
        .replace("{{title}}", example.title)
        .replace("{{module}}", example.binary)
}

/// The landing page: a link to each example that was built, and the API docs.
///
/// The docs link is relative because the Pages workflow copies them beside the
/// page; a server that has only the examples — what `run-wasm` starts — leaves
/// it without a target.
fn landing_page(examples: &[Example]) -> String {
    let cards: String = examples
        .iter()
        .map(|example| {
            format!(
                "      <li><a href=\"./{dir}/\"><strong>{label}</strong><span>{description}</span></a></li>\n",
                dir = example.dir,
                label = example.label,
                description = example.description,
            )
        })
        .collect();
    LANDING_HTML
        .replace("{{cards}}", &cards)
        .replace("{{docs}}", DOCS_DIR)
}

/// Where the API docs sit under the site's root.
const DOCS_DIR: &str = "docs";

/// The page that starts one example.
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
/// the examples' scenes take their camera aspect from the render target's size
/// each frame. A window that changes shape re-aims the projection and shows
/// more or less of the scene; nothing is stretched.
///
/// The page carries no overlay of its own: whatever the example draws over the
/// canvas — the corner controls, its link to the API docs — is the example's
/// own, so those sit together by construction rather than by two hand-kept
/// rules that can collide. The landing page at the root is where the site's
/// navigation lives; an example page reached directly has only the example's
/// own overlay.
///
/// `touch-action: none` is what makes a finger drag reach the app at all. The
/// Pointer Events specification has `preventDefault()` on `pointerdown` *not*
/// cancel the browser's own panning, so without it a drag is taken over by the
/// page mid-gesture and the app is sent a `pointercancel` instead of the moves.
/// The overflow and overscroll rules keep the page itself from scrolling under
/// the canvas.
const EXAMPLE_HTML: &str = r#"<!DOCTYPE html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover" />
    <title>{{title}}</title>
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
    </style>
  </head>
  <body>
    <script type="module">
      import init from "./{{module}}.js";
      init();
    </script>
  </body>
</html>
"#;

/// The site's landing page: what the examples are, and where the docs are.
const LANDING_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>unlit3d</title>
    <style>
      :root {
        color-scheme: dark;
      }
      html,
      body {
        margin: 0;
        padding: 0;
        background: #101014;
        color: #e8e8ee;
        font-family: system-ui, sans-serif;
      }
      main {
        max-width: 40rem;
        margin: 0 auto;
        padding: 4rem 1.5rem;
      }
      h1 {
        margin: 0 0 0.5rem;
        font-size: 2rem;
      }
      p.lede {
        margin: 0 0 2.5rem;
        color: #a0a0b0;
      }
      ul {
        list-style: none;
        margin: 0;
        padding: 0;
        display: grid;
        gap: 1rem;
      }
      li a {
        display: block;
        padding: 1.25rem 1.5rem;
        border: 1px solid #2a2a34;
        border-radius: 0.75rem;
        background: #16161c;
        color: inherit;
        text-decoration: none;
      }
      li a:hover {
        border-color: #4a4a5a;
        background: #1c1c24;
      }
      li strong {
        display: block;
        margin-bottom: 0.25rem;
        font-size: 1.1rem;
      }
      li span {
        color: #a0a0b0;
        font-size: 0.95rem;
      }
    </style>
  </head>
  <body>
    <main>
      <h1>unlit3d</h1>
      <p class="lede">An unlit rendering library for WebGPU, with a multi-world runtime beside it.</p>
      <ul>
{{cards}}        <li>
          <a href="./{{docs}}/index.html"><strong>API docs</strong></a>
          <span>The workspace's rustdoc, built from the latest commit.</span>
        </li>
      </ul>
    </main>
  </body>
</html>
"#;
