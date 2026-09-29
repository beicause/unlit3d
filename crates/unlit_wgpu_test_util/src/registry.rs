//! The registry that lets one test body run under `libtest-mimic` natively and
//! in a browser through wasm.
//!
//! [`gpu_tests!`](crate::gpu_tests) collects a file's tests into the registry
//! the harness runs, and [`gpu_test_main!`](crate::gpu_test_main) supplies the
//! `main` that drives it. One source file serves both runners, because a body
//! is an ordinary `async fn` and both runners reach it through the same
//! `all_tests()`.
//!
//! A body is `async` even when it awaits nothing in particular: the device it
//! asks [`Ctx::headless`](crate::Ctx::headless) for comes from wgpu's
//! asynchronous adapter and device requests, which a browser cannot satisfy
//! without yielding.

use core::future::Future;
use core::pin::Pin;

/// A test's future, boxed so bodies of different types share one registry.
pub type TestBody = Pin<Box<dyn Future<Output = ()>>>;

/// Builds one test's future. Called once per run.
pub type TestFn = fn() -> TestBody;

/// One test the harness knows how to run.
#[derive(Clone, Copy)]
pub struct TestEntry {
    /// The module the test was declared in, as `module_path!` reports it.
    ///
    /// Only for diagnostics: the two runners name a test by [`Self::name`],
    /// because a browser is handed one binary that reuses several test files
    /// and would otherwise qualify the same test differently.
    pub module: &'static str,
    /// The test's name, which is unique across the whole suite.
    pub name: &'static str,
    /// Builds the future the harness drives.
    pub body: TestFn,
    /// The message a panic is expected to carry, for a `#[should_panic]` test.
    ///
    /// Both runners compare a caught panic against this. The browser needs it
    /// before the test runs: a panic aborts a wasm instance instead of
    /// unwinding, so the panic hook has to decide the verdict itself.
    pub expected_panic: Option<&'static str>,
}

impl TestEntry {
    /// The test's name qualified by its module.
    pub fn full_name(&self) -> String {
        format!("{}::{}", self.module, self.name)
    }
}

/// Declares a file's GPU tests, and the registry the harness runs them from.
///
/// The bodies are ordinary `async fn`s declared in the file; this macro lists
/// them so the harness can reach them by name. A name that has no body, or one
/// that is not an `async fn`, is a compile error, so the list cannot drift out
/// of step with the file.
///
/// A test that must panic carries `#[should_panic(expected = "...")]` in the
/// list rather than on its body, because the attribute is what the runner
/// compares a caught panic against and the body is not itself a test item.
///
/// # Example
///
/// ```
/// use unlit_wgpu_test_util::gpu_tests;
///
/// async fn draws_a_triangle() {
///     assert_eq!(1 + 1, 2);
/// }
///
/// async fn rejects_an_empty_mesh() {
///     panic!("rejected: a mesh needs at least one vertex");
/// }
///
/// gpu_tests! {
///     draws_a_triangle,
///     #[should_panic(expected = "rejected:")]
///     rejects_an_empty_mesh,
/// }
///
/// assert_eq!(all_tests().len(), 2);
/// assert_eq!(all_tests()[1].expected_panic, Some("rejected:"));
/// ```
#[macro_export]
macro_rules! gpu_tests {
    // The terminator is matched before the test arms, because `@acc [...]` with
    // nothing left to consume is more specific than either of them.
    (@acc [$($acc:tt)*]) => {
        /// Every test this file declares.
        pub fn all_tests() -> ::std::vec::Vec<$crate::TestEntry> {
            ::std::vec![$($acc)*]
        }
    };

    (@acc [$($acc:tt)*]
     #[should_panic(expected = $msg:literal)]
     $name:ident,
     $($rest:tt)*) => {
        $crate::gpu_tests!(@acc
            [$($acc)* $crate::TestEntry {
                module: module_path!(),
                name: stringify!($name),
                body: (|| ::std::boxed::Box::pin($name())) as $crate::TestFn,
                expected_panic: ::core::option::Option::Some($msg),
            },]
            $($rest)*);
    };

    (@acc [$($acc:tt)*] $name:ident, $($rest:tt)*) => {
        $crate::gpu_tests!(@acc
            [$($acc)* $crate::TestEntry {
                module: module_path!(),
                name: stringify!($name),
                body: (|| ::std::boxed::Box::pin($name())) as $crate::TestFn,
                expected_panic: ::core::option::Option::None,
            },]
            $($rest)*);
    };

    // The catch-all entry arm has to come last: it matches anything, including
    // the `@acc` invocations the arms above make, which would recurse forever.
    ($($rest:tt)*) => { $crate::gpu_tests!(@acc [] $($rest)*); };
}

/// Provides the entry point a test binary drives its registry through.
///
/// `$tests` is an expression evaluating to `Vec<TestEntry>`. The same file
/// therefore serves both runners: natively this is the test binary's `main`,
/// and in a wasm build it is the export the page calls.
///
/// A native run with [`WASM_TEST_ENV`](crate::WASM_TEST_ENV) set becomes the
/// browser's proxy rather than a runner, so a test binary compiled for the host
/// still lists and reports every test while the bodies run in a page.
///
/// The caller needs `wasm-bindgen` in scope, because the attribute below
/// expands to paths naming that crate. It is a wasm-only dependency of the
/// test crates, so nothing pays for it on a platform that has no wasm.
#[macro_export]
macro_rules! gpu_test_main {
    ($tests:expr) => {
        /// Run the registered test named `name`, as the test page asks.
        ///
        /// This is the wasm build's entry point: a browser has no test binary
        /// to start, so the page calls this instead and reads the verdict back
        /// out of `sessionStorage`.
        #[cfg(target_arch = "wasm32")]
        #[wasm_bindgen::prelude::wasm_bindgen]
        pub fn run_test(name: String) {
            $crate::browser::run($tests, name);
        }

        // A test target is a binary on every target, so wasm needs a `main`
        // too. The registry is reached through the export above instead.
        #[cfg(target_arch = "wasm32")]
        fn main() {
            let _ = $tests;
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn main() {
            // Set by `cargo xtask test-wasm` for the host build, which then
            // drives the browser instead of running the tests in-process. The
            // module name is a compile-time constant rather than part of that
            // variable, since each test binary loads its own module and one
            // value shared by the whole `cargo nextest run` could not say which.
            if ::std::env::var_os($crate::WASM_TEST_ENV).is_some() {
                $crate::run_wasm($tests, env!("CARGO_CRATE_NAME"));
            } else {
                $crate::run($tests);
            }
        }
    };
}
