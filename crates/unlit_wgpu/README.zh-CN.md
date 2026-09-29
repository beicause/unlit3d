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
| `unlit` | 是 | `pipeline::SpecializedUnlitPipeline`、`pipeline::UnlitOptions`、`UnlitFlags` 变体位，以及它们所组合的 WESL 模块 |
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
- `texel_array` —— 定长元素的平坦数组，既可以绑定为一个 storage buffer，也可以在
  设备没有 storage buffer 时绑定为纹理、由着色器用 `textureLoad` 读取。两条路径
  保持相同的字节与相同的绑定编号，因此调用者只需选一种句柄，无需按设备分支。
- `capabilities` —— 适配器在 WebGPU 基线之外还能做什么；在适配器仍存活时采集，
  随帧走到录制绘制之处。`DeviceCapabilities` 只装 `base_vertex`，是否具备 storage
  buffer 已在设备自身的 limits 上。`DeviceTier` 是另一半——请求多少基线能力：从
  WebGPU 基线（默认），到适配器自身的 limits，再到 WebGL2 的形态。
- `scene` —— 一帧的声明式描述：管线、它们的绑定组、材质、网格、顶点缓冲与绘制
  区间。
- `render_attachments` —— 一个 pass 渲染到的附件、开启 pass 的入口，以及用于离屏
  帧的 `create_render_target`。
- `specialize` —— 变体缓存：`Specializer` 按 key 改写
  `PipelineDescriptor`，`Variants` 则为每个 key 编译并复用一个
  `SpecializedPipeline`；对非单射的 key 另有一张规范形式映射表。
- `pipeline` —— 本 crate 绘制所用的绑定槽位、绑定组索引与顶点缓冲槽位；在
  `unlit` feature 下还包含内置 unlit 管线。
- `util` —— `Hashed`，一个预先算好哈希的值：对它求哈希只需写入已存的那个字，
  而不必遍历值本身；当每帧 key 的成员较大时，这让 key 保持廉价。
- `ui` —— egui 后端，在 `egui` feature 下提供。

## 示例

一个场景所需的全部内容只构建一次，之后每帧复用；`Example::draw` 就是帧循环里运行的
部分。下面是一次完整的走查：设备创建、网格压缩、每一个绑定组与绘制。

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
    MESH_METADATA_BINDING, POSITION_SLOT, UV_COLOR_SLOT, UnlitOptions, SpecializedUnlitPipeline,
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
            layout: &pipeline.descriptor().bind_group_layouts(device).mesh.expect("a compressed mesh"),
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

`mesh`、`buffer_pool`、`offset_allocator`、`staging` 与 `render_attachments`
的文档注释中也含有可运行示例。

## 着色器

本 crate 附带的着色器以 WESL 编写，并在构建期打包；见 `shader`。该包始终带有镜像
调用者所绑定 Rust 类型的模块（`globals`、`view`、`mesh_metadata` 与
`mesh_compression` 解码函数）；`unlit` feature 额外加入内置入口着色器，由
`pipeline::SpecializedUnlitPipeline` 组合成 `pipeline::UnlitOptions` 所选的变体。

