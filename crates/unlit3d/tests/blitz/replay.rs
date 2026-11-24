//! Replaying a batch from where it began: the state that stood when the batch
//! was queued, rather than the state it has since folded into.

use super::{
    Arc, BlitzDocument, BlitzSource, Ctx, FrameSourceOrder, InputEvent, InputEvents, Modifiers,
    MouseButton, MouseEvent, Mutex, OnBlitzEvent, Seen, TestGpu, TouchPhase, WheelUnit, World,
    bundle, feed, pointer_payload, refresh_input, render_watched_page, spawn_input,
};

/// A batch is replayed against the state that stood when it was queued, not the
/// state it has since folded into. A release in a batch that also moves reports
/// the press the state still held, so the move before it is a drag rather than a
/// hover.
pub(super) async fn a_batch_replays_from_the_state_that_stood_before_it() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    // The press lands alone, so the next batch is queued while the button is
    // already down.
    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Button {
            position: Some([20.0, 20.0]),
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        })],
    );
    gpu.render(&world);
    seen.lock().expect("the log is not poisoned").clear();

    // The move and the release arrive together. The move is made while the
    // button is down, which the batch's own release has not lifted yet.
    feed(
        &world,
        &input,
        [
            InputEvent::Mouse(MouseEvent::Moved {
                position: [30.0, 20.0],
            }),
            InputEvent::Mouse(MouseEvent::Button {
                position: Some([30.0, 20.0]),
                button: MouseButton::Primary,
                pressed: false,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let move_event = seen
        .iter()
        .find(|event| event.name == "pointermove")
        .unwrap_or_else(|| panic!("the move should reach the page, got {seen:?}"));
    assert_eq!(
        move_event.buttons,
        Some(1),
        "a move before the batch's own release should still hold the button, got {seen:?}"
    );
    assert_eq!(
        move_event.client,
        Some([30.0, 20.0]),
        "the move should be reported where it happened, got {seen:?}"
    );
    assert_eq!(
        move_event.primary,
        Some(true),
        "the mouse is the primary pointer, got {seen:?}"
    );
    let up = seen
        .iter()
        .find(|event| event.name == "pointerup")
        .unwrap_or_else(|| panic!("the release should reach the page, got {seen:?}"));
    assert_eq!(
        up.buttons,
        Some(0),
        "the release should report no button held, got {seen:?}"
    );
}

/// The same replay read the other way round: a press in a batch that begins
/// with a move reports the button as up on that move, because the batch's own
/// press has not happened yet.
pub(super) async fn a_move_before_a_press_reports_the_button_that_still_stood() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    seen.lock().expect("the log is not poisoned").clear();
    feed(
        &world,
        &input,
        [
            InputEvent::Mouse(MouseEvent::Moved {
                position: [20.0, 20.0],
            }),
            InputEvent::Mouse(MouseEvent::Button {
                position: Some([20.0, 20.0]),
                button: MouseButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let move_event = seen
        .iter()
        .find(|event| event.name == "pointermove")
        .unwrap_or_else(|| panic!("the move should reach the page, got {seen:?}"));
    assert_eq!(
        move_event.buttons,
        Some(0),
        "a move before the batch's own press should report no button held, got {seen:?}"
    );
}

/// A batch that begins with a wheel and then moves reports the wheel where the
/// pointer stood when it turned, not where the move left it: the position the
/// batch folded into is the last position of the batch, which is a place the
/// wheel never was.
pub(super) async fn a_wheel_is_reported_where_the_pointer_stood() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div style="height: 64px; background: #ff0000"></div>
        <div style="height: 400px; background: #0000ff"></div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Moved {
            position: [20.0, 20.0],
        })],
    );
    gpu.render(&world);
    seen.lock().expect("the log is not poisoned").clear();

    feed(
        &world,
        &input,
        [
            InputEvent::Mouse(MouseEvent::Wheel {
                delta: [0.0, -32.0],
                unit: WheelUnit::Pixel,
                phase: TouchPhase::Moved,
                modifiers: Modifiers::default(),
            }),
            InputEvent::Mouse(MouseEvent::Moved {
                position: [200.0, 200.0],
            }),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let wheel = seen
        .iter()
        .find(|event| event.name == "wheel")
        .unwrap_or_else(|| panic!("the wheel should reach the page, got {seen:?}"));
    assert_eq!(
        wheel.client,
        Some([20.0, 20.0]),
        "the wheel should be reported where the pointer stood, not where the batch left it, got {seen:?}"
    );
}

/// A batch that says nothing about the pointer still leaves it as it was: an
/// empty tick, and a tick carrying only a scale change, neither lifts a button
/// nor forgets where the pointer is.
pub(super) async fn a_frame_without_events_keeps_the_pointer_state() {
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
            InputEvent::Mouse(MouseEvent::Moved {
                position: [20.0, 20.0],
            }),
            InputEvent::Mouse(MouseEvent::Button {
                position: Some([20.0, 20.0]),
                button: MouseButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);
    seen.lock().expect("the log is not poisoned").clear();

    // A tick with nothing in it, then a tick carrying only a scale change:
    // neither is an event about the pointer, so neither may reach the page.
    refresh_input(&world);
    gpu.render(&world);
    feed(
        &world,
        &input,
        [InputEvent::ScaleFactorChanged { scale_factor: 1.0 }],
    );
    gpu.render(&world);
    assert!(
        seen.lock().expect("the log is not poisoned").is_empty(),
        "a tick with no pointer events should dispatch none"
    );

    // The press from two frames ago still stands, so the move that follows is
    // made with the button down.
    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Moved {
            position: [30.0, 20.0],
        })],
    );
    gpu.render(&world);
    let seen = seen.lock().expect("the log is not poisoned").clone();
    let move_event = seen
        .iter()
        .find(|event| event.name == "pointermove")
        .unwrap_or_else(|| panic!("the move should reach the page, got {seen:?}"));
    assert_eq!(
        move_event.buttons,
        Some(1),
        "a batch with no pointer events should leave the button held, got {seen:?}"
    );
}

/// A world that has never ticked has no batch to replay, so the first frame
/// states a default pointer state rather than a stale one.
pub(super) async fn a_first_frame_replays_from_an_empty_baseline() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (mut world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    // The buffer the harness spawned has ticked once, so the world has a batch:
    // dropping it leaves the source with nothing to replay.
    let input_entity = world
        .root_scope()
        .singleton::<InputEvents>()
        .expect("the harness spawns a buffer");
    let _ = world.root_scope_mut().despawn(input_entity);
    let _ = input;

    seen.lock().expect("the log is not poisoned").clear();
    gpu.render(&world);

    assert!(
        seen.lock().expect("the log is not poisoned").is_empty(),
        "a source with no buffer should dispatch nothing"
    );
}

/// A source that reads only every other batch replays from the state the batch
/// it did read was queued against, so a button pressed in the batch it missed
/// is still held when the next one arrives.
pub(super) async fn a_batch_that_skips_one_still_replays_from_the_state_left() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, html);

    gpu.render_frames(&world, 2);
    seen.lock().expect("the log is not poisoned").clear();
    // Two batches arrive with one frame between them, so the frame reads only
    // the second: the press in the first is lost as an event but not as state.
    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Button {
            position: Some([20.0, 20.0]),
            button: MouseButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        })],
    );
    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Moved {
            position: [30.0, 20.0],
        })],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let move_event = seen
        .iter()
        .find(|event| event.name == "pointermove")
        .unwrap_or_else(|| panic!("the move should reach the page, got {seen:?}"));
    assert_eq!(
        move_event.buttons,
        Some(1),
        "a press the frame never saw should still be held, got {seen:?}"
    );
}

