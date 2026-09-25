[English](README.md) | 简体中文

# wgpu_unlit_render

面向 WebGPU 的、紧凑且有主见的**无光照（unlit）**绘制渲染器。它在一个 render pass
内把整个场景——不透明与透明实例一视同仁——绘制到调用者指定的一个
`wgpu::TextureView`；目标平台是 WebGPU（及其背后的原生后端），并且移动端优先。
不支持 WebGL 与 GLES。

本 crate 是分层的。那些通用模块——资源追踪、顶点压缩、缓冲池、staging、声明式
场景、变体缓存——完全不知道内置管线的存在；而内置 unlit 管线正是用它们组合而成
的，方式与调用者自建管线时完全一样。

本 crate 处于**极早期开发阶段**；API 会自由变动。它不依赖 ECS 层。

## 在工作区中的位置

`wgpu_unlit_render` 是工作区的基础，不依赖工作区中的任何其他 crate。与 ECS 集成的
高层 API 是 [`unlit3d`](../unlit3d/README.zh-CN.md)，它在
本 crate 之上提供组件、帧源与 winit 呈现。测试所用的 GPU 测试骨架是
[`wgpu_unlit_test_util`](../wgpu_unlit_test_util/README.zh-CN.md)。

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
  索引 `Uint16x4`、骨骼权重 `Unorm16x4`）与 `MeshMetadata` 解码参数，以及一个
  无需把各通道实体化即可打包的顶点流写入器。
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

## 用法

本 crate 通过录制 `Scene` 来绘制；内置管线只是填充 `Scene` 的一种方式。下面省略了
device、绑定组与缓冲的创建，假设它们已就绪：

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

内置管线的完整可编译走查——设备创建、网格压缩、每一个绑定组与绘制——作为 `unlit`
模块的文档示例收录在本 crate 的文档中。`mesh`、`buffer_pool`、`offset_allocator`、
`staging` 与 `render_attachments` 的文档注释中也含有可运行示例。

若要编写自己的着色器，请用 [`wesl`](https://docs.rs/wesl) 组合它：本 crate 的
`shader` 条目是一个 WESL `StaticPackage`，因此解析器可以 import 内置管线所用的同一
批模块。

## 测试

```text
cargo xtask test     # 整个工作区：nextest 加 doctest
cargo nextest run -p wgpu_unlit_render   # 只跑本 crate
```

GPU 集成测试把网格渲染到离屏纹理上，回读后与 `tests/snapshots` 下的快照用
SSIMULACRA2 感知指标比较。该目录是指向
[`wgpu_unlit_render_asset_files`](../../wgpu_unlit_render_asset_files/README.md)
submodule 的软链接；用 `git submodule update --init` 拉取。若确有意改动后要重新生成
快照，带 `SNAPSHOT_UPDATE=1` 运行对应测试，然后在提交前审查图像差异。

## 文档

- [`docs/DESIGN.md`](../../docs/DESIGN.md) —— 设计取舍与架构。
- Crate 级文档：`cargo doc -p wgpu_unlit_render --open`。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
