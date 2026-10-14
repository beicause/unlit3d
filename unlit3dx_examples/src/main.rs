//! The multi-world example's command line: everything it runs is in the
//! library, so the same program is what a browser loads and what the
//! Android activity calls.

fn main() -> std::process::ExitCode {
    unlit3dx_examples::run()
}
