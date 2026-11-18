//! The multi-world demo: one scene where rendering, physics and audio each
//! run in their own world, joined by jobs and snapshots.
//!
//! The scene is the application; the run is the skeleton. The [`demo::Demo`]
//! struct implements [`unlit3dx::app::App`], and [`run`] hands it to
//! [`unlit3dx::app::run`], which reads the command line and picks the window,
//! the headless capture or the MCP host. The same call is what the browser
//! page and the Android activity reach, so a scene change lands everywhere
//! at once.
//!
//! The control panel drives two music tracks — a looping background piece and
//! a one-shot MIDI sequence — each of which pauses, resumes and rewinds on its
//! own, and it drops the next ball or puts every live ball away. Space drops
//! one too, on the keyboard, from inside the world that holds them. Bodies
//! fall, bounce off the ground and each other while the music plays. The
//! soundfont is embedded at build time: `build.rs` reads the
//! `FluidR3Mono_GM.sf3` file the repository keeps out of version control under
//! `assets`, and the demo runs silent when it is missing.

#![forbid(unsafe_code)]

pub mod demo;
pub mod render;
pub mod scene;

#[cfg(target_os = "android")]
pub mod android;

use std::process::ExitCode;

/// Run the example the binary was started as.
///
/// The skeleton reads the command line, picks a run, and drives it. The demo
/// scene is the only thing this crate owns.
pub fn run() -> ExitCode {
    unlit3dx::app::run(demo::Demo::default())
}
