//! The pointers a page is driven by: what each one states about its own
//! buttons, and how one device's actions leave the others alone.
//!
//! A page reads a pointer event's `buttons` as that pointer's own, and Blitz's
//! default pointermove handling starts a drag only while one is held, so a pen
//! or a finger whose press is reported as nothing is a selection the page never
//! begins.

use super::{
    Ctx, InputEvent, Modifiers, MouseButton, MouseEvent, PointerAction, PointerEvent, PointerKind,
    Seen, TouchEvent, TouchPhase, feed, render_watched_page,
};

/// The page the pointer tests drive.
const HTML: &str = r#"<html style="background: #808080"><body style="margin: 0">
    <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
    </div>
</body></html>"#;

/// A pen event stating `action`.
fn pen(action: PointerAction, position: [f32; 2]) -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Pen,
        id: 0,
        action,
        position: Some(position),
        modifiers: Modifiers::default(),
    })
}

/// A finger event for contact `id`.
fn finger(id: u64, phase: TouchPhase, position: [f32; 2]) -> InputEvent {
    InputEvent::Touch(TouchEvent {
        id,
        phase,
        position,
        force: None,
    })
}

/// The primary mouse button, pressed or released at `position`.
fn mouse_button(pressed: bool, position: [f32; 2]) -> InputEvent {
    InputEvent::Mouse(MouseEvent::Button {
        position: Some(position),
        button: MouseButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
    })
}

/// The first event the page saw under `name`.
fn first<'a>(seen: &'a [Seen], name: &str) -> &'a Seen {
    seen.iter()
        .find(|event| event.name == name)
        .unwrap_or_else(|| panic!("the page should see a {name}, got {seen:?}"))
}

/// The last event the page saw under `name`.
fn last<'a>(seen: &'a [Seen], name: &str) -> &'a Seen {
    seen.iter()
        .rev()
        .find(|event| event.name == name)
        .unwrap_or_else(|| panic!("the page should see a {name}, got {seen:?}"))
}

/// A pen's press is reported as a press: the pointer that goes down already
/// holds its button, so the page starts a drag there and then.
pub(super) async fn a_pen_press_states_the_press_it_makes() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [
            pen(PointerAction::Pressed, [20.0, 20.0]),
            pen(PointerAction::Moved, [30.0, 20.0]),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let down = first(&seen, "pointerdown");
    assert_eq!(
        down.buttons,
        Some(1),
        "a pen that presses holds its button, got {seen:?}"
    );
    assert_eq!(
        down.client,
        Some([20.0, 20.0]),
        "and is reported where it pressed, got {seen:?}"
    );
    let moved = first(&seen, "pointermove");
    assert_eq!(
        moved.buttons,
        Some(1),
        "the pen keeps its button while it stays down, got {seen:?}"
    );
}

/// A finger that stays down keeps its button from one frame to the next, so a
/// page that drags with it keeps dragging.
pub(super) async fn a_finger_that_stays_down_keeps_its_button() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [finger(0, TouchPhase::Started, [20.0, 20.0])],
    );
    gpu.render(&world);
    feed(&world, &input, [finger(0, TouchPhase::Moved, [30.0, 20.0])]);
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let down = first(&seen, "pointerdown");
    assert_eq!(
        down.buttons,
        Some(1),
        "a finger that presses holds its button, got {seen:?}"
    );
    assert_eq!(
        down.primary,
        Some(true),
        "and is the only contact, so it is the primary one, got {seen:?}"
    );
    let moved = last(&seen, "pointermove");
    assert_eq!(
        moved.buttons,
        Some(1),
        "the finger keeps its button into the next frame, got {seen:?}"
    );
    assert_eq!(
        moved.primary,
        Some(true),
        "and stays the primary contact, got {seen:?}"
    );
}

/// A mouse release says nothing about a pen: the pen stays pressed, so the move
/// after it is still a move with the button down.
pub(super) async fn a_mouse_release_does_not_lift_the_pen() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(&world, &input, [pen(PointerAction::Pressed, [20.0, 20.0])]);
    gpu.render(&world);
    feed(
        &world,
        &input,
        [
            mouse_button(true, [40.0, 20.0]),
            mouse_button(false, [40.0, 20.0]),
        ],
    );
    gpu.render(&world);
    feed(&world, &input, [pen(PointerAction::Moved, [30.0, 20.0])]);
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let released = last(&seen, "pointerup");
    assert_eq!(
        released.buttons,
        Some(0),
        "the mouse's own release lifts its button, got {seen:?}"
    );
    let moved = last(&seen, "pointermove");
    assert_eq!(
        moved.buttons,
        Some(1),
        "the pen is still pressed after the mouse let go, got {seen:?}"
    );
}

/// Two fingers are two contacts: the second is not the primary one, and lifting
/// it leaves the first pressed and still the one a drag follows.
pub(super) async fn lifting_one_finger_leaves_the_other_down() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [finger(0, TouchPhase::Started, [20.0, 20.0])],
    );
    gpu.render(&world);
    feed(
        &world,
        &input,
        [finger(1, TouchPhase::Started, [40.0, 20.0])],
    );
    gpu.render(&world);
    feed(&world, &input, [finger(1, TouchPhase::Ended, [40.0, 20.0])]);
    gpu.render(&world);
    feed(&world, &input, [finger(0, TouchPhase::Moved, [25.0, 20.0])]);
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let second = last(&seen, "pointerdown");
    assert_eq!(
        second.primary,
        Some(false),
        "the younger contact is not the primary one, got {seen:?}"
    );
    assert_eq!(
        second.buttons,
        Some(1),
        "but it holds a button of its own, got {seen:?}"
    );
    let lifted = last(&seen, "pointerup");
    assert_eq!(
        lifted.primary,
        Some(false),
        "the lifted contact is the younger one, got {seen:?}"
    );
    assert_eq!(
        lifted.buttons,
        Some(0),
        "a contact that ends afterwards holds nothing, got {seen:?}"
    );
    let moved = last(&seen, "pointermove");
    assert_eq!(
        moved.buttons,
        Some(1),
        "the finger still down is still pressed, got {seen:?}"
    );
    assert_eq!(
        moved.primary,
        Some(true),
        "and is now the primary contact, got {seen:?}"
    );
}
