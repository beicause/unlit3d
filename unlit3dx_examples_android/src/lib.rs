//! The Android shared library for `unlit3dx_examples`.
//!
//! An activity loads a `.so` and looks one unmangled symbol up in it, so the
//! library has to be a `cdylib`. It is a crate of its own rather than a crate
//! type on the example because `crate-type` applies to every target: the
//! example's own build would then link a shared library on the desktop and in
//! the browser, where nothing loads one. This crate compiles for Android alone.
//!
//! The library keeps the example's name, `libunlit3dx_examples.so`, which is
//! what the activity asks for in its `android.app.lib_name` manifest entry.

#![cfg(target_os = "android")]
#![warn(unsafe_code)]

use winit::platform::android::activity::AndroidApp;

/// Run the example in the activity that loaded the library.
///
/// The activity resolves this symbol by name, through the `extern` block its
/// conversation with the glue declares, so it is exported unmangled and with the
/// `"Rust"` ABI that block names. Everything it does is the example's own
/// entry point, which keeps this crate to the export alone.
#[expect(
    unsafe_code,
    reason = "exporting the entry point to the Android activity requires `#[unsafe(no_mangle)]`"
)]
#[unsafe(no_mangle)]
pub extern "Rust" fn android_main(app: AndroidApp) {
    unlit3dx_examples::android::android_main(app);
}
