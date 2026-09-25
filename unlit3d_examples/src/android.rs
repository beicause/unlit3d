//! The Android entry point.
//!
//! Android starts an `Activity` rather than a process. The activity loads the
//! shared library and calls [`android_main`] on a thread of its own, handing it
//! the activity; there is no command line to parse and no `main`, so this is
//! the one entry point that is not [`crate::run`]. Once the event loop is
//! built, the windowed loop it is handed to is the same one the binary drives.

use winit::event_loop::EventLoop;
use winit::platform::android::EventLoopBuilderExtAndroid;
use winit::platform::android::activity::AndroidApp;

/// Run the example in the activity that loaded the library.
///
/// Called once per activity the process creates, so a loop torn down when one
/// is destroyed is rebuilt for the next rather than resumed. The symbol is
/// exported unmangled — a `"Rust"` ABI function — because that is the name the
/// activity's glue looks up; a panic here is caught and logged by the glue
/// instead of reaching the activity.
#[unsafe(no_mangle)]
pub extern "Rust" fn android_main(app: AndroidApp) {
    crate::init_logging();

    // The example's own defaults: an activity is started without arguments,
    // and the window size it asks for is the display's to overrule.
    let event_loop = EventLoop::<crate::UserEvent>::with_user_event()
        .with_android_app(app)
        .build()
        .expect("an event loop");
    crate::windowed(crate::cli::Args::default(), event_loop);
}
