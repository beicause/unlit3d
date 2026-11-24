//! Driving the page: pointer, wheel, key, text and IME input reaching its
//! document, and the callbacks that watch what Blitz dispatches.

use super::{
    Arc, BlitzDocument, BlitzSource, Code, Ctx, FramePassOrder, FrameSourceOrder, ImeEvent,
    ImeKind, InputEvent, Key, KeyEvent, KeyLocation, Location, Modifiers, MouseEvent, Mutex,
    OnBlitzEvent, Pass, PointerAction, PointerEvent, PointerKind, Seen, Source, TestGpu, TextEvent,
    TouchPhase, WheelUnit, World, bind_target, bundle, click, feed, pixel, pointer_payload,
    refresh_input, render_page, render_watched_page, spawn_input,
};

/// A click reaches the document, toggles the control it lands on, and is
/// reported to the callbacks that watch the page.
pub(super) async fn a_click_toggles_a_checkbox() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="checkbox" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 24px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([20.0, 20.0]));
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter().any(|event| event.name == "click"),
        "the click should reach the page's callbacks, got {seen:?}"
    );
    let toggled = seen
        .iter()
        .rev()
        .find(|event| event.name == "input")
        .expect("toggling the control dispatches an input event");
    assert_eq!(
        toggled.checked,
        Some(true),
        "the control should be checked when its input event is dispatched"
    );
}

/// One tick's input reaches a page once, however many passes draw that page:
/// every pass builds the page's source, and every build reads the same batch.
pub(super) async fn one_tick_reaches_a_shared_page_once() {
    let ctx = Ctx::headless().await;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let outer = gpu.pass();
    // A pass inside the outer one, so both of them reach the page's source.
    let nested = world.scope_mut(outer).spawn(Pass::new(FramePassOrder(1)));

    // Both passes draw, so both reach the source under the nested one.
    gpu.bind_offscreen_target_with(&world, 1, false);
    bind_target(&gpu, &world, nested);

    let input = spawn_input(&mut world);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let source = world.scope_mut(nested).spawn(bundle![Source::new(
        FrameSourceOrder::BLITZ,
        BlitzSource::new()
    )]);
    world.scope_mut(source).spawn(bundle![
        BlitzDocument::new(html),
        OnBlitzEvent::new(move |_scope, _entity, event| {
            let (buttons, client, primary, mods) = pointer_payload(event.data());
            log.lock().expect("the log is not poisoned").push(Seen {
                name: event.data().name().to_owned(),
                value: String::new(),
                checked: None,
                code: None,
                location: None,
                buttons,
                client,
                primary,
                mods,
            });
        }),
    ]);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([20.0, 20.0]));
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let clicks = seen.iter().filter(|event| event.name == "click").count();
    assert_eq!(
        clicks, 1,
        "two passes drawing one page should dispatch the click once, got {seen:?}"
    );
}

/// A window that loses focus takes the page's focus with it, so a field the user
/// left stops taking text.
pub(super) async fn losing_focus_leaves_the_text_field() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="text" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 200px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([40.0, 20.0]));
    gpu.render(&world);
    feed(&world, &input, [InputEvent::FocusChanged(false)]);
    gpu.render(&world);
    feed(
        &world,
        &input,
        [InputEvent::Text(TextEvent("no".to_owned()))],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        !seen.iter().any(|event| event.value == "no"),
        "a field in a window that lost focus should not take text, got {seen:?}"
    );
}

/// A key press that a shortcut does not consume reports its release too, so a
/// page that watches one sees the key go up.
pub(super) async fn a_key_release_reaches_the_page() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [
            InputEvent::Key(KeyEvent {
                key: Key::Enter,
                location: KeyLocation::Standard,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Key(KeyEvent {
                key: Key::Enter,
                location: KeyLocation::Standard,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter().any(|event| event.name == "keyup"),
        "the key's release should reach the page, got {seen:?}"
    );
}

/// A wheel turns the page under the pointer, so a document taller than the
/// target is read by scrolling rather than only by what fits.
pub(super) async fn a_wheel_scrolls_the_page() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div style="height: 64px; background: #ff0000"></div>
        <div style="height: 400px; background: #0000ff"></div>
    </body></html>"#;
    let (mut world, gpu, target, _source, _page) = render_page(&ctx, html);
    let input = spawn_input(&mut world);
    gpu.render_frames(&world, 2);

    let before = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        before[0].abs_diff(255) <= 2 && before[2] <= 2,
        "the page should start at its first block, got {before:?}"
    );

    feed(
        &world,
        &input,
        [
            // The pointer has to be over the page for it to be the one
            // scrolled, and a wheel reports where it is used.
            InputEvent::Mouse(MouseEvent::Moved {
                position: [32.0, 32.0],
            }),
            InputEvent::Mouse(MouseEvent::Wheel {
                delta: [0.0, -64.0],
                unit: WheelUnit::Pixel,
                phase: TouchPhase::Moved,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    let after = pixel(&ctx.device, &ctx.queue, &target, 32, 32).await;
    assert!(
        after[2].abs_diff(255) <= 2 && after[0] <= 2,
        "the wheel should have scrolled the first block out of view, got {after:?}"
    );
}

/// A pointer that leaves the page is reported to it, which is what ends a hover.
pub(super) async fn leaving_the_page_is_reported_to_it() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="left" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [
            InputEvent::Mouse(MouseEvent::Moved {
                position: [20.0, 20.0],
            }),
            InputEvent::Mouse(MouseEvent::Left),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter().any(|event| event.name == "pointerleave"),
        "the pointer's departure should reach the page, got {seen:?}"
    );
}

/// A callback that cancels an event stops Blitz's own reaction to it: the box
/// the click landed on stays as it was, because Rust decided the click meant
/// something else.
pub(super) async fn a_callback_can_cancel_the_default_action() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="checkbox" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 24px; height: 24px">
    </body></html>"#;
    let (mut world, gpu, target, source, seen, input) = render_watched_page(&ctx, html);
    world
        .scope_mut(source)
        .spawn(bundle![OnBlitzEvent::new(|_scope, _entity, event| {
            if event.data().name() == "click" {
                event.prevent_default();
            }
        })]);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([20.0, 20.0]));
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter().any(|event| event.name == "click"),
        "the click should still reach the callbacks, got {seen:?}"
    );
    assert!(
        !seen.iter().any(|event| event.name == "input"),
        "a cancelled click should not toggle the box, got {seen:?}"
    );
    // The box is white inside while it is unchecked and filled with the accent
    // colour once it is checked.
    let inside = pixel(&ctx.device, &ctx.queue, &target, 20, 20).await;
    assert!(
        inside[..3].iter().all(|&channel| channel == 255),
        "a cancelled click should leave the box unchecked, got {inside:?}"
    );
}

/// Input belongs to the frame that read it: a click dispatched once is not
/// dispatched again by the frames that follow it.
pub(super) async fn input_is_dispatched_once() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([20.0, 20.0]));
    gpu.render(&world);
    // The frames after it tick the world, which is what hands the source a
    // batch with nothing in it.
    for _ in 0..3 {
        refresh_input(&world);
        gpu.render(&world);
    }

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let clicks = seen.iter().filter(|event| event.name == "click").count();
    assert_eq!(
        clicks, 1,
        "the click should be dispatched by one frame only, got {seen:?}"
    );
}

