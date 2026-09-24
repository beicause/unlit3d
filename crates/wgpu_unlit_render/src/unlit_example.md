# Example

Drawing a textured, vertex-colored cube with the built-in pipeline. The
setup work — device creation, uploading the mesh, building the bind groups
— happens once; `Example::draw` is what runs every frame.

```rust
use wgpu_unlit_render::globals::{Globals, View};
use wgpu_unlit_render::mesh::{
    MeshInfo, MeshInstance, MeshMetadata, compress_indices, compress_positions,
};
use wgpu_unlit_render::pipeline::{
    BASE_COLOR_SAMPLER_BINDING, BASE_COLOR_TEXTURE_BINDING, CAMERA_BINDING, FRAME_BINDING,
    GLOBAL_GROUP, INSTANCE_SLOT, MATERIAL_GROUP, MESH_GROUP, MESH_INFO_BINDING,
    MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT, UnlitFlags, UnlitOptions,
    UnlitPipeline,
};
use wgpu_unlit_render::render_attachments::{
    color_clear, create_render_target, depth_clear, stencil_clear,
};
use wgpu_unlit_render::scene::{DrawEntry, DrawRange, Scene};
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
    // every frame is better off in a `COPY_DST` buffer written through
    // `queue.write_buffer`, which needs no map/unmap cycle.
    let upload = |bytes: &[u8], usage: wgpu::BufferUsages, label: &str| {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes.len() as u64,
            usage,
            mapped_at_creation: true,
        });
        buffer.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(bytes);
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
    let instances = vec![
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
    //    parameters. `min_binding_size` takes each struct's shader size,
    //    so wgpu validates the bindings when the bind group is created.
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
    let attachments = create_render_target(&self.device, wgpu::TextureFormat::Rgba8UnormSrgb, width, height, 4).attachments;

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
    .with_vertex_buffer(POSITION_SLOT, self.positions.slice(..))
    .with_vertex_buffer(UV_COLOR_SLOT, self.uv_color.slice(..))
    .with_vertex_buffer(INSTANCE_SLOT, self.instances.slice(..))
    .with_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);

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
# }
#
# /// A unit cube with per-face UVs and vertex colors.
# fn cube() -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[u8; 4]>, Vec<u32>) {
#     let faces = [
#         ([-1.0f32, 0.0, 0.0], [0.0f32, 0.0, -1.0], [0.0f32, 1.0, 0.0]),
#         ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
#         ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
#         ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
#         ([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
#         ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
#     ];
#     let (mut positions, mut uvs, mut colors, mut indices) =
#         (Vec::new(), Vec::new(), Vec::new(), Vec::new());
#     for (normal, tangent, bitangent) in faces {
#         let base = positions.len() as u32;
#         let (n, t, b) = (
#             glam::Vec3::from(normal),
#             glam::Vec3::from(tangent),
#             glam::Vec3::from(bitangent),
#         );
#         for (u, v) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
#             positions.push((n + t * u + b * v).to_array());
#             uvs.push([(u + 1.0) * 0.5, (v + 1.0) * 0.5]);
#             colors.push([
#                 ((u + 1.0) * 0.5 * 255.0) as u8,
#                 ((v + 1.0) * 0.5 * 255.0) as u8,
#                 255,
#                 255,
#             ]);
#         }
#         indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
#     }
#     (positions, uvs, colors, indices)
# }
#
# /// An 8x8 checkerboard base-color texture.
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
```
