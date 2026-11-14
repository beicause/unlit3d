//! The Blitz-only scene: an HTML document painted over an empty frame.
//!
//! No mesh source and no camera: what the frame shows is the page alone, laid
//! out by Blitz and rasterized on the CPU. The snapshot `blitz_only.webp`
//! freezes the second frame, like every other overlay scene: the first frame of
//! a source is the one that builds its document.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE};
use unlit_ecs::bundle;
use unlit3d::prelude::*;

/// The colour the scene clears to.
///
/// The page covers the target, so this shows only through the gaps a page with a
/// transparent canvas would leave; it is still stated so a change to the page
/// leaves a frame that differs from the snapshot rather than one that hides the
/// regression behind the clear colour.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.02,
    b: 0.02,
    a: 1.0,
};

/// The scene: one page and nothing else.
pub static SCENE: SceneDef = SceneDef {
    id: "blitz_only",
    title: "Blitz only",
    description: "an HTML page laid out by Blitz, no mesh source and no camera",
    size: TEST_SIZE,
    baseline: None,
    clear: Some(CLEAR_COLOR),
    frames: 2,
    step_seconds: None,
    samples: 1,
    depth: false,
    egui: false,
    reproducible_egui: false,
    build,
};

/// The page the scene paints.
///
/// It is written to exercise the paths a document goes through rather than to
/// look like a web site: block flow with margins and padding, an inline run of
/// text, a rounded box, a gradient, a positioned box and a border. Every one of
/// them takes a different branch of the paint tree, so a frame that loses one
/// changes the picture.
const PAGE: &str = r#"<!doctype html>
<html>
<head>
<style>
  html, body { margin: 0; }
  body {
    background: #f4f1ea;
    color: #24405e;
    font-size: 13px;
    font-family: monospace;
    width: 256px;
    height: 192px;
  }
  h1 { margin: 8px 10px 2px; font-size: 17px; color: #16324f; }
  p { margin: 0 10px 8px; line-height: 16px; }
  .row { margin: 0 10px; }
  .card {
    display: inline-block;
    width: 64px;
    height: 40px;
    margin-right: 8px;
    border-radius: 10px;
    background: #d9524a;
  }
  .gradient {
    display: inline-block;
    width: 64px;
    height: 40px;
    border-radius: 10px;
    background: linear-gradient(90deg, #2f7d6b, #8ec9a8);
  }
  .outlined {
    display: inline-block;
    width: 64px;
    height: 40px;
    margin-right: 8px;
    border: 2px solid #24405e;
    box-sizing: border-box;
  }
  .strip {
    position: absolute;
    left: 10px;
    top: 150px;
    width: 236px;
    height: 22px;
    border-radius: 11px;
    background: #e8b04b;
  }
</style>
</head>
<body>
  <h1>unlit3d</h1>
  <p>a frame source painted by Blitz</p>
  <div class="row"><div class="card"></div><div class="gradient"></div><div class="outlined"></div></div>
  <div class="strip"></div>
</body>
</html>"#;

/// Build the Blitz-only scene's world.
fn build(
    world: &mut World,
    _context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    _size: (u32, u32),
    _options: SceneOptions,
) -> SceneControl {
    world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(BlitzSource::new(PAGE))]);

    SceneControl {
        // The page is static: it is laid out and rasterized once, and the frames
        // after it reuse that texture.
        advance: Box::new(|_world, _frame, _delta, _size| {}),
        snapshot: Box::new(|frame| (frame == 1).then(|| "blitz_only.webp".to_owned())),
    }
}
