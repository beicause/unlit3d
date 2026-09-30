//! Presentation the browser can offer this example: the button that makes the
//! canvas fullscreen, and the landscape lock that only fullscreen permits.
//!
//! The example's content has a fixed aspect, so it reads best on a display of
//! roughly that shape. A phone held upright shows it as a band across the
//! middle of the screen, which is the case this module exists for: the button
//! asks the browser to make the canvas fullscreen, and a device being held
//! upright is then locked to landscape so the picture fills the display.
//!
//! Only the browser has a page to make fullscreen or a screen to lock, so
//! everywhere else the capability is reported as absent and the frame loop
//! draws no button. The click on that button is the gesture a browser accepts
//! as permission to go fullscreen, and it stays available after the user leaves
//! fullscreen, so the app can be put back into it as often as they ask.

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

/// Whether the page can take the canvas fullscreen.
///
/// The browser reports this for the document as a whole: it is false for a page
/// that never had the API, and for one in an `iframe` without the `fullscreen`
/// permission. No button is drawn when it is false, so a phone that cannot go
/// fullscreen is not shown a control that could only fail.
#[must_use]
pub fn supported() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return false;
        };
        document.fullscreen_enabled()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Whether the document is fullscreen at this moment.
///
/// The button reads this every frame and labels itself for the action it would
/// take. That matters most on a phone, where leaving fullscreen is otherwise
/// only possible through the browser's own chrome, which a page cannot reach.
#[must_use]
pub fn active() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return false;
        };
        document.fullscreen()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Put `window` into or out of fullscreen, whichever the document is not.
///
/// A browser grants this only to a page with a *transient activation*, and the
/// click on the button is what provides one. The request leaves that click's
/// own DOM handler — the panel records it and the frame loop reads it a frame
/// or two later — but an activation outlives that by seconds, so the call is
/// still made from inside the window the gesture opened.
///
/// A refusal is silent: winit neither reports nor logs one, so a browser that
/// turns the request down simply leaves the app windowed. Nothing is asked for
/// while already fullscreen except to leave it, so the button cannot trap a
/// user in a display they did not choose.
pub fn toggle(window: &Window) {
    #[cfg(target_arch = "wasm32")]
    {
        // Armed here rather than at start-up because the event it waits for is
        // the one the request below is about to cause. Registering is
        // idempotent, so every press after the first only flips the state.
        watch_fullscreen_change();
        let fullscreen = match window.fullscreen() {
            Some(_) => None,
            None => Some(Fullscreen::Borderless(None)),
        };
        window.set_fullscreen(fullscreen);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = window;
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
/// Called before the first fullscreen request, which is the event it waits for.
/// A browser that refuses the listener simply never locks, which is what a
/// platform with no orientation does anyway.
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
