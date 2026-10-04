//! The unlit3d-cli binary: parse the arguments, render, and write the image.

use std::io::Write;
use std::process::ExitCode;

use unlit3d_cli::cli::{self, Parsed};

fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help(help)) => {
            stdout(&help);
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(args)) => run(args.as_ref()),
        Err(error) => {
            stderr(&format!("error: {error}\n\n{}", cli::usage()));
            ExitCode::from(2)
        }
    }
}

/// Render the parsed arguments and write the image.
fn run(args: &cli::Args) -> ExitCode {
    let config = match args.config() {
        Ok(config) => config,
        Err(error) => {
            stderr(&format!("error: {error}\n\n{}", cli::usage()));
            return ExitCode::from(2);
        }
    };
    let frame = match pollster::block_on(unlit3d_cli::render(&config)) {
        Ok(frame) => frame,
        Err(error) => {
            stderr(&format!("error: {error}\n"));
            return ExitCode::from(1);
        }
    };
    if let Err(error) = unlit3d_cli::save(&args.output, &frame) {
        stderr(&format!("error: {error}\n"));
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// Write text to stdout, ignoring a closed pipe.
fn stdout(text: &str) {
    let _ = std::io::stdout().write_all(text.as_bytes());
}

/// Write text to stderr, ignoring a closed pipe.
fn stderr(text: &str) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}