若要编写自己的入口着色器，请直接用 [`wesl`](https://docs.rs/wesl) 组合它：`shader`
是一个 WESL `StaticPackage`，因此 `wesl::resolver::PackageResolver` 可以针对内置
管线所用的同一批模块解析 `import unlit_wgpu::mesh_compression;`。

## 设计

### 资源：一张依赖追踪图

本 crate 拥抱 `wgpu` 并直接管理其资源：不做不必要的包装，也不提供面向 CPU 侧数据
管理与同步的高层 API。它真正提供便利的是创建**它自己的** GPU 资源——UBO 与 SSBO
结构体声明、网格量化与压缩——而不是把 `wgpu` 本身藏起来。

`resources` 把每个资源及其依赖放进一张有向无环图：

- **无环是图本身保证的不变量。** 会成环的依赖在声明处即被拒绝，因此依赖图始终可按
  依赖序遍历。
- **句柄是带类型的。** `ResourceId<R>` 的 `R` 即资源类型本身：`get` 直接返回该资源，
  无需按变体匹配；`replace` 只接受同类型资源，故 id 不会改指另一种资源。类型在编译期
  无从得知处——依赖集合、脏资源遍历、移除的返回值——仍用擦除的 `ResourceId<Resource>`。
- **插入是即时的。** 插入即建节点并返回 id，依赖由 `add_dependency` 逐个声明，没有待
  收尾的中间状态。句柄带世代数，节点被移除后槽位虽会被复用，陈旧 id 也只解析为空，
  不会改指新占用该槽位的资源。依赖 id 失效就地报错而非返回错误：这属于调用方错误，
  只能在声明处暴露。
- **追踪精准，更新延迟。** 替换资源会把它标记为脏，这也意味着依赖它的资源需要更新；
  移除一个资源会连同依赖它的一并移除。用户调用特定 API 来使资源状态更新，比如在
  buffer 扩容后更新依赖它的绑定组。
- **纹理视图的格式随视图记录。** 这是「不做不必要的包装」的唯一例外：wgpu 无法从
  `TextureView` 得知其创建时的格式，而 sRGB 视图覆盖非 sRGB 纹理时，管线要匹配的正是
  视图格式而非纹理格式。

![一帧所依赖的 GPU 资源图](https://raw.githubusercontent.com/beicause/unlit3d/main/crates/unlit_wgpu/assets/webgpu-draw-diagram.svg)

### 绘制：场景即数据

绘制过程——即 render pass——也遵循数据驱动。`Scene` 是一份声明式的绘制列表，每条
绘制指明它所需的管线、绑定组、顶点缓冲与绘制范围，`Scene::record` 把它重放进一个
`wgpu::RenderPass`：

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

录制过程会追踪 pass 状态——管线、绑定组、顶点缓冲、索引缓冲、scissor 与模板参考
值——并跳过目标已绑定的 `set_*`，因此当场景的条目排列得让相邻绘制状态相同时，每段
状态只需一次切换而不是每条绘制一次。这个排列是更高层的职责，不属于本 crate：
`Scene` 只按给定顺序录制。

### 管线特化：键、特化器、缓存的管线

<details>
<summary>为什么缓存是这个形状</summary>

管线的编译昂贵，且只在一种精确配置下有效；同一份**蓝图**却会在渲染目标、顶点布局、
混合状态等维度上派生出多条具体管线。把「按配置编译一次、之后复用」自动化，只需要
三个概念：

- **键**（`SpecializerKey`）命名一种配置。键可能是**单射**的——不同键必然对应不同
  蓝图，如渲染目标；也可能不是——键携带蓝图并不依赖的信息，如网格的原始顶点属性
  （着色器只关心其中几个格式）。非单射的键因此另有一个**规范形式**（`Canonical`）：
  规范形式相同的两个键共用一条管线。
- **特化器**（`Specializer`）是纯函数，把一个键作用到蓝图上就地改写，并回报该键的
  规范形式。
- **缓存的管线**（`Variants`）把前两者绑在一起：按键缓存编译结果，变体以创建顺序存为
  数组并返回其下标。

**蓝图是 `PipelineDescriptor<P>`，以编译出的管线类型 `P` 为参数。** 缓存一条变体与
管线种类无关，所以 `P` 不该被写死：渲染管线是 `PipelineDescriptor<wgpu::RenderPipeline>`，
计算管线是 `PipelineDescriptor<wgpu::ComputePipeline>`，两者共用同一个缓存与同一套特化。

**特化器没有 `device` 参数**，因此它只能是「键 → 蓝图改写」的纯函数，不能编译任何
东西。这正是蓝图必须是**语义描述**（如 `UnlitOptions`）而不能是 wgpu 描述符镜像的
原因：后者持有 `ShaderModule` 等需要用 device 创建的句柄。WESL 组合与真正的 wgpu
调用因此都留在蓝图的 `create` 里。

**缓存分两级，但没有「整份蓝图」的缓存。** 一级是调用方的键到变体下标，二级是规范
形式到变体下标；键为单射时二级永不被查询。不以蓝图本身为键，是因为蓝图不可哈希
（编译期常量含 `f64`），而且「用一个小键做记忆化」正是这套类型存在的意义——一个
家族的键有限，查表比逐字段比较整份蓝图便宜。变体永不淘汰，这与「键有限」的前提一致；
作为两级缓存正确性的兜底，调试构建下会校验「同一规范形式必须产生同一蓝图」。

**针对渲染目标特化不是内置着色器的私事。** 管线的颜色格式、采样数与深度格式必须与
它所在 pass 的附件匹配，这对任何管线都成立，与蓝图由谁所写无关。这一维度因此是一个
通用特化器 `SurfaceSpecializer`，作用在实现了 `SurfaceTarget` 的任意蓝图上：内置
unlit 与自定义管线各自实现该 trait，而不是由内置管线用私有函数独占这份逻辑。「目标
没有深度附件」也是合法用法，故深度状态随目标可选：此时蓝图必须**不带**深度状态，
而不是留着一个过期的格式。

</details>

**绑定组的布局不是管线的状态。** 内置 unlit 的三个布局（全局、材质、网格）完全由
蓝图决定，且能在不编译任何东西的情况下独立使用——比如描述一条材质的绑定接口——因此
它们按需从蓝图推导，而不是与编译产物并排存一份，避免布局与蓝图两份真相漂移。

**内置管线没有特权。** 它只是同一套机制的一个普通使用者：其蓝图 `UnlitOptions` 实现
`PipelineDescriptor`，而 `SpecializedUnlitPipeline` 不过是
`SpecializedPipeline<wgpu::RenderPipeline, UnlitOptions>` 的别名。不存在第二条专为
内置着色器铺设的编译路径。把它注册进更高层的家族机制，是
[`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
的事。

### 读取本帧的数组：storage buffer 还是 texel

内置着色器要读四个帧级数组：逐网格的解码参数、本帧的骨骼矩阵、本帧的形变权重，
以及某个网格自身的形变位移。最直接的绑定方式是只读 storage buffer，凡满足 WebGPU
基线的设备都走这条。

WebGL2 在这里不满足基线：GLES 3.0 完全没有 SSBO，`wgpu` 报告的
`max_storage_buffers_per_shader_stage` 为 0，声明 storage buffer 的绑定组布局会被
直接拒绝。因此同一批数组也提供纹理形式——把每个元素的字节当作 `Rgba32Float` 或
`R32Float` 二维纹理的 `f32` 通道，用 `textureLoad` 读取。[`texel_array`] 负责这套
布局与上传；[`shader`] 的 `array_access.wesl` 负责所有调用者都经过的取值函数，
于是着色器主体只写一遍，只有那个模块知道到达的是哪种资源。

两条路径保持相同的绑定编号、字节布局与元素顺序，差异因此被限制在资源类型上。
用哪条由设备决定：[`UnlitOptions::standard`](pipeline::UnlitOptions::standard) 依据
设备 limits 设置 `UnlitFlags::TEXEL_ARRAY`。自建变体的调用者应通过
[`UnlitOptions::with_flags`](pipeline::UnlitOptions::with_flags) 设置 flags，它会保留
设备给出的答案——直接赋值 `flags` 会把它丢掉，进而向没有 storage buffer 的设备索要
一个会被拒绝的绑定。

每个 buffer 绑定都写明 `min_binding_size`，其中 uniform 的大小必须是 16 的整数倍。
缺少 `BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED` 的设备——WebGL2 与 ANGLE 的 GLES——会
拒绝 uniform 绑定不满足该条件的管线，这也是 uniform 类型都派生
[`const_shader_layout::ShaderLayoutCompat`] 的原因：它把结构体大小向上取整到 16，
使布局无从偏离该规则。storage 绑定没有这一要求，其最小值是一个元素的大小，这也正是
数组增长不会让布局失效的原因。

<details>
<summary>为什么行宽是读回来的而不是写死的</summary>

`COPY_BYTES_PER_ROW_ALIGNMENT` 是 256 字节，所以被拷贝纹理的一行必须跨越整数个该
对齐，窄于它的元素要与其他元素共处一行。与其把由此得到的宽度写进着色器，不如让 CPU
挑一个同时满足拷贝对齐与纹理上限的行宽，着色器再用 `textureDimensions` 把宽度读回来。
这样着色器既不依赖元素大小，也不依赖设备的纹理上限；而元素永不跨越行边界，数组增长
也就永远不会把一个元素切开。

</details>

### 逐帧上传：池化的 staging buffer

<details>
<summary>为什么不用 <code>queue.write_buffer</code>，也不用 <code>StagingBelt</code></summary>

逐帧变化的数据——相机与 globals uniform、逐实例数据、mesh 元数据——通过跨帧复用的
staging buffer 上传，而不是逐次调用 `queue.write_buffer`：

- `queue.write_buffer` 每次调用都新分配一个临时 staging buffer，并自行提交一次 copy，
  因此既无法与帧内其他工作合批，又每帧都有分配。
- `wgpu` 的 `StagingBelt` 同样不合适：帧间尺寸增长会让它永久持有每种出现过的尺寸的
  块，且从不释放。
- 因此每个目标 buffer 自持一个 staging buffer 池：host 写入复用的映射，encoder 记录
  copy，复制完成后映射交还 host 供后续帧再次使用。

池的大小稳定在在飞帧数，不随帧数增长；帧变大时替换过小的 buffer 而不是并存；尺寸
长期回落后可显式回收。

</details>

逐帧上传对调用方透明：调用方只维护 CPU 侧数据，如分配或移除 mesh，渲染帧时自动把
变更同步到 GPU，无需记住调用上传 API。这不同于[资源](#资源一张依赖追踪图)一节中依赖
图的延迟更新，后者在 buffer 等资源被替换后仍由用户调用 API 触发。

一帧的上传与消费它们的 render pass 记录进同一个 encoder，因此一帧一次提交，这保持了
渲染结束的确定性；没有内容的帧也提交，以带上该帧的上传。

## 测试

```text
cargo xtask test     # 整个工作区：nextest 加 doctest
cargo nextest run -p unlit_wgpu   # 只跑本 crate
```

GPU 集成测试把网格渲染到离屏纹理上，回读后与 `tests/snapshots` 下的快照用
SSIMULACRA2 感知指标比较。该目录是指向
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
submodule 的软链接；在本仓库用 `git submodule update --init --checkout` 拉取，
`--checkout` 是为了越过它的 `update = none`——那让 git 依赖方不必拉取永远用不到的快照。
若确有意改动后要重新生成快照，带 `SNAPSHOT_UPDATE=1` 运行对应测试，然后在提交前审查
图像差异。

单元测试位于 `src/` 内，覆盖私有纯逻辑；`tests/` 下的集成测试只经公开 API 使用本
crate。各测试层在整个工作区中的位置，以及 CI 所跑的内容，见
[根 README](https://github.com/beicause/unlit3d/blob/main/README.zh-CN.md#测试与基准)。

## 另见

- [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
  —— 构建在本 crate 之上的 ECS 集成 API。
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)
  —— 该 API 使用的 world。
- [`unlit_wgpu_test_util`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.zh-CN.md)
  —— 无头 GPU 测试骨架。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
