English | [简体中文](README.zh-CN.md)

# wgpu_unlit_render

A compact, opinionated renderer for **unlit** draws on WebGPU. It draws a whole
scene — opaque and transparent instances alike — in a single render pass into
one `wgpu::TextureView` chosen by the caller, and targets WebGPU (and the
native backends behind it) with a mobile-first bias. WebGL and GLES are not
supported.

The crate is layered. The general modules — resource tracking, vertex
compression, buffer pools, staging, the declarative scene, the variant cache —
know nothing about the built-in pipeline, and the unlit pipeline is composed
from them exactly as a caller's own pipeline would be.

This crate is at an **early stage of development**; APIs change freely. It has
no dependency on the ECS layer.

## Role in the workspace

`wgpu_unlit_render` is the foundation of the workspace and depends on nothing
else in it. The ECS-integrated API is
[`unlit3d`](../unlit3d/README.md), which builds components, frame sources and
winit presentation on top of this crate. The GPU test harness the tests draw on
is [`wgpu_unlit_test_util`](../wgpu_unlit_test_util/README.md).

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `unlit` | yes | `pipeline::UnlitPipeline`, `pipeline::UnlitOptions`, the `UnlitFlags` variant bits, and the WESL module they compose |
| `egui` | no | the `ui` module, an egui backend that draws tessellated egui output as ordinary screen-space draws. Implies `unlit` |

With `--no-default-features` the crate keeps its general facilities — the
resource graph, mesh compression, the offset allocator and buffer pools,
staging, `Scene`, `RenderAttachments`, the variant cache, and the WESL modules
mirroring the types a caller binds — and drops everything specific to the
built-in pipeline, including the WESL compiler it composes shaders with. CI
checks that combination separately.

## What is in the box

- `resources` — a dependency-tracked graph of the GPU resources a frame uses.
  Replacing a resource marks everything transitively built from it dirty;
  removing one drops its dependents; virtual nodes own no handle and serve as
  aggregation roots.
- `mesh` — vertex compression (`Snorm16x4` positions, `Snorm16x2` UVs,
  `Unorm8x4` colors, `Uint16x4` joints, `Unorm16x4` weights) and the
  `MeshMetadata` decode parameters, plus a vertex-stream writer that packs
  channels without materializing them.
- `offset_allocator` — an O(1), allocation-free sub-allocator over one
  contiguous range, using the two-level segregated fit from Aaltonen's
  `OffsetAllocator`.
- `buffer_pool` and `vertex_pool` — GPU buffers sub-allocated with it, so many
  meshes share one buffer per kind or per vertex layout.
- `staging` — host-visible staging buffers reused across frames instead of a
  fresh `queue.write_buffer` allocation per upload.
- `scene` — the declarative description of a frame: pipelines, their bind
  groups, materials, meshes, vertex buffers and draw ranges.
- `render_attachments` — the attachments a pass renders into, the pass-opening
  entry point, and `create_render_target` for an offscreen frame.
- `specialize` — variant caching: a `Specializable` value is compiled once per
  key and reused, with a canonical map for keys that are not injective.
- `pipeline` — the binding slots, bind-group indices and vertex-buffer slots
  the crate draws with, plus the built-in unlit pipeline under the `unlit`
  feature.
- `ui` — the egui backend, under the `egui` feature.

## Usage

The crate draws by recording a `Scene`; the built-in pipeline is one way to
fill one. Abbreviated, with the device, bind groups and buffers already built:

```rust,no_run
use wgpu_unlit_render::pipeline::{GLOBAL_GROUP, POSITION_SLOT, UnlitOptions, UnlitPipeline};
use wgpu_unlit_render::render_attachments::{RenderAttachments, color_clear, depth_clear, stencil_clear};
use wgpu_unlit_render::scene::{DrawEntry, DrawRange, Scene};

// A pipeline is only valid for the target its options describe.
let options = UnlitOptions::standard(&device);
let pipeline = UnlitPipeline::new(&device, &options);

let draw = DrawEntry::new(&pipeline.pipeline, DrawRange::indexed(0..index_count))
    .with_bind_group(GLOBAL_GROUP, &global_bind_group)
    .with_vertex_buffer(POSITION_SLOT, &position_buffer)
    .with_index_buffer(&index_buffer, wgpu::IndexFormat::Uint16);
let scene = Scene::new().with_draw(draw);

let attachments = RenderAttachments::from_views(Some(color), Some(depth), None);
let mut encoder = device.create_command_encoder(&Default::default());
{
    let mut pass = attachments.begin_pass(&mut encoder, color_clear(), depth_clear(), stencil_clear());
    scene.record(&mut pass);
}
queue.submit([encoder.finish()]);
```

The crate's own documentation carries a full, compiling walkthrough of the
built-in pipeline — device setup, mesh compression, every bind group and the
draw — as the `unlit` module example. The doc comments of `mesh`,
`buffer_pool`, `offset_allocator`, `staging` and `render_attachments` also
contain runnable examples.

To write your own shader, compose it with
[`wesl`](https://docs.rs/wesl): the crate's `shader` item is a WESL
`StaticPackage`, so a resolver can import the same modules the built-in
pipeline uses.

## Tests

```text
cargo xtask test     # the whole workspace, through nextest plus the doctests
cargo nextest run -p wgpu_unlit_render   # just this crate
```

The GPU integration tests render meshes into offscreen textures, read them
back and compare them against the snapshots under `tests/snapshots` with the
SSIMULACRA2 perceptual metric. That directory is a symlink into the
[`wgpu_unlit_render_asset_files`](../../wgpu_unlit_render_asset_files/README.md)
submodule; clone it with `git submodule update --init`. To re-bless a snapshot
after an intentional change, run the test with `SNAPSHOT_UPDATE=1` set, then
review the image diff before committing it.

## Documentation

- [`docs/DESIGN.md`](../../docs/DESIGN.md) — the design rationale and architecture
  (in Chinese).
- Crate-level docs: `cargo doc -p wgpu_unlit_render --open`.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
