//! How a pointer stops: an ending that names no position, and a window that
//! loses focus with pointers still down.
//!
//! A platform states the end of a gesture without saying where it happened — a
//! canvas cancels a touch, a tablet loses its stylus — and the page is still
//! owed the ending at the place it last saw the pointer. A window that loses
//! focus is never told anything else: the events that would end its pointers go
//! to whatever window took the focus, so the loss itself has to end them.

use keyboard_types::Modifiers as DomModifiers;

use super::{
    Ctx, InputEvent, Modifiers, MouseEvent, PointerAction, PointerEvent, PointerKind, Seen,
    TouchEvent, TouchPhase, feed, render_watched_page,
};

/// The page the ending tests drive.
const HTML: &str = r#"<html style="background: #808080"><body style="margin: 0">
    <div id="target" style="position: absolute; left: 0; top: 0; width: 64px; height: 64px">
    </div>
</body></html>"#;

/// A pen event at `position`, or stating no position at all.
fn pen(action: PointerAction, position: Option<[f32; 2]>) -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        kind: PointerKind::Pen,
        id: 0,
        action,
        position,
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

/// The pointer moving to `position`.
fn hover(position: [f32; 2]) -> InputEvent {
    InputEvent::Mouse(MouseEvent::Moved { position })
}

/// The held modifiers changing, which is what the page reads from a pointer or
/// a wheel event.
fn shift(pressed: bool) -> InputEvent {
    InputEvent::ModifiersChanged(if pressed {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    } else {
        Modifiers::default()
    })
}

/// The events the page saw under `name`.
fn named<'a>(seen: &'a [Seen], name: &str) -> Vec<&'a Seen> {
    seen.iter().filter(|event| event.name == name).collect()
}

/// The last event the page saw under `name`.
fn last<'a>(seen: &'a [Seen], name: &str) -> &'a Seen {
    seen.iter()
        .rev()
        .find(|event| event.name == name)
        .unwrap_or_else(|| panic!("the page should see a {name}, got {seen:?}"))
}

/// A cancelled pen still ends the press it began, and is reported where the pen
/// last was rather than at a place it never reached.
pub(super) async fn a_cancel_without_a_position_ends_the_pen() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [pen(PointerAction::Pressed, Some([20.0, 20.0]))],
    );
    gpu.render(&world);
    // A platform that cancels the gesture has no position to give: the pen left
    // the surface, so there is nowhere to say the cancel happened.
    feed(
        &world,
        &input,
        [pen(PointerAction::Released { cancelled: true }, None)],
    );
    gpu.render(&world);

    let ended = seen.lock().expect("the log is not poisoned").clone();
    let cancelled = last(&ended, "pointercancel");
    assert_eq!(
        cancelled.client,
        Some([20.0, 20.0]),
        "a cancel is reported where the pointer last was, got {ended:?}"
    );
    assert_eq!(
        cancelled.buttons,
        Some(0),
        "and the pointer holds nothing once it is cancelled, got {ended:?}"
    );
    assert!(
        named(&ended, "pointerup").is_empty(),
        "a cancel is not an up, got {ended:?}"
    );

    // The press is over: a move after it is a move with nothing held.
    feed(
        &world,
        &input,
        [pen(PointerAction::Moved, Some([30.0, 20.0]))],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let moved = last(&seen, "pointermove");
    assert_eq!(
        moved.buttons,
        Some(0),
        "a cancelled press stays lifted, got {seen:?}"
    );
}

/// An ordinary release that names no position ends at the place the pointer was,
/// so the page reads the end of the gesture where it had followed it to.
pub(super) async fn a_release_without_a_position_ends_where_the_pointer_was() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [pen(PointerAction::Pressed, Some([20.0, 20.0]))],
    );
    gpu.render(&world);
    feed(
        &world,
        &input,
        [pen(PointerAction::Released { cancelled: false }, None)],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let released = last(&seen, "pointerup");
    assert_eq!(
        released.client,
        Some([20.0, 20.0]),
        "a release with no position ends where the press was, got {seen:?}"
    );
    assert_eq!(
        released.buttons,
        Some(0),
        "and the pointer holds nothing after it, got {seen:?}"
    );
}

