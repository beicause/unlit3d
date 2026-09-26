English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md)

# unlit_wgpu

A compact, opinionated renderer for unlit draws on WebGPU. It draws a whole
scene — opaque and transparent instances alike — in a single render pass into
one [`wgpu::TextureView`] chosen by the caller, and targets WebGPU (and the
native backends behind it) with a mobile-first bias. WebGL and GLES are not
supported.

The crate is layered: the modules below are general facilities a caller builds
any pipeline on top of, and the built-in unlit pipeline — and the egui backend
that draws with it — are added by features. Nothing in the general modules knows
about the built-in pipeline: it uses the same binding and slot conventions, the
same vertex compression and the same resource tracking a caller's own pipeline
would.

It is at an **early stage of development**; APIs change freely. It has no
dependency on the ECS layer; the ECS-integrated API built on it is
[`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md).

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `unlit` | yes | [`pipeline`] — `UnlitPipeline`, `UnlitOptions`, the `UnlitFlags` variant bits, and the WESL module they compose |
| `egui` | no | the `ui` module: an egui backend that draws tessellated egui output as ordinary screen-space draws. Implies `unlit` |

With `--no-default-features` the crate keeps its general facilities — the
resource graph, mesh compression, the offset allocator and buffer pools,
staging, `Scene`, `RenderAttachments`, the variant cache, and the WESL modules
mirroring the types a caller binds — and drops everything specific to the
built-in pipeline, including the WESL compiler it composes shaders with. CI
checks that combination separately.

## What is in the box

- [`resources`] — a dependency-tracked graph of the GPU resources a frame uses.
  Resources are created, replaced and removed through it, and the graph
  propagates "dirty" state to dependents so that derived resources (bind groups,
  pipelines) are rebuilt lazily. Replacing a resource marks everything
  transitively built from it dirty, removing one drops its dependents, and
  virtual nodes own no handle and serve as aggregation roots.
- [`mesh`] — vertex compression (`Snorm16x4` positions, `Snorm16x2` UVs,
  `Unorm8x4` colors, `Uint16x4` joints, `Unorm16x4` weights) and the
  [`MeshMetadata`](mesh::MeshMetadata) decode parameters that make the compact formats usable in a
  shader, plus a vertex-stream writer that packs channels without materializing
  them.
- [`offset_allocator`] — an O(1), allocation-free sub-allocator over one
  contiguous range, using the two-level segregated fit from Aaltonen's
  `OffsetAllocator`.
- [`buffer_pool`] and [`vertex_pool`] — GPU buffers sub-allocated with it, so
  many meshes share one buffer per kind or per vertex layout instead of each
  owning its own.
- [`staging`] — host-visible staging buffers reused across frames instead of a
  fresh `queue.write_buffer` allocation per upload.
- [`scene`] — the declarative description of a frame: pipelines, their bind
  groups, materials, meshes, vertex buffers and draw ranges.
- [`render_attachments`] — the attachments a pass renders into, the pass-opening
  entry point, and [`create_render_target`](render_attachments::create_render_target) for an offscreen frame.
- [`specialize`] — variant caching: a [`Specializable`](specialize::Specializable)
  value is compiled once per key and reused, with a canonical map for keys that
  are not injective.
- [`pipeline`] — the binding slots, bind-group indices and vertex-buffer slots
  the crate draws with, plus the built-in unlit pipeline under the `unlit`
  feature.
- `ui` — the egui backend, under the `egui` feature.

## Example

Everything one scene needs is built once and reused every frame; `Example::draw`
is what runs in the frame loop. The walkthrough below is complete: device setup,
mesh compression, every bind group and the draw.

```rust
# #[cfg(feature = "unlit")]
# {
use unlit_wgpu::globals::{Globals, View};
use unlit_wgpu::mesh::{
    MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP, MESH_INFO_BINDING,
    MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT, UnlitOptions, UnlitPipeline,
};
use unlit_wgpu::render_attachments::{
    color_clear, create_render_target, depth_clear, stencil_clear,
};
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use zerocopy::IntoBytes;

/// Everything one scene needs, built once and reused every frame.
struct Example {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: UnlitPipeline,
    globals: wgpu::BindGroup,
    material: wgpu::BindGroup,
    mesh: wgpu::BindGroup,
    positions: wgpu::Buffer,
    uv_color: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    instances: wgpu::Buffer,
    instance_count: u32,
}

impl Example {
    /// Build the pipeline, the geometry and every bind group.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Example {
        // 1. Pick the shader variant. Each flag adds both a shader code path
        //    and the matching vertex attributes / bindings. The pipeline is
        //    built for `standard`'s target: an `Rgba8UnormSrgb` color format
        //    with 4x MSAA, which is what the render target uses below.
        let options = UnlitOptions::standard(device);
        let pipeline = UnlitPipeline::new(device, &options);

        // 2. Compress the mesh. Positions and UVs become 16-bit normalized
        //    integers relative to a bounding box; the decode parameters go
        //    into `metadata`.
        let (positions, uvs, colors, indices) = cube();
        let mut metadata = MeshMetadata::default();
        // The compressors produce lazy iterators, so nothing is allocated.
        let packed_positions: Vec<_> = compress_positions(&positions, &mut metadata).collect();
        let packed_indices: Vec<u16> = compress_indices(&indices).unwrap().collect();

        // Static geometry goes straight into mapped-at-creation buffers: the
        // bytes are written into the buffer's own memory, so there is no
        // staging copy and no `COPY_DST` usage to declare. Data that changes
        // every frame wants a `COPY_DST` buffer instead, uploaded through a
        // `StagingBuffer`, which reuses one staging buffer across frames.
        let upload = |bytes: &[u8], usage: wgpu::BufferUsages, label: &str| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage,
                mapped_at_creation: true,
            });
            buffer
                .slice(..)
                .get_mapped_range_mut()
                .unwrap()
                .copy_from_slice(bytes);
            buffer.unmap();
            buffer
        };
        let vertex = wgpu::BufferUsages::VERTEX;
        let positions = upload(packed_positions.as_bytes(), vertex, "positions");
        let indices = upload(packed_indices.as_bytes(), wgpu::BufferUsages::INDEX, "indices");

        // The UV-and-color slot uses the same path, except that the stream
        // compresses the raw attributes and interleaves them in attribute
        // order as it writes, so they are never materialized in a `Vec<u8>`.
        let stream = options.uv_color_stream();
        let uv_color = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uv_color"),
            size: stream.byte_len(packed_positions.len()) as u64,
            usage: vertex,
            mapped_at_creation: true,
        });
        {
            let mut view = uv_color.slice(..).get_mapped_range_mut().unwrap();
            stream.write(&uvs, &colors, &mut metadata, view.slice(..).into_slice(..));
        }
        uv_color.unmap();

        // 3. Per-instance data: an affine model matrix plus a base color.
        let instances = [
            MeshInstance::new(
                glam::Affine3A::from_rotation_y(0.6),
                glam::Vec4::new(1.0, 0.85, 0.4, 1.0),
            ),
            MeshInstance::new(
                glam::Affine3A::from_translation(glam::Vec3::new(1.4, 0.0, -0.5)),
                glam::Vec4::new(0.4, 0.8, 1.0, 1.0),
            ),
        ];
        let instance_count = instances.len() as u32;
        let instances = upload(instances.as_bytes(), vertex, "instances");

        // 4. The global group: camera, frame clock, and the per-mesh decode
        //    parameters. The layouts carry each struct's shader size, so wgpu
        //    validates the bindings when the bind group is created.
        let camera = View::new(glam::Mat4::IDENTITY, glam::Vec3::ZERO);
        let globals = Globals::default();
        let camera = upload(camera.as_bytes(), wgpu::BufferUsages::UNIFORM, "camera");
        let globals = upload(globals.as_bytes(), wgpu::BufferUsages::UNIFORM, "globals");
        let metadata_buffer = upload(
            metadata.as_bytes(),
            wgpu::BufferUsages::STORAGE,
            "mesh_metadata",
        );
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &pipeline.global_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: CAMERA_BINDING,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: FRAME_BINDING,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: MESH_METADATA_BINDING,
                    resource: metadata_buffer.as_entire_binding(),
                },
            ],
        });

        // 5. The material group: the base-color texture and its sampler. Only
        //    present because `base_color_texture` is enabled.
        let (texture_view, sampler) = checkerboard(device, queue);
        let material = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: pipeline.material_layout.as_ref().unwrap(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_TEXTURE_BINDING,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: BASE_COLOR_SAMPLER_BINDING,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // 6. The mesh group selects which metadata entry decodes this draw, so
        //    one pipeline can draw many differently-compressed meshes. It
        //    exists only while a channel is compressed.
        let mesh_info = upload(
            MeshInfo::new(0).as_bytes(),
            wgpu::BufferUsages::UNIFORM,
            "mesh_info",
        );
        let mesh = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh"),
            layout: pipeline.mesh_layout.as_ref().expect("a compressed mesh"),
            entries: &[wgpu::BindGroupEntry {
                binding: MESH_INFO_BINDING,
                resource: mesh_info.as_entire_binding(),
            }],
        });

        Example {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            globals: globals_group,
            material,
            mesh,
            positions,
            uv_color,
            indices,
            index_count: packed_indices.len() as u32,
            instances,
            instance_count,
        }
    }

    /// Record one frame and submit it.
    fn draw(&self, width: u32, height: u32) {
        // The attachment set owns every attachment — the color target, the
        // multisample and depth textures — and is recreated when the target
        // changes. Recording the pass is the caller's: load ops are chosen
        // per pass.
        let attachments = create_render_target(
            &self.device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            width,
            height,
            4,
        )
        .attachments;

        // One draw: the pipeline, every bind group and vertex buffer it needs,
        // and what to draw. Slots are bound by index, so a variant only binds
        // the buffers its shader declares.
        let draw = DrawEntry::new(
            &self.pipeline.pipeline,
            DrawRange::indexed(0..self.index_count).with_instances(0..self.instance_count),
        )
        .with_bind_group(GLOBAL_GROUP, &self.globals)
        .with_bind_group(MATERIAL_GROUP, &self.material)
        .with_bind_group(MESH_GROUP, &self.mesh)
        .with_vertex_buffer(POSITION_SLOT, &self.positions)
        .with_vertex_buffer(UV_COLOR_SLOT, &self.uv_color)
        .with_vertex_buffer(INSTANCE_SLOT, &self.instances)
        .with_index_buffer(&self.indices, wgpu::IndexFormat::Uint16);

        let scene = Scene::new().with_draw(draw);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("encoder") });
        // Everything the scene draws happens in this one pass.
        {
            let mut pass = attachments.begin_pass(
                &mut encoder,
                color_clear(),
                depth_clear(),
                stencil_clear(),
            );
            scene.record(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
    }
}
# /// A unit cube with per-face UVs and vertex colors, generated from the six
# /// faces' normal/tangent/bitangent triples.
# fn cube() -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>) {
#     let faces = [
#         ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, -1.0], [0.0f32, 1.0, 0.0]),
#         ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
#         ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
#         ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
#         ([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
#         ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
#     ];
#     let mut mesh = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
#     for (normal, tangent, bitangent) in faces {
#         let base = mesh.0.len() as u32;
#         let (n, t, b) = (
#             glam::Vec3::from(normal),
#             glam::Vec3::from(tangent),
#             glam::Vec3::from(bitangent),
#         );
#         for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
#             mesh.0.push((n + t * u + b * v).to_array());
#             mesh.1.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
#             mesh.2.push([
#                 ((u + 1.0) * 0.5 * 255.0) as u8,
#                 ((v + 1.0) * 0.5 * 255.0) as u8,
#                 255,
#                 255,
#             ]);
#         }
#         mesh.3
#             .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
#     }
#     mesh
# }
#
# /// An 8x8 checkerboard base-color texture and its sampler.
# fn checkerboard(
#     device: &wgpu::Device,
#     queue: &wgpu::Queue,
# ) -> (wgpu::TextureView, wgpu::Sampler) {
#     const SIZE: u32 = 8;
#     let mut texels = Vec::new();
#     for y in 0..SIZE {
#         for x in 0..SIZE {
#             let v = if (x + y) % 2 == 0 { 255u8 } else { 40 };
#             texels.extend_from_slice(&[v, v, v, 255]);
#         }
#     }
#     let texture = device.create_texture(&wgpu::TextureDescriptor {
#         label: Some("checker"),
#         size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
#         mip_level_count: 1,
#         sample_count: 1,
#         dimension: wgpu::TextureDimension::D2,
#         format: wgpu::TextureFormat::Rgba8UnormSrgb,
#         usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
#         view_formats: &[],
#     });
#     queue.write_texture(
#         texture.as_image_copy(),
#         &texels,
#         wgpu::TexelCopyBufferLayout {
#             offset: 0,
#             bytes_per_row: Some(SIZE * 4),
#             rows_per_image: Some(SIZE),
#         },
#         wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
#     );
#     let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
#     let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
#     (view, sampler)
# }
#
# let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
# let adapter =
#     pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
#         .expect("an adapter");
# let (device, queue) =
#     pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
#         .expect("a device");
let example = Example::new(&device, &queue);
example.draw(256, 192);
device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
# }
```

The doc comments of `mesh`, `buffer_pool`, `offset_allocator`, `staging` and
`render_attachments` also contain runnable examples.

## Shaders

The shaders this crate ships are authored in WESL and bundled at build time; see
[`shader`]. The package always carries the modules mirroring the Rust types a
caller binds (`globals`, `view`, `mesh_metadata` and the `mesh_compression`
decode functions); the `unlit` feature adds the built-in entry shader, which
`UnlitPipeline` composes into the variant `UnlitOptions` selects.

A caller who wants their own entry shader composes it directly with
[`wesl`](https://docs.rs/wesl): [`shader`] is a WESL `StaticPackage`, so
`wesl::resolver::PackageResolver` can resolve
`import unlit_wgpu::mesh_compression;` against the same modules the built-in
pipeline uses.

## Tests

```text
cargo xtask test     # the whole workspace, through nextest plus the doctests
cargo nextest run -p unlit_wgpu   # just this crate
```

The GPU integration tests render meshes into offscreen textures, read them back
and compare them against the snapshots under `tests/snapshots` with the
SSIMULACRA2 perceptual metric. That directory is a symlink into the
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
submodule; clone it with `git submodule update --init`. To re-bless a snapshot
after an intentional change, run the test with `SNAPSHOT_UPDATE=1` set, then
review the image diff before committing it.

## See also

- [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md)
  — the ECS-integrated API built on this crate.
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md)
  — the world that API uses.
- [`unlit_wgpu_test_util`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.md)
  — the headless GPU test harness.
- [Design document](https://github.com/beicause/unlit3d/blob/main/docs/DESIGN.md)
  — architecture and rationale (in Chinese).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
