//! The Blitz-only scene: an HTML document painted over an empty frame.
//!
//! No mesh source and no camera: what the frame shows is the page alone, laid
//! out by Blitz and rasterized by Vello. The scene freezes two frames,
//! `blitz_only/frame_00.webp` and `blitz_only/frame_01.webp`. The first holds
//! the page as its markup declares it; the capture drives the controls between
//! the two, so the second shows the form in the state a reader left it. A source
//! builds its document on its first frame, which is drawn before the loop and
//! is the frame both captures are taken after.

use super::{SceneControl, SceneDef, SceneOptions, TEST_SIZE, spawn_in, spawn_shared};
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

/// The frames the scene freezes, and so the length of its sequence.
const FRAMES: u32 = 2;

/// The scene: one page and nothing else.
pub static SCENE: SceneDef = SceneDef {
    id: "blitz_only",
    title: "Blitz only",
    description: "an HTML page laid out by Blitz, no mesh source and no camera",
    size: TEST_SIZE,
    baseline: None,
    clear: Some(CLEAR_COLOR),
    frames: FRAMES,
    step_seconds: None,
    samples: 1,
    depth: false,
    egui: false,
    reproducible_egui: false,
    build,
};

/// The page the scene paints.
///
/// It exercises the paths a document goes through rather than looking like a
/// web site: block flow with margins and padding, a run of text, a rounded box,
/// a gradient, a positioned box and a border. Each takes a different branch of
/// the paint tree, so a frame that loses one changes the picture.
///
/// It also carries the parts of a page that need more than markup: an image
/// decoded from its `data:` URL, a checkbox, a group of radios with one checked,
/// and an empty text field. Its labels are Chinese, which is what the
/// bundled Noto Sans SC subset is for. The capture drives them before it
/// freezes a frame -- it unticks the checkbox, picks the second radio and types
/// into the field -- so the picture shows the state it left rather than the
/// state the markup states, and the status line holds the name of the last
/// event the page dispatched. Every control is
/// positioned rather than flowed, so the layout does not depend on how an
/// inline-block baseline sorts them.
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
  .row { position: absolute; left: 10px; top: 48px; }
  .block {
    display: inline-block;
    width: 64px;
    height: 40px;
    margin-right: 8px;
    border-radius: 10px;
  }
  .card { background: #d9524a; }
  .gradient { background: linear-gradient(90deg, #2f7d6b, #8ec9a8); }
  .outlined { border: 2px solid #24405e; box-sizing: border-box; }
  .chip {
    position: absolute;
    left: 10px;
    top: 96px;
    width: 72px;
    height: 30px;
    border-radius: 6px;
  }
  .at { position: absolute; font-size: 12px; }
  .strip {
    position: absolute;
    left: 10px;
    top: 164px;
    width: 236px;
    height: 20px;
    border-radius: 10px;
    background: #e8b04b;
    padding-left: 8px;
    box-sizing: border-box;
    font-size: 11px;
    line-height: 20px;
    color: #3a2a10;
  }
</style>
</head>
<body>
  <h1>unlit3d</h1>
  <p>由 Blitz 布局与绘制的页面</p>
  <div class="row"><div class="block card"></div><div class="block gradient"></div><div class="block outlined"></div></div>
  <img class="chip" src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAQ0lEQVR42mPQr802bcq3bi926in3mFjtN60+dHZz9IL2xKXdGav689ZPLtkyvXrn7MZ989sPL+47uYJhVMOohuGrAQAtoYngerfVgwAAAABJRU5ErkJggg==">
  <input id="box" class="at" type="checkbox" checked style="left: 90px; top: 100px">
  <label class="at" for="box" style="left: 114px; top: 99px">勾选</label>
  <input id="a" class="at" type="radio" name="pick" checked style="left: 150px; top: 100px">
  <label class="at" for="a" style="left: 174px; top: 99px">一</label>
  <input id="b" class="at" type="radio" name="pick" style="left: 200px; top: 100px">
  <label class="at" for="b" style="left: 224px; top: 99px">二</label>
  <input id="field" class="at" type="text" style="left: 10px; top: 136px; width: 120px">
  <div class="strip" id="status">等待事件</div>
</body>
</html>"#;

/// Build the Blitz-only scene's world.
fn build(
    world: &mut World,
    _context: RenderContext,
    pass: Entity,
    _ui: Option<Entity>,
    _size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let source = world
        .scope_mut(pass)
        .expect("the pass is alive")
        .spawn(bundle![Source::new(
            FrameSourceOrder::BLITZ,
            BlitzSource::new()
        )]);

    // A capture runs without a window, so nothing would push the clicks and
    // keystrokes the page's controls react to: the scene spawns a writer of its
    // own and feeds them itself. A windowed run leaves input to the window.
    let input = options.own_input.then(|| {
        let input = InputWriter::new();
        // `InputFeed` expands into a writer and the buffer it feeds, so it is
        // spawned as itself: a `bundle![...]` around it would register the feed
        // as one component and spread neither.
        spawn_shared(world, InputFeed(input.clone()));
        input
    });

    spawn_in(world, source, bundle![BlitzDocument::new(PAGE)]);

    // The page reacts to nothing on its own: a click, a keystroke or a tick has
    // no handler inside it, and writing the event name into the status line is
    // the smallest thing the callback can do that shows it arrived.
    spawn_in(
        world,
        source,
        bundle![OnBlitzEvent::new(|_scope, _entity, event| {
            let name = event.name();
            let document = event.document_mut();
            let Some(status) = document.get_element_by_id("status") else {
                return;
            };
            let Some(&text) = document
                .get_node(status)
                .and_then(|node| node.children.first())
            else {
                return;
            };
            event
                .document_mut()
                .mutate()
                .set_node_text(text, &format!("最后事件：{name}"));
        })],
    );

    SceneControl {
        // A page repaints only on what it was told, so the frame that pushes
        // input is the one before the frame the snapshot freezes: input queued
        // here is taken by the next frame's dispatch, which runs before that
        // frame draws.
        advance: Box::new(move |_world, frame, _delta, _size| {
            let Some(input) = &input else {
                return;
            };
            if frame != 0 {
                return;
            }
            input.push_all(click([98.0, 108.0]));
            input.push_all(click([208.0, 108.0]));
            input.push_all(click([40.0, 146.0]));
            input.push(InputEvent::Text(TextEvent("输入".to_owned())));
        }),
        snapshot: Box::new(|frame| {
            (frame < FRAMES).then(|| format!("blitz_only/frame_{frame:02}.webp"))
        }),
    }
}

/// The press and release at a position that a document reads as a click.
fn click(position: [f32; 2]) -> [InputEvent; 2] {
    [
        InputEvent::Mouse(MouseEvent::Button {
            position,
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        }),
        InputEvent::Mouse(MouseEvent::Button {
            position,
            button: MouseButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        }),
    ]
}