/// Leaving the window with no position is still a leave, and is reported where
/// the pointer left.
pub(super) async fn a_leave_without_a_position_is_reported_where_the_pointer_left() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [pen(PointerAction::Moved, Some([20.0, 20.0]))],
    );
    gpu.render(&world);
    feed(&world, &input, [pen(PointerAction::Left, None)]);
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let left = last(&seen, "pointerleave");
    assert_eq!(
        left.client,
        Some([20.0, 20.0]),
        "a leave with no position reports where the pointer left, got {seen:?}"
    );
}

/// A window that loses focus cancels every contact it was holding, oldest
/// first, and a contact that starts after it comes back finds nothing left
/// over: a ghost contact would make the new finger the second one.
pub(super) async fn losing_focus_ends_the_contacts_it_held() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(
        &world,
        &input,
        [
            finger(0, TouchPhase::Started, [20.0, 20.0]),
            finger(1, TouchPhase::Started, [40.0, 20.0]),
        ],
    );
    gpu.render(&world);
    feed(&world, &input, [InputEvent::FocusChanged(false)]);
    gpu.render(&world);
    feed(&world, &input, [InputEvent::FocusChanged(true)]);
    gpu.render(&world);
    feed(
        &world,
        &input,
        [finger(2, TouchPhase::Started, [25.0, 20.0])],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let cancelled = named(&seen, "pointercancel");
    assert_eq!(
        cancelled.len(),
        2,
        "both contacts are cancelled when the window loses focus, got {seen:?}"
    );
    assert_eq!(
        cancelled[0].primary,
        Some(true),
        "the older contact is cancelled as the primary one, got {seen:?}"
    );
    assert_eq!(
        cancelled[1].primary,
        Some(false),
        "and the younger one as the second, got {seen:?}"
    );
    assert!(
        cancelled.iter().all(|event| event.buttons == Some(0)),
        "a cancelled contact holds nothing, got {seen:?}"
    );
    let started = last(&seen, "pointerdown");
    assert_eq!(
        started.primary,
        Some(true),
        "a contact that starts after the window regained focus is the only one down, got {seen:?}"
    );
    assert_eq!(
        started.buttons,
        Some(1),
        "and holds its own button, got {seen:?}"
    );
}

/// The modifiers are let go when the window does: the keys that held them went
/// to whatever window took the focus, so a wheel after it is not reported as
/// shifted.
pub(super) async fn losing_focus_lets_go_of_the_modifiers() {
    let ctx = Ctx::headless().await;
    let (world, gpu, _target, _source, seen, input) = render_watched_page(&ctx, HTML);

    gpu.render_frames(&world, 2);
    feed(&world, &input, [shift(true)]);
    gpu.render(&world);
    feed(&world, &input, [hover([20.0, 20.0])]);
    gpu.render(&world);

    let held_seen = seen.lock().expect("the log is not poisoned").clone();
    let held = last(&held_seen, "pointermove");
    assert_eq!(
        held.mods,
        Some(DomModifiers::SHIFT.bits()),
        "the shift a key states reaches the pointer the page sees, got {held_seen:?}"
    );

    // The loss and what follows it in the same batch: the state the batch is
    // replayed from still holds the shift, so the move is only reported without
    // it if losing focus lets go of the modifiers as it happens.
    feed(
        &world,
        &input,
        [
            InputEvent::FocusChanged(false),
            InputEvent::FocusChanged(true),
            hover([30.0, 20.0]),
        ],
    );
    gpu.render(&world);

    let seen = seen.lock().expect("the log is not poisoned").clone();
    let after = last(&seen, "pointermove");
    assert_eq!(
        after.mods,
        Some(DomModifiers::empty().bits()),
        "a window that lost focus let go of the keys that were held, got {seen:?}"
    );
}
