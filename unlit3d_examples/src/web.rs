//! Presentation a pointer press asks for: fullscreen in the browser, and the
//! landscape lock that only fullscreen permits.
//!
//! The example's content has a fixed aspect, so it reads best on a display of
//! roughly that shape. A phone held upright shows it as a band across the
//! middle of the screen, which is the case this module exists for: the first
//! press on the window asks for fullscreen, and a device being held upright is
//! then locked to landscape so the picture fills the display.
//!
//! Only the browser has a page to make fullscreen or a screen to lock, so on
//! every other platform the request is a no-op. The type is still built and
//! still receives the press, so the frame loop needs no platform knowledge of
//! its own.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::JsValue;
#[cfg(target_arch = "wasm32")]
use winit::window::Fullscreen;
use winit::window::Window;

/// The browser's answer to the example's first press on the window.
///
/// A scene's world is rebuilt on every switch, but this concerns the page
/// rather than the scene, so the example keeps one for the life of the app and
/// a switch does not make the app ask again.
pub struct FullscreenRequest {
    /// Whether a press has already been handled.
    requested: bool,
}

impl FullscreenRequest {
    /// Prepare for the request, and return a state that has not asked for
    /// anything yet.
    ///
    /// In the browser this also starts watching for the document entering
    /// fullscreen, which is when the orientation may be locked.
    #[must_use]
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        watch_fullscreen_change();
        Self { requested: false }
    }

    /// Handle a pointer press, asking for fullscreen on the first one.
    ///
    /// Called from the [`MouseInput`](winit::event::WindowEvent::MouseInput)
    /// and [`Touch`](winit::event::WindowEvent::Touch) arms of the event loop,
    /// which are the two ways a press arrives.
    pub fn press(&mut self, window: &Window) {
        if self.requested {
            return;
        }
        self.requested = true;
        #[cfg(target_arch = "wasm32")]
        request_fullscreen(window);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = window;
    }
}

impl Default for FullscreenRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Ask the browser to show `window` fullscreen.
///
/// A browser grants this only to a page with a *transient activation*, which is
/// why the caller makes the request from the press that grants one rather than
/// from the frame loop, where there is no gesture to point at.
///
/// The press reaches the caller through winit, whose web backend queues window
/// events and dispatches them from a task of its own rather than from inside
/// the DOM handler they arrived in. The activation survives that, which is why
/// the request is made from the event rather than from the frame the event is
/// seen in: an activation is granted for seconds, not indefinitely.
///
/// A refusal is logged and the app stays windowed — an `iframe` without the
/// `fullscreen` permission, or a browser with a stricter policy. Nothing is
/// asked for while already fullscreen, so a user who leaves it is not pulled
/// back in by their next press.
#[cfg(target_arch = "wasm32")]
fn request_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        return;
    }
    window.set_fullscreen(Some(Fullscreen::Borderless(None)));
}

/// Lock the screen to landscape while the document is fullscreen and the device
/// is being held upright.
///
/// The call is *not* made next to the fullscreen request, for two reasons that
/// both come from the spec: a lock is only permitted while the document is
/// already fullscreen, and entering fullscreen is asynchronous. So the lock is
/// driven by `fullscreenchange`, which fires once the browser has entered it.
///
/// That event also stands in for a matching `unlock`: leaving fullscreen
/// releases the orientation by itself, so the lock lasts exactly as long as the
/// fullscreen that permitted it and a phone with rotation enabled cannot turn
/// back mid-session.
///
/// A device already held sideways is left alone, so the lock never fights a
/// user holding it the way they want. A refusal — a desktop browser has no
/// orientation to lock — is expected and logged at debug level.
#[cfg(target_arch = "wasm32")]
fn lock_landscape_when_upright() {
    let Ok(orientation) = screen_orientation() else {
        return;
    };
    let upright = matches!(
        orientation.kind,
        web_sys::OrientationType::PortraitPrimary | web_sys::OrientationType::PortraitSecondary
    );
    if !upright {
        return;
    }
    if let Err(error) = orientation
        .value
        .lock(web_sys::OrientationLockType::Landscape)
    {
        log::debug!("the screen orientation cannot be locked: {error:?}");
    }
}

/// The screen's orientation object and the orientation it currently reports.
#[cfg(target_arch = "wasm32")]
struct ScreenOrientation {
    kind: web_sys::OrientationType,
    value: web_sys::ScreenOrientation,
}

/// Read the screen's orientation, or explain why it could not be read.
///
/// Every step can be missing — a browser may expose no `screen`, no
/// `orientation` on it, or no `type` on that — and the API reports each by
/// returning a value rather than by throwing.
#[cfg(target_arch = "wasm32")]
fn screen_orientation() -> Result<ScreenOrientation, JsValue> {
    let screen = web_sys::window()
        .ok_or_else(|| JsValue::from_str("the page has no window"))?
        .screen()?;
    let value = screen.orientation();
    let kind = value.type_()?;
    Ok(ScreenOrientation { kind, value })
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// The `fullscreenchange` listener, kept alive for the page's lifetime.
    ///
    /// Registered once, because fullscreen can be entered and left repeatedly,
    /// and never dropped: the document holds a reference to it either way, and
    /// the app lives as long as the page does.
    static FULLSCREEN_CHANGE: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Watch for the document entering fullscreen, so the orientation can be locked
/// once it is permitted.
///
/// Called once, with the first press. A browser that refuses the listener
/// simply never locks, which is what a platform with no orientation does
/// anyway.
#[cfg(target_arch = "wasm32")]
fn watch_fullscreen_change() {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    FULLSCREEN_CHANGE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let listener = Closure::<dyn FnMut()>::new(lock_landscape_when_upright);
        if let Err(error) = document
            .add_event_listener_with_callback("fullscreenchange", listener.as_ref().unchecked_ref())
        {
            log::debug!("the page cannot watch for fullscreen changes: {error:?}");
            return;
        }
        *slot = Some(listener);
    });
}