/// A page that is replaced mid-gesture is handed the state the world holds,
/// not the state its own last batch left behind: a rebuilt document starts from
/// what stood when the batch arrived, like any other.
pub(super) async fn a_replaced_page_replays_from_the_world_state() {
    let ctx = Ctx::headless().await;
    let html = r#"<html style="background: #808080"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let red = r#"<html style="background: #ff0000"><body style="margin: 0">
        <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
        </div>
    </body></html>"#;
    let mut world = World::new();
    let gpu = TestGpu::frame_only(&mut world, &ctx);
    let pass = gpu.pass();
    gpu.bind_offscreen_target_with(&world, 1, false);

    let input = spawn_input(&mut world);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let source = gpu.spawn_source(&mut world, FrameSourceOrder::BLITZ, BlitzSource::new());
    let page = world.scope_mut(source).spawn(bundle![
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
    let _ = pass;

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [
            InputEvent::Mouse(MouseEvent::Moved {
                position: [20.0, 20.0],
            }),
            InputEvent::Mouse(MouseEvent::Button {
                position: Some([20.0, 20.0]),
                button: MouseButton::Primary,
                pressed: true,
                modifiers: Modifiers::default(),
            }),
        ],
    );
    gpu.render(&world);

    // The document is rebuilt while the button is still down.
    {
        let mut row = world.scope(source).query_one::<&mut BlitzDocument>(page);
        row.get().set_markup(red);
    }
    seen.lock().expect("the log is not poisoned").clear();

    feed(
        &world,
        &input,
        [InputEvent::Mouse(MouseEvent::Moved {
            position: [30.0, 20.0],
        })],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let move_event = seen
        .iter()
        .find(|event| event.name == "pointermove")
        .unwrap_or_else(|| panic!("the move should reach the page, got {seen:?}"));
    assert_eq!(
        move_event.buttons,
        Some(1),
        "a replaced page should still see the button the world holds, got {seen:?}"
    );
}
