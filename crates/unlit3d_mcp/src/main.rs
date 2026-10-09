//! The standalone MCP binary: a headless unlit3d world served over stdio.
//!
//! The world lives on a render thread of its own; the transport thread only
//! forwards commands to it, exactly as a windowed host forwards them to its
//! event loop.

use std::process::ExitCode;

use unlit3d_mcp::serve_stdio_offscreen;

fn main() -> ExitCode {
    match serve_stdio_offscreen((960, 720), 4, true) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            stderr(&format!("error: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Write text to stderr, ignoring a closed pipe.
fn stderr(text: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{text}");
}
