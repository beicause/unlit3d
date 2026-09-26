English | [简体中文](README.zh-CN.md)

# xtask

The repository's task runner, behind the `cargo xtask` alias. It wraps the
commands a contributor runs before a commit, so CI and a local run cannot
drift apart, and it builds the example's web and Android artifacts.

This is a development tool for this repository only. It is not a workspace
member — the workspace manifest lists it under `exclude`, and `.cargo/config.toml`
wires it up as `xtask = "run --manifest-path xtask/Cargo.toml --"` — so the task
runner's own dependencies never weigh on the workspace the tasks act on. It is
`publish = false`.

## Tasks

```text
cargo xtask check          # clippy over the whole workspace, then `cargo fmt --check`
cargo xtask test           # nextest over unit and integration tests, then the doctests
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

Runs `cargo nextest run --workspace --all-targets --all-features`, then
`cargo test --workspace --all-features --doc`. Nextest gives every test its own
process, so one test's device, logger or panic cannot reach another's; it does
not run doctests, hence the second pass. The two commands together are what CI
runs. Accepts `--release`.

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

The binary is named explicitly rather than left to the default target
selection: the example crate is a `cdylib` as well as a binary, and on
`wasm32-unknown-unknown` both want to write `unlit3d_examples.wasm`, which cargo
reports as an output filename collision. The binary is what a browser runs.

### `cargo xtask build-android`

Two tools, split along the language boundary. `cargo ndk` cross-compiles the
example for Android and drops the `.so` into the app's `jniLibs` directory,
which is where Gradle looks for native libraries and packages whatever it
finds; Gradle then compiles the activity and assembles the APK around it. The
library is built first because an APK without it is an activity that cannot
start.

Both tools configure themselves from the environment: `cargo ndk` reads the NDK
out of `ANDROID_NDK_HOME` (or the newest one under `ANDROID_HOME/ndk`) and
Gradle reads the SDK out of `ANDROID_HOME` (or `android/local.properties`). A
JDK 17 or newer must be on `PATH`, which is what AGP requires.

One ABI, `arm64-v8a`, and one API level, 26, are compiled, matching
`abiFilters` and `minSdk` in `android/app/build.gradle.kts` — those two files
are the whole contract between the Rust and Gradle halves of the build. The
default build type is debug; `--release` builds a release APK, which is left
unsigned because the project ships no signing configuration.

### `cargo xtask publish`

Publishes the crates that reach crates.io — `unlit_ecs`, `unlit_wgpu` and
`unlit3d` — one at a time and in that order. The order is a requirement rather
than a preference: a crate cannot be packaged until the crates it depends on are
in the registry, so publishing `unlit3d` first fails with `no matching package
named unlit_ecs found`. `cargo publish` waits for each uploaded crate to appear
in the index, which is what makes the next one resolve.

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
src/run_wasm.rs       `cargo xtask run-wasm`: the wasm build, bindgen and page
src/build_android.rs  `cargo xtask build-android`: the cross-build and the APK
src/publish.rs        `cargo xtask publish`: the crates and their publish order
src/http.rs           the static file server `run-wasm` serves with
src/step.rs           running one child command and reporting which step failed
```

The crate denies `missing_docs`, so every item carries a doc comment.

## Building it

```text
cargo run --manifest-path xtask/Cargo.toml -- check
```

Because it is excluded from the workspace, it has its own `Cargo.lock` and its
own `target/` directory. `cargo xtask <task>` from the repository root is the
normal entry point.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
