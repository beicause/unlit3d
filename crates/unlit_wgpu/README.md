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
| `unlit` | yes | [`pipeline`] — `SpecializedUnlitPipeline`, `UnlitOptions`, the `UnlitFlags` variant bits, and the WESL module they compose |
| `egui` | no | the `ui` module: an egui backend that draws tessellated egui output as ordinary screen-space draws. Implies `unlit` |

With `--no-default-features` the crate keeps its general facilities — the
resource graph, mesh compression, the offset allocator and buffer pools,
staging, `Scene`, `RenderAttachments`, the variant cache, and the WESL modules
mirroring the types a caller binds — and drops everything specific to the
built-in pipeline, including the WESL compiler it composes shaders with. CI
checks that combination separately.

## What is in the box

- [`resources`] — a dependency-tracked graph of the GPU resources a frame uses.
  Resources are inserted, replaced and given up through it, and the graph
  propagates "dirty" state to dependents so that derived resources (bind groups,
  pipelines) are rebuilt lazily. Replacing a resource marks everything
  transitively built from it dirty, and a single `maintain` pass per frame
  collects the resources no handle holds any more and runs the rebuild recipes.
  Virtual nodes own no handle and serve as aggregation roots.
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
- [`array_pool`] — an [`Array`](texel_array::Array) sub-allocated the same way,
  for variable-length slices a frame-wide array holds rather than one resource
  each. It keeps a CPU mirror of the bytes, so several slices are written before
  one upload reaches the GPU.
- [`staging`] — host-visible staging buffers reused across frames instead of a
  fresh `queue.write_buffer` allocation per upload.
- [`texel_array`] — a flat array of fixed-size elements bound either as one
  storage buffer or, where the device has none, as a texture the shader reads
  with `textureLoad`. Both paths keep the same bytes and the same binding
  numbers, so a caller picks one handle type and never branches on the device.
- [`capabilities`] — what an adapter can do beyond the WebGPU baseline, captured
  while the adapter is still alive and carried to where a frame is recorded.
  `DeviceCapabilities` holds `base_vertex`; whether storage buffers exist is
  already on the device's own limits. `DeviceTier` is the other half — how much
  of the baseline to request, from the WebGPU baseline (the default) through
  the adapter's own limits to WebGL2's shape.
- [`scene`] — the declarative description of a frame: pipelines, their bind
  groups, materials, meshes, vertex buffers and draw ranges.
- [`render_attachments`] — the attachments a pass renders into, the pass-opening
  entry point, and [`create_render_target`](render_attachments::create_render_target) for an offscreen frame.
- [`specialize`] — variant caching: a [`Specializer`](specialize::Specializer)
  rewrites a [`PipelineDescriptor`](specialize::PipelineDescriptor) for a key,
  and [`Variants`](specialize::Variants) compiles and reuses one
  [`SpecializedPipeline`](specialize::SpecializedPipeline) per key, with a
  canonical map for keys that are not injective.
- [`pipeline`] — the binding slots, bind-group indices and vertex-buffer slots
  the crate draws with, plus the built-in unlit pipeline under the `unlit`
  feature.
- [`util`] — [`Hashed`](util::Hashed), a value whose hash is computed once up
  front: hashing it writes the stored word instead of walking the value, which
  keeps a per-frame key cheap when its members are large.
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
    MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use unlit_wgpu::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_METADATA_BINDING, POSITION_SLOT,
    UV_COLOR_SLOT, UnlitOptions, SpecializedUnlitPipeline,
};
use unlit_wgpu::specialize::SpecializedPipeline;
use unlit_wgpu::render_attachments::{
    color_clear, create_render_target, depth_clear, stencil_clear,
};
use unlit_wgpu::scene::{DrawEntry, DrawRange, Scene};
use zerocopy::IntoBytes;

/// Everything one scene needs, built once and reused every frame.
struct Example {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: SpecializedUnlitPipeline,
    globals: wgpu::BindGroup,
    material: wgpu::BindGroup,
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
        let pipeline = SpecializedPipeline::create(device, options.clone());

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

