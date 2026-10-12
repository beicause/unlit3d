//! The multi-world demo's library half: the scene the windowed binary
//! shows, and the thread-free headless run the --headless mode and the
//! snapshot tests drive.
//!
//! The binary keeps the windowed mode: the winit plumbing, the GPU
//! request and the MCP transport live there, while everything that
//! defines the scene itself is here, so the headless run steps the very
//! same world graphs the window shows.

pub mod headless;
pub mod scene;
