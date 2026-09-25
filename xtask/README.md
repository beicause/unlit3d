English | [简体中文](README.zh-CN.md)

# xtask

The repository's task runner, behind the `cargo xtask` alias. It wraps the
commands a contributor runs before a commit, so CI and a local run cannot
drift apart, and it builds and serves the web example.

This is a development tool for this repository only. It is not a workspace
member — the workspace manifest lists it under `exclude`, and `.cargo/config.toml`
wires it up as `xtask = "run --manifest-path xtask/Cargo.toml --"` — so the task
runner's own dependencies never weigh on the workspace the tasks act on. It is
`publish = false`.

## Tasks

```text
cargo xtask check      # clippy over the whole workspace, then `cargo fmt --check`
cargo xtask test       # nextest over unit and integration tests, then the doctests
cargo xtask run-wasm   # build the web example and serve it for a browser
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

## Layout

```text
src/main.rs      argument parsing (argh) and task dispatch
src/check.rs     `cargo xtask check`
src/test.rs      `cargo xtask test`
src/run_wasm.rs  `cargo xtask run-wasm`: the wasm build, bindgen and page
src/http.rs      the static file server `run-wasm` serves with
src/step.rs      running one child command and reporting which step failed
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