        // 3. Per-instance data: an affine model matrix, a base color and the
        //    index of the metadata entry this mesh decodes through. The index
        //    is the mesh's, but it rides the instance stream because that is
        //    what a draw reaches without a bind group of its own.
        let metadata_index = 0;
        let instances = [
            MeshInstance::new(
                glam::Affine3A::from_rotation_y(0.6),
                glam::Vec4::new(1.0, 0.85, 0.4, 1.0),
            )
            .with_metadata_index(metadata_index),
            MeshInstance::new(
                glam::Affine3A::from_translation(glam::Vec3::new(1.4, 0.0, -0.5)),
                glam::Vec4::new(0.4, 0.8, 1.0, 1.0),
            )
            .with_metadata_index(metadata_index),
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
            layout: &pipeline.descriptor().bind_group_layouts(device).global,
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
            layout: &pipeline.descriptor().bind_group_layouts(device).material.unwrap(),
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

        // 6. No mesh group: the built-in variants read every input from the
        //    global group or the instance stream, so a draw binds none of its
        //    own. The index remains a general extension point a caller's
        //    pipeline can bind per-mesh data into.

        Example {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            globals: globals_group,
            material,
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

The doc comments of `mesh`, `array_pool`, `buffer_pool`, `offset_allocator`,
`staging` and `render_attachments` also contain runnable examples.

## Shaders

The shaders this crate ships are authored in WESL and bundled at build time; see
[`shader`]. The package always carries the modules mirroring the Rust types a
caller binds (`globals`, `view`, `mesh_metadata` and the `mesh_compression`
decode functions); the `unlit` feature adds the built-in entry shader, which
`SpecializedUnlitPipeline` composes into the variant `UnlitOptions` selects.

A caller who wants their own entry shader composes it directly with
[`wesl`](https://docs.rs/wesl): [`shader`] is a WESL `StaticPackage`, so
`wesl::resolver::PackageResolver` can resolve
`import unlit_wgpu::mesh_compression;` against the same modules the built-in
pipeline uses.

## Design

### Resources: one dependency-tracked graph

The crate embraces `wgpu` and manages its resources directly: no unnecessary
wrappers, and no high-level API for CPU-side data management and
synchronization. What it does make convenient is creating *its own* GPU
resources — UBO and SSBO struct declarations, and mesh quantization and
compression — rather than hiding `wgpu` itself.

[`resources`] keeps every resource and its dependencies in one directed acyclic
graph:

- **Acyclicity is an invariant the graph itself guarantees.** A dependency that
  would close a cycle is rejected where it is declared, so the graph can always
  be traversed in dependency order.
- **Handles are typed.** `ResourceId<R>`'s `R` is the resource type itself:
  `get` returns that resource directly with no variant matching, and `replace`
  accepts only the same type, so an id cannot come to mean a different kind of
  resource. Where the type cannot be known at compile time — dependency sets,
  dirty-resource walks, a stored field handed back to the graph — the erased
  `ResourceId<Resource>` is still used.
- **Insertion is immediate.** Inserting creates the node and returns its id,
  dependencies are declared one at a time with `add_dependency`, and there is no
  intermediate state waiting to be finished. A dependency that cannot be
  recorded — a cycle — panics where it is declared rather than returning an
  error: that is a caller error, and it can only be surfaced there.
- **A handle is a reference.** `ResourceId` is a counted handle, not a bare
  index: cloning one takes another reference to the resource and dropping one
  gives it up. A resource lives exactly while some id names it, so there is no
  removal call and no handle that goes stale. A dependency holds a reference
  too, which is what lets a resource built only to feed a consumer live exactly
  as long as that consumer does.
- **Tracking is precise, updates are lazy.** `replace` only marks a resource
  dirty — the resources depending on it need updating too. Nothing happens at
  the point of the change. A frame calls
  [`ResourceGraph::maintain`](resources::ResourceGraph::maintain) exactly once,
  before it reads any resource: that one pass collects the resources nothing
  holds any more — following the references a collected node releases, so a
  whole chain goes in one call — and rebuilds the dirty resources in dependency
  order. Uploading new bytes into an existing buffer dirties nothing;
  reallocating it dirties only what actually consumed the old handle.
- **A rebuild is a recipe, not a callback at the call site.** A resource the
  graph can rebuild is inserted with a [`Rebuild`](resources::Rebuild) closure
  that reads its inputs back out of the graph by id, so it observes a buffer
  that was reallocated or an array that was replaced at the moment it runs. A
  dirty node with no recipe stays dirty: the caller changed something the graph
  cannot rebuild on its own.
- **A texture view's format is recorded with the view.** This is the one
  exception to "no unnecessary wrappers": `wgpu` cannot tell a `TextureView`'s
  format from the view itself, and when an sRGB view covers a non-sRGB texture,
  the format a pipeline has to match is the view's rather than the texture's.

![The GPU resources one frame depends on, as a graph](https://raw.githubusercontent.com/beicause/unlit3d/main/crates/unlit_wgpu/assets/webgpu-draw-diagram.svg)

### Drawing: the scene is data

Drawing — the render pass — is data-driven too. [`Scene`](scene::Scene) is a
declarative list of draws, each naming the pipeline, bind groups, vertex buffers
and draw range it needs, and
[`Scene::record`](scene::Scene::record) replays it into a
[`wgpu::RenderPass`](https://docs.rs/wgpu/latest/wgpu/struct.RenderPass.html):

```rust,ignore
for draw in scene.draws {
    pass.set_pipeline(&draw.pipeline);
    for (index, bind_group) in draw.bind_groups {
        pass.set_bind_group(index, bind_group);
    }
    for (slot, buffer, range) in draw.vertex_buffers {
        pass.set_vertex_buffer(slot, buffer, range);
    }
    if let Some((buffer, range, format)) = draw.index_buffer {
        pass.set_index_buffer(buffer, range, format);
    }
    if let Some(scissor) = draw.scissor {
        pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
    }
    pass.set_stencil_reference(draw.stencil_reference);

    match draw.range {
        DrawRange::Indexed { indices, base_vertex, instances } => {
            pass.draw_indexed(indices, base_vertex, instances)
        }
        DrawRange::Vertices { vertices, instances } => pass.draw(vertices, instances),
    }
}
```

Recording tracks the pass state — pipeline, bind groups, vertex buffers, index
buffer, scissor and stencil reference — and skips a `set_*` whose target is
already bound, so a scene whose entries are ordered to keep neighbouring draws
alike costs one state change per run rather than one per draw. That ordering is
the higher layer's job, not this crate's: `Scene` records what it is given.

### Pipeline specialization: a key, a specializer, a cached pipeline

<details>
<summary>Why the cache is shaped this way</summary>

Compiling a pipeline is expensive and the result is valid for one exact
configuration, yet one **blueprint** derives many concrete pipelines across
dimensions like the render target, the vertex layout and the blend state.
Automating "compile once per configuration, then reuse" needs three concepts:

- A **key** (`SpecializerKey`) names one configuration. A key may be
  **injective** — distinct keys necessarily mean distinct blueprints, as a render
  target does — or it may not be: a key can carry information the blueprint does
  not depend on, such as a mesh's raw vertex attributes, of which the shader
  reads only a few formats. A non-injective key therefore also has a **canonical
  form** (`Canonical`): two keys with the same canonical form share one pipeline.
- A **specializer** (`Specializer`) is a pure function that applies a key to a
  blueprint, rewriting it in place, and reports the key's canonical form.
- A **cached pipeline** (`Variants`) ties the two together: it caches compiled
  results by key, storing the variants in creation order and returning their
  index.

**The blueprint is `PipelineDescriptor<P>`, parameterized over the pipeline it
compiles into.** Caching a variant has nothing to do with the kind of pipeline,
so `P` is not fixed: a render pipeline is a
`PipelineDescriptor<wgpu::RenderPipeline>` and a compute pipeline a
`PipelineDescriptor<wgpu::ComputePipeline>`, both sharing one cache and one set
of specializers.

**A specializer takes no `device` parameter**, so it can only be a pure
"key to blueprint rewrite" and cannot compile anything. This is exactly why a
blueprint has to be a **semantic description** (such as `UnlitOptions`) rather
than a mirror of a wgpu descriptor: the latter holds handles like
`ShaderModule` that need a device to create. WESL composition and the actual
wgpu calls therefore both stay inside the blueprint's `create`.

**The cache has two levels but no cache of the whole blueprint.** The first
level maps the caller's key to a variant index, the second maps the canonical
form to a variant index; when the key is injective the second is never
consulted. The blueprint itself is not the key because a blueprint cannot be
hashed (its compilation constants hold `f64`), and because "memoize on a small
key" is the whole point of these types: a family has finitely many keys, and a
lookup is cheaper than comparing a whole blueprint field by field. Variants are
never evicted, matching the bounded-key assumption; as a backstop for the
two-level cache's correctness, debug builds check that one canonical form always
yields one blueprint.

**Specializing for a render target is not the built-in shader's private
business.** A pipeline's color format, sample count and depth format have to
match the attachments of the pass it is used in. That holds for any pipeline,
whoever wrote the blueprint. This dimension is therefore a general specializer,
[`SurfaceSpecializer`](specialize::SurfaceSpecializer), acting on any blueprint
that implements [`SurfaceTarget`](specialize::SurfaceTarget): the built-in unlit
pipeline and a custom one each implement that trait, rather than the built-in
pipeline monopolizing the logic in a private function. "A target with no depth
attachment" is also legal, so the depth state follows the target optionally: in
that case the blueprint must carry **no** depth state rather than keeping a
stale format.

</details>

**A bind-group layout is not pipeline state.** The built-in unlit pipeline's
three layouts (global, material, mesh) follow entirely from the blueprint, and
can be used standalone without compiling anything — to describe a material's
binding interface, say — so they are derived from the blueprint on demand rather
than stored beside the compiled product, which avoids a second source of truth
drifting from the blueprint.

**The built-in pipeline has no privileges.** It is an ordinary user of the same
mechanism: its blueprint `UnlitOptions` implements `PipelineDescriptor`, and
`SpecializedUnlitPipeline` is nothing more than the alias
`SpecializedPipeline<wgpu::RenderPipeline, UnlitOptions>`. There is no second
compilation path laid down for the built-in shader. Registering it with the
higher layer's family mechanism is
[`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md)'s
business.

### Reading the frame's arrays: storage buffers or texels

The built-in shader reads four frame-wide arrays: the per-mesh decode
parameters, the frame's joint matrices, its morph weights and its morph
displacements. The last is a mesh's own geometry, but one array holds it for the
whole frame and a mesh names its slice through its metadata entry, which is what
leaves the mesh group with nothing to bind. The straightforward binding for each
is a read-only storage buffer, and every device that meets the WebGPU baseline
takes it.

WebGL2 does not meet that baseline here: GLES 3.0 has no SSBO at all, `wgpu`
reports `max_storage_buffers_per_shader_stage` as zero, and a bind-group layout
naming one is rejected outright. The same arrays are therefore also available as
textures — each element's bytes become `f32` lanes of an `Rgba32Float` or
`R32Float` 2D texture, read with `textureLoad`. [`texel_array`] owns that layout
and its upload; [`shader`]'s `array_access.wesl` owns the accessors every caller
goes through, so the shader body is written once and only that module knows
which resource arrived.

Both paths keep the binding numbers, the byte layout and the element order, so
the difference stays confined to the resource type. Which one is used follows
the device: [`UnlitOptions::standard`](pipeline::UnlitOptions::standard) sets
`UnlitFlags::TEXEL_ARRAY` from the device's limits. A caller building a variant
of its own sets its flags through
[`UnlitOptions::with_flags`](pipeline::UnlitOptions::with_flags), which keeps the
device's answer — assigning `flags` outright would drop it and ask a
storage-less device for a binding it rejects.

Every buffer binding states its `min_binding_size`, and a uniform one states a
size that is a multiple of 16. A device without
`BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED` — WebGL2, and ANGLE's GLES — rejects a
pipeline whose uniform binding is not, which is why the uniform types derive
[`const_shader_layout::ShaderLayoutCompat`]: it rounds each struct's size up to
16, so the layout cannot drift from the rule. Storage bindings have no such
requirement, and their minimum is the size of one element, which is what lets
an array grow without invalidating the layout.

<details>
<summary>Why the row width is read back rather than declared</summary>

`COPY_BYTES_PER_ROW_ALIGNMENT` is 256 bytes, so a row of a copied texture has to
span a whole number of those, and an element narrower than that shares its row
with others. Rather than bake the resulting width into the shader, the CPU picks
a row that satisfies both the copy alignment and the texture limit, and the
shader recovers the width with `textureDimensions`. The shader then depends on
neither the element size nor the device's resolution limit, and an element never
straddles a row, so growing the array never splits one.

</details>

### Per-frame uploads: pooled staging buffers

<details>
<summary>Why not <code>queue.write_buffer</code>, and why not <code>StagingBelt</code></summary>

Data that changes every frame — the camera and globals uniform, per-instance
data, mesh metadata — is uploaded through staging buffers reused across frames
rather than by calling `queue.write_buffer` each time:

- Every `queue.write_buffer` call allocates a fresh temporary staging buffer and
  submits a copy of its own, so it can neither batch with the rest of the
  frame's work nor avoid an allocation per frame.
- `wgpu`'s `StagingBelt` is equally unsuitable: growth between frames makes it
  hold onto a block of every size it has ever seen, forever, and it never
  releases them.
- Each destination buffer therefore keeps its own pool of staging buffers: the
  host writes into a reused mapping, the encoder records the copy, and the
  mapping goes back to the host for later frames once the copy completes.

The pool's size settles at the number of frames in flight and does not grow with
the frame count; when a frame grows, an undersized buffer is replaced rather
than kept alongside; after the size falls for a while it can be reclaimed
explicitly.

</details>

Per-frame upload is transparent to the caller: the caller maintains only
CPU-side data, such as allocating or removing meshes, and the changes are
synchronized to the GPU automatically when the frame renders, with no upload API
to remember. This differs from the lazy dependency-graph updates in
[Resources](#resources-one-dependency-tracked-graph), which the user triggers
through one `maintain` call per frame, after a buffer or other resource is
replaced or given up.

A frame's uploads and the render pass consuming them are recorded into one
encoder, so a frame is one submission, which keeps rendering's completion
deterministic; a frame with no content is submitted too, to carry that frame's
uploads.

## Tests

```text
cargo xtask test     # the whole workspace, through nextest plus the doctests
cargo nextest run -p unlit_wgpu   # just this crate
```

The GPU integration tests render meshes into offscreen textures, read them back
and compare them against the snapshots under `tests/snapshots` with the
SSIMULACRA2 perceptual metric. That directory is a symlink into the
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
submodule; clone it here with `git submodule update --init --checkout`, which is
what gets past the `update = none` that keeps a git dependency from fetching
snapshots it never compares against. To re-bless a snapshot
after an intentional change, run the test with `SNAPSHOT_UPDATE=1` set, then
review the image diff before committing it.

The unit tests live inside `src/` and cover the private pure logic; the
integration tests in `tests/` reach the crate through its public API only. Where
each layer sits across the workspace, and what CI runs, is in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

## See also

- [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md)
  — the ECS-integrated API built on this crate.
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md)
  — the world that API uses.
- [`unlit_wgpu_test_util`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.md)
  — the headless GPU test harness.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
