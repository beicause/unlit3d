//! The standalone unlit3d MCP server: serve the protocol over stdio against a
//! fresh offscreen world.

use std::io::Write;
use std::process::ExitCode;

use unlit3d_mcp::{Host, serve_stdio};

fn main() -> ExitCode {
    let served = serve_stdio(|| pollster::block_on(Host::new_offscreen((960, 720), 4, true)));
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            stderr(&format!("error: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Write text to stderr, ignoring a closed pipe.
fn stderr(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}
