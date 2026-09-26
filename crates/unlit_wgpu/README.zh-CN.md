[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.md) | 简体中文

# unlit_wgpu

面向 WebGPU 的、紧凑且有主见的**无光照（unlit）**绘制渲染器。它在一个 render pass
内把整个场景——不透明与透明实例一视同仁——绘制到调用者指定的一个
`wgpu::TextureView`；目标平台是 WebGPU（及其背后的原生后端），并且移动端优先。
不支持 WebGL 与 GLES。

本 crate 是分层的：下面这些模块是调用者搭建任意管线所用的通用设施，而内置 unlit
管线——以及用它的 egui 后端——由 feature 添加。通用模块完全不知道内置管线的存在：
它使用与调用者自建管线完全相同的绑定槽位约定、顶点压缩与资源追踪。

本 crate 处于**极早期开发阶段**；API 会自由变动。它不依赖 ECS 层；构建在其上的
与 ECS 集成的 API 是
[`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `unlit` | 是 | `pipeline::UnlitPipeline`、`pipeline::UnlitOptions`、`UnlitFlags` 变体位，以及它们所组合的 WESL 模块 |
| `egui` | 否 | `ui` 模块：一个把 egui 的细分输出当作普通屏幕空间绘制来画的后端。隐含 `unlit` |

使用 `--no-default-features` 时，本 crate 保留其通用设施——资源图、网格压缩、偏移
分配器与缓冲池、staging、`Scene`、`RenderAttachments`、变体缓存，以及镜像调用者
所绑定类型的那些 WESL 模块——并去掉一切与内置管线相关的东西，包括用于组合着色器
的 WESL 编译器。CI 会单独检查这一组合。

## 内容概览

- `resources` —— 一帧所用 GPU 资源的依赖追踪图。替换某个资源会把所有由它传递
  构建出的资源标记为脏；移除某个资源会一并丢弃其依赖者；虚拟节点不持有任何句柄，
  用作聚合根。
- `mesh` —— 顶点压缩（位置 `Snorm16x4`、UV `Snorm16x2`、顶点色 `Unorm8x4`、骨骼
  索引 `Uint16x4`、骨骼权重 `Unorm16x4`）、让这些紧凑格式可在着色器中使用
  `MeshMetadata` 解码参数，以及一个无需把各通道实体化即可打包的顶点流写入器。
- `offset_allocator` —— 在一段连续区间上 O(1) 且不分配的次级分配器，采用
  Aaltonen 的 `OffsetAllocator` 中的两级分离适配算法。
- `buffer_pool` 与 `vertex_pool` —— 用上述分配器做次级分配的 GPU 缓冲，使许多网格
  按种类或按顶点布局共享同一个缓冲。
- `staging` —— 跨帧复用的 host 可见 staging 缓冲，而不是每次上传都让
  `queue.write_buffer` 新分配一个。
- `scene` —— 一帧的声明式描述：管线、它们的绑定组、材质、网格、顶点缓冲与绘制
  区间。
- `render_attachments` —— 一个 pass 渲染到的附件、开启 pass 的入口，以及用于离屏
  帧的 `create_render_target`。
- `specialize` —— 变体缓存：`Specializable` 值按 key 编译一次并复用；对非单射的
  key 另有一张规范形式映射表。
- `pipeline` —— 本 crate 绘制所用的绑定槽位、绑定组索引与顶点缓冲槽位；在
  `unlit` feature 下还包含内置 unlit 管线。
- `ui` —— egui 后端，在 `egui` feature 下提供。

## 示例

一个场景所需的全部内容只构建一次，之后每帧复用；`Example::draw` 就是帧循环里运行的
部分。下面是一次完整的走查：设备创建、网格压缩、每一个绑定组与绘制。

```rust
# #[cfg(feature = "unlit")]
# fn main() {
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
# #[cfg(not(feature = "unlit"))]
# fn main() {}
```

`mesh`、`buffer_pool`、`offset_allocator`、`staging` 与 `render_attachments`
的文档注释中也含有可运行示例。

## 着色器

本 crate 附带的着色器以 WESL 编写，并在构建期打包；见 `shader`。该包始终带有镜像
调用者所绑定 Rust 类型的模块（`globals`、`view`、`mesh_metadata` 与
`mesh_compression` 解码函数）；`unlit` feature 额外加入内置入口着色器，由
`pipeline::UnlitPipeline` 组合成 `pipeline::UnlitOptions` 所选的变体。

若要编写自己的入口着色器，请直接用 [`wesl`](https://docs.rs/wesl) 组合它：`shader`
是一个 WESL `StaticPackage`，因此 `wesl::resolver::PackageResolver` 可以针对内置
管线所用的同一批模块解析 `import unlit_wgpu::mesh_compression;`。

## 测试

```text
cargo xtask test     # 整个工作区：nextest 加 doctest
cargo nextest run -p unlit_wgpu   # 只跑本 crate
```

GPU 集成测试把网格渲染到离屏纹理上，回读后与 `tests/snapshots` 下的快照用
SSIMULACRA2 感知指标比较。该目录是指向
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
submodule 的软链接；用 `git submodule update --init` 拉取。若确有意改动后要重新生成
快照，带 `SNAPSHOT_UPDATE=1` 运行对应测试，然后在提交前审查图像差异。

## 另见

- [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
  —— 构建在本 crate 之上的 ECS 集成 API。
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)
  —— 该 API 使用的 world。
- [`unlit_wgpu_test_util`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.zh-CN.md)
  —— 无头 GPU 测试骨架。
- [设计文档](https://github.com/beicause/unlit3d/blob/main/docs/DESIGN.md)
  —— 架构与设计取舍。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
