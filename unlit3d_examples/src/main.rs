//! The command-line entry point.
//!
//! Everything the example does lives in the library, because the Android build
//! has no process to start from and enters through its own `android_main`
//! instead. This binary is that library's [`run`](unlit3d_examples::run).

fn main() -> std::process::ExitCode {
    unlit3d_examples::run()
}
