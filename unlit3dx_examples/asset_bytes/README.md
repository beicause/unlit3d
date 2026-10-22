English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/asset_bytes/README.zh-CN.md)

# unlit3dx_asset_bytes

The soundfont [`unlit3dx_examples`](../README.md) plays, carried inside the
binary so the demo needs no file at run time and the same bytes reach a window,
a browser and an Android activity.

The bytes are a crate of their own for one reason: compile time.

## Why the soundfont is not in the demo

`include_bytes!` is not free for whoever reads it. A `pub const` or a `static`
holding the soundfont copies fourteen megabytes into the metadata of the crate
that declares it, and every crate that compiles against that crate reads that
metadata back — the demo's own rebuild included. Measured on the demo before
the split, the artifacts went from 0.5 MB of metadata and a 14 MB rlib without
the soundfont to 57 MB and 100 MB with it. Declared here instead, the metadata
stays at kilobytes.

A function that calls `include_bytes!` in its own body hands out a
`&'static [u8]` instead of the bytes themselves. That is why the soundfont is
a function and never a constant — but a function alone is not enough. An
optimised profile (`opt-level` 1 and up, which this workspace builds with)
serialises the body so a caller in another crate can inline it, and the body
carries what it embeds: measured here, `soundfont` without
`#[inline(never)]` still wrote 14 MB of metadata. With the attribute the body
stays out of the metadata and it falls to 4.8 KB. Every function in this crate
therefore carries `#[inline(never)]`, and the demo reads a 14 MB rlib that
nothing parses.

## Usage

```rust,ignore
let soundfont: Arc<[u8]> = unlit3dx_asset_bytes::soundfont().into();
let voice = audio.ask(move |world| install_synth(world, &soundfont));
```

## The missing soundfont

`assets/FluidR3Mono_GM.sf3` is gitignored, so a fresh clone has none. `build.rs`
resolves it next to the workspace root and exports its path as
`SOUNDFONT_FILE`; when the file is absent it writes an empty stub instead and
the demo starts up silent rather than failing to compile.

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