/// Text a key produced reaches the focused field, which is what makes the page
/// an editable document rather than a picture of one.
pub(super) async fn typing_reaches_a_text_field() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="text" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 200px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([40.0, 20.0]));
    gpu.render(&world);
    feed(
        &world,
        &input,
        [InputEvent::Text(TextEvent("hi".to_owned()))],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    assert!(
        seen.iter()
            .any(|event| event.name == "input" && event.value == "hi"),
        "the typed text should reach the field, got {seen:?}"
    );
}

// This file's share of the suite's registry: the suite root concatenates
// it with its siblings', and `gpu_test_main!` there runs the result under
// `cargo nextest` natively and in a browser through the wasm export it adds.
/// A pen is a pointer of its own, so a page that reads only the mouse
/// would miss it: the pen press and lift reach the control as a pointer
/// click and toggle it.
pub(super) async fn a_pen_click_toggles_a_checkbox() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="checkbox" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 24px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    let pen = |action: PointerAction| {
        InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Pen,
            id: 0,
            action,
            position: Some([20.0, 20.0]),
            modifiers: Modifiers::default(),
        })
    };
    feed(
        &world,
        &input,
        [
            pen(PointerAction::Pressed),
            pen(PointerAction::Released { cancelled: false }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let toggled = seen
        .iter()
        .rev()
        .find(|event| event.name == "input")
        .expect("a pen click on the control dispatches an input event");
    assert_eq!(
        toggled.checked,
        Some(true),
        "the control should be checked when a pen toggles it"
    );
}

/// A composed character arrives as a preedit and then as a commit, and
/// the commit is what the field inserts: a page that dropped the IME
/// events would show an empty field.
pub(super) async fn an_ime_commit_reaches_a_text_field() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <input type="text" id="box"
               style="position: absolute; left: 8px; top: 8px; width: 200px; height: 24px">
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(&world, &input, click([40.0, 20.0]));
    gpu.render(&world);
    feed(
        &world,
        &input,
        [
            InputEvent::Ime(ImeEvent {
                kind: ImeKind::Preedit {
                    text: "ni".to_owned(),
                    active_range: None,
                },
            }),
            InputEvent::Ime(ImeEvent {
                kind: ImeKind::Commit(CHINESE.to_owned()),
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    // Blitz keeps the undecided preedit in the field's value and replaces it
    // when the commit arrives, so the value holds the composed text beside the
    // preedit that was still showing.
    assert!(
        seen.iter()
            .any(|event| event.name == "input" && event.value.contains(CHINESE)),
        "the committed text should reach the field, got {seen:?}"
    );
    assert!(
        seen.iter().any(|event| event.name == "composition"),
        "the preedit should be reported as a composition, got {seen:?}"
    );
}

/// The character the input-method test commits.
const CHINESE: &str = "你";
/// A key reaches a page as the code and the side of the keyboard it
/// sits on, not only as the character it types: a shortcut matches the
/// physical key, and the left and right shift are two of them.
pub(super) async fn a_key_reaches_the_page_as_a_code_and_a_side() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    // A character key is dropped while it types nothing but text, so the two
    // keys here are ones a page reads as keys: an arrow, and a key the crate
    // only carries the platform code of, on the numeric keypad.
    feed(
        &world,
        &input,
        [
            InputEvent::Key(KeyEvent {
                key: Key::ArrowLeft,
                location: KeyLocation::Standard,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Key(KeyEvent {
                key: Key::Other(37),
                location: KeyLocation::Numpad,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let keys: Vec<_> = seen
        .iter()
        .filter(|event| event.name == "keydown")
        .collect();
    assert_eq!(
        keys.first().and_then(|event| event.code),
        Some(Code::ArrowLeft),
        "a key reaches the page as its physical code, got {keys:?}"
    );
    assert_eq!(
        keys.first().and_then(|event| event.location),
        Some(Location::Standard),
        "and with the side of the keyboard it is on, got {keys:?}"
    );
    assert_eq!(
        keys.last().and_then(|event| event.location),
        Some(Location::Numpad),
        "a keypad key says it is on the keypad, got {keys:?}"
    );
}
