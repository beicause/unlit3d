English | [简体中文](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/asset_bytes/README.zh-CN.md)

# unlit3d_asset_bytes

The glTF documents [`unlit3d_examples`](../README.md)' `gltf` scene draws — a
skinned fox and a morph stress test — carried inside the binary so the scene
needs no file at run time. Both come from the
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
submodule.

The bytes are a crate of their own for one reason: compile time.

## Why the assets are not in the example

`include_bytes!` is not free for whoever reads it. A `pub const` or a `static`
holding a document copies the bytes into the metadata of the crate that
declares it, and every crate that compiles against that crate reads that
metadata back — the example's own rebuild included. Declared inside
`unlit3d_examples`, a megabyte of documents is a megabyte of metadata that every
incremental rebuild of every crate downstream has to parse again.

A function that calls `include_bytes!` in its own body hands out a
`&'static [u8]` instead of the bytes themselves. That is why every asset here
is a function and never a constant — but a function alone is not enough. An
optimised profile (`opt-level` 1 and up, which this workspace builds with)
serialises the body so a caller in another crate can inline it, and the body
carries what it embeds: measured here, `fox` without `#[inline(never)]` still
wrote 0.6 MB of metadata, one copy per asset. With the attribute they stay out
of the metadata and it falls to 5 KB. Every function in this crate therefore
carries `#[inline(never)]`, and the crate can grow without slowing down
anything that reads it.

## Usage

```rust,ignore
let fox = unlit3d_asset_bytes::fox();
let document = LitGltf::from_bytes(fox).expect("the fox asset is a valid glTF document");
```

| Function | Asset |
|----------|-------|
| `fox` | `Fox.glb`, the skinned fox the `gltf` scene walks |
| `morph_stress_test` | `MorphStressTest.glb`, the cube whose eight morph targets pulse |

## License

Dual-licensed under either of MIT or Apache-2.0, at your option, as declared in
the workspace manifest.
