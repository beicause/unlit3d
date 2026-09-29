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
/// Called once per activity the process creates. Recreating an activity — a
/// configuration change the manifest did not absorb, or the process being
/// rebuilt — starts a call of its own, which is why this builds a fresh loop
/// rather than reusing one. Suspending and resuming the activity is *not* a
/// second call: the loop stays alive and the app keeps its state across it.
///
/// The symbol is exported unmangled — through the `"Rust"` ABI the glue's
/// `extern` block declares — because that is the name the activity's glue looks
/// the function up by; a panic here is caught and logged by the glue instead of
/// reaching the activity. The export itself is the one piece of unsafe code in
/// the crate: `#[unsafe(no_mangle)]` asserts that no other library exports the
/// name.
#[expect(
    unsafe_code,
    reason = "exporting the entry point to the Android activity requires `#[unsafe(no_mangle)]`"
)]
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
