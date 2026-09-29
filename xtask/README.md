English | [简体中文](https://github.com/beicause/unlit3d/blob/main/xtask/README.zh-CN.md)

# xtask

The repository's task runner, behind the `cargo xtask` alias. It wraps the
commands a contributor runs before a commit, so CI and a local run cannot drift
apart, and it builds the example's web and Android artifacts.

This is a development tool for this repository only, and `publish = false`. It
is a workspace member, so `cargo clippy --workspace` and `cargo fmt --all`
cover it along with the crates it acts on; `.cargo/config.toml` wires it up as
`xtask = "run -p xtask --"`.

## Tasks

```text
cargo xtask check          # clippy over the whole workspace, then `cargo fmt --check`
cargo xtask test           # nextest over unit and integration tests, then the doctests
cargo xtask test-wasm      # run the GPU tests in a browser, through wasm and WebGL2
cargo xtask run-wasm       # build the web example and serve it for a browser
cargo xtask build-android  # build the Android library and the APK around it
cargo xtask publish        # publish the workspace's crates to crates.io, in dependency order
```

### `cargo xtask check`

Runs `cargo clippy --workspace --all-targets --all-features` and then
`cargo fmt --all -- --check`. Clippy goes first because its diagnostics may
leave the tree in a state `rustfmt` would rewrite, so formatting is the last
word. Accepts `--release`.

### `cargo xtask test`

Runs `cargo nextest run --workspace --lib --bins --tests --all-features`, then
`cargo test --workspace --all-features --doc`. Nextest gives every test its own
process, so one test's device, logger or panic cannot reach another's; it does
not run doctests, hence the second pass. The targets are named rather than swept
in with `--all-targets` because the benchmarks are not test binaries.

The nextest pass runs twice: once with the default tier — the WebGPU baseline's
limits — and once with `UNLIT3D_DEVICE_TIER=webgl2`, which narrows every headless
device to WebGL2's limits and missing downlevel capabilities. The second pass is
what exercises the paths a browser takes — the shader's arrays read through
textures instead of storage buffers, and a mesh's vertex offset baked into its
indices — on a machine whose own adapter is Vulkan, Metal or DX12. Set the
variable to `native` to run against the adapter's own limits instead of the
baseline. Accepts `--release`, which applies to both passes.

A frame that fails its snapshot comparison is written before the test fails, under
`target/snapshot-mismatches` for the default pass and
`target/snapshot-mismatches-webgl2` for the tier pass — a directory each, so a
frame that differs under both tiers can be told apart. CI uploads them when a
test fails, so a mismatch can be looked at rather than only scored.

### `cargo xtask test-wasm`

Runs the same GPU tests `cargo xtask test` does, but on
`wasm32-unknown-unknown` in a real browser, and then starts the example itself. A
wasm binary has no process to start and no exit code to hand back, so the pass is
assembled rather than run:

1. The workspace is built for wasm — under the default features, which is what
   the browser and the APK ship — so a change that breaks the example or the
   library for this target fails here rather than only in its own job.
2. `cargo nextest list --list-type binaries-only` builds the wasm test binaries
   and reports where they landed — asked of nextest so the target list, features
   and names stay the ones the host pass uses.
3. `wasm-bindgen` turns each into a JS module exporting `run_test`.
4. The test page, which loads one module and calls that export, is copied beside
   them, together with a `wasm_paths.json` mapping a module name to its script.
5. A Node runner serves that directory and opens the page once per test in a
   fresh browser context. A frame that fails its comparison is encoded in the
   browser — the page has no filesystem — and handed to this runner, which
   writes it under `target/snapshot-mismatches-wasm` for CI to upload.
6. The *same* native test binaries, with `UNLIT3D_WASM_TEST` set, become a proxy
   instead of a test suite: `cargo nextest` drives them, and each trial asks the
   runner to run that test in the browser. Nextest's listing, filtering,
   reporting and exit code then all work as they do for an ordinary suite.

The verdict travels through `sessionStorage`, because a browser has no exit code
and a panicking test traps its wasm instance rather than unwinding — which is
also why `#[should_panic]` is judged by a panic hook rather than by catching the
panic.

Before the tests, the runner also loads the example — the program a browser
actually runs, which the GPU tests never start — and checks that it boots, finds
a backend and leaves a shaded image on its canvas rather than a blank one. It is
the same page `run-wasm` serves, built by the same code, so the two cannot drift.

The runner's JS dependencies and a browser are installed on the first run;
`CHROME_PATH` names an existing browser to reuse instead of downloading one, and
`--show` opens a visible window to watch a failure happen. The profile the
nextest pass uses is `wasm`, defined in `.config/nextest.toml`.

### `cargo xtask run-wasm`

The standard pipeline for a `wasm32-unknown-unknown` binary a browser runs:
build the example for the target, hand the wasm to `wasm-bindgen` so it gets a
JS loader, then serve the output behind a built-in static file server — WebGPU
is only available in a *secure context*, and `localhost` is one while a
`file://` URL is not. The server is built into the task runner, so nothing needs
to be installed.

It binds to loopback on port 8000, or the ports after it if that one is taken,
and prints the URL to open. `--no-serve` builds and runs bindgen without
serving; `--release` builds in release mode; trailing positional arguments are
passed through to the `cargo build`.

The binary is named explicitly rather than left to the default target selection:
the example crate is a `cdylib` as well as a binary, and on
`wasm32-unknown-unknown` both want to write `unlit3d_examples.wasm`, which cargo
reports as an output filename collision. The binary is what a browser runs.

### `cargo xtask build-android`

Two tools, split along the language boundary. `cargo ndk` cross-compiles the
example for Android and drops the `.so` into the app's `jniLibs` directory,
which is where Gradle looks for native libraries and packages whatever it finds;
Gradle then compiles the activity and assembles the APK around it. The library
is built first because an APK without it is an activity that cannot start.

Both tools configure themselves from the environment: `cargo ndk` reads the NDK
out of `ANDROID_NDK_HOME` (or the newest one under `ANDROID_HOME/ndk`) and
Gradle reads the SDK out of `ANDROID_HOME` (or `android/local.properties`). A
JDK 17 or newer must be on `PATH`, which is what AGP requires.

One ABI, `arm64-v8a`, and one API level, 26, are compiled, matching `abiFilters`
and `minSdk` in `android/app/build.gradle.kts` — those two files are the whole
contract between the Rust and Gradle halves of the build. The default build type
is debug; `--release` builds a release APK, which is left unsigned because the
project ships no signing configuration.

### `cargo xtask publish`

Publishes the crates that reach crates.io — `unlit_ecs`, `unlit_wgpu` and
`unlit3d` — one at a time and in that order. The order is a requirement rather
than a preference: a crate cannot be packaged until the crates it depends on are
in the registry, so publishing `unlit3d` first fails with
`no matching package named unlit_ecs found`. `cargo publish` waits for each
uploaded crate to appear in the index, which is what makes the next one resolve.

`cargo publish --workspace` would order the crates itself, but its `--dry-run`
cannot verify the packages (rust-lang/cargo#16525); publishing crate by crate
keeps the verification pass a release is worth. `--dry-run` runs every check
without uploading.

The test harness, the example and the task runner are `publish = false`, so they
are never uploaded. A real publish is not repeatable — cargo refuses a version
that is already on the registry — so a run that stops partway must be resumed at
the crate that failed.

## Layout

```text
src/main.rs           argument parsing (argh) and task dispatch
src/check.rs          `cargo xtask check`
src/test.rs           `cargo xtask test`
src/test_wasm.rs      `cargo xtask test-wasm`: the browser pass and its runner
src/run_wasm.rs       `cargo xtask run-wasm`: the wasm build, bindgen and page
src/build_android.rs  `cargo xtask build-android`: the cross-build and the APK
src/publish.rs        `cargo xtask publish`: the crates and their publish order
src/http.rs           the static file server `run-wasm` serves with
src/step.rs           running one child command and reporting which step failed
```

## Building it

```text
cargo run -p xtask -- check
```

Being a workspace member, it shares the workspace's `Cargo.lock` and `target/`.
`cargo xtask <task>` from anywhere in the repository is the normal entry point.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
