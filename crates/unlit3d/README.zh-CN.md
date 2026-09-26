[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md) | 简体中文

# unlit3d

高层渲染 API：与 ECS 集成的层，每帧把一个由携带组件的实体构成的世界转换为 GPU
绘制命令。它连接 `unlit_ecs` 与 `unlit_wgpu`，并把两者保留为直接依赖——它们的条目
通过各自的路径访问，大多数调用者需要的那些则由 `prelude` 重导出。

与本工作区其他部分一样，本 crate 处于**极早期开发阶段**；API 会自由变动。它紧贴
裸 `wgpu`：渲染器直接使用 `wgpu` 资源，你需要具备 WebGPU 知识才能用好它。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `ui` | 是 | `ui` 模块（作为帧源绘制的 egui 叠加层）以及它所使用的 `unlit_wgpu` egui 后端 |
| `winit` | 是 | `winit` 模块：`WindowSurface`，把 `Renderer` 呈现到窗口交换链；以及 winit 输入转发 |

使用 `--no-default-features` 时，本 crate 保留 ECS 组件、帧源、mesh 路径、管线抽象
与可移植的 `input` 模块——它们都不依赖 egui 或 winit。

## 帧模型

一帧**不是**「一个主场景加若干附加物」。`Renderer` 自身不绘制任何东西：它持有该帧
的渲染目标，并按各帧源通过 `FrameOrder` 声明的顺序录制它们。

- 每个源在 `build_scene` 中构建自己的 `Scene`，随后渲染器按序把所有场景录制进一个
  在目标附件之上开启的 pass。因此一帧就是一个 encoder、一次提交。
- 内置的 mesh 渲染就是其中一个源 `MeshSource`，它不比调用者自己的源享有更多特权。
  渲染器里没有任何 mesh 专用的字段或绘制路径。
- GPU 状态——`wgpu::Device`、`wgpu::Queue` 与 `ResourceGraph`——作为资源组件存在于
  world 中，通过 `RenderContext` 寻址。`spawn_context` 负责生成它们并返回地址。
- **构建与录制是两个阶段。** 源在 `build_scene` 期间往图里注册资源并暂存上传；录制
  阶段只读取已经产出的场景，因此源自身状态与资源图之间永远不会产生借用冲突。
- 源自己的 GPU 资源由源释放；`despawn_source` 会在销毁实体前把释放排队，因为资源图
  无法察觉到实体消失。

## 组件

一个可渲染实体携带 `GpuMesh`、`GpuMaterial` 与 `GpuPipeline`。`GpuPipeline` 携带的
是 *key* 而不是已编译的管线：某个实体需要哪条具体管线，取决于该帧的渲染目标与网格的
顶点布局，而这两者在生成实体时都不知道。*家族（family）* 弥合了这个缺口——它把
`Variants` 缓存与 `Specializer`、`PipelineFactory` 配在一起，每帧把一个 key 解析为
一条具体管线。

`MeshSource::register_unlit_family` 注册内置的 unlit 家族；`MeshSource::register_family`
注册调用者自己的家族，这与内置家族走的是同一条路。其他组件包括 `Transform`、
`Camera`、`RenderLoadOps`、`InstanceColor` 以及 `ZSortedDrawing` 标记。

`ui::UiPanel` 本身就是一个持有闭包的行为组件，所以一个界面就是一个实体——一帧可以有
与实体数量相同的面板，`ui::UiSource` 驱动器会按查询顺序运行 world 携带的所有面板。

## 示例

下面的帧骨架假设 `device`/`queue` 由外部提供。`spawn_context` 把 GPU 状态放进
world，`MeshSource` 作为源挂载，`Renderer` 是帧驱动器：

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::resources::ResourceGraph;

let (device, queue) =
    wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
let mut world = LocalWorld::new();

// 1. Spawn the frame's GPU context, the built-in mesh source and the
//    frame driver, and register the built-in unlit family.
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
let mut mesh_source = MeshSource::new(&world, ctx);
mesh_source.register_unlit_family(&world);
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
let source = spawn_source(&mut world, mesh_source);
let renderer = world.spawn((Renderer::new(ctx),));

// 2. Allocate geometry and a material through the mesh source.
let (mesh, material) = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        let positions = [[0.0; 3]; 3];
        let uvs = [[0.0; 2]; 3];
        let colors = [[255u8; 4]; 3];
        let indices = [0u32, 1, 2];
        let mesh = source.allocate_unlit_mesh(
            &world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        );
        let device = source.device(&world);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("example::texture"),
            size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // The material reads a view of the texture and a sampler, both of
        // which the graph keeps for it: only ids cross the boundary.
        let view = source.register_texture_and_default_view(&world, texture).1;
        let sampler = source.register_sampler(&world, None);
        let material = source.allocate_unlit_material(&world, &key, view, sampler);
        (mesh, material)
    })
    .unwrap();

// 3. Spawn a renderable entity, carrying the unlit key.
world.spawn((
    Transform::default(),
    mesh,
    material.unwrap(),
    UnlitPipeline::new(key),
));

// 4. Bind a render target from the frame's resource graph and render one
//    frame (uses the noop device, so it produces a valid command buffer
//    without touching a GPU).
let ft = create_render_target(
    &world.get::<wgpu::Device>(ctx.device).unwrap(),
    wgpu::TextureFormat::Rgba8UnormSrgb,
    1280, 720, 1,
);
let (color_view, depth_view) = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        let color_view = source.register_texture_and_default_view(&world, ft.color).1;
        let depth_view = MeshSource::graph(&world, ctx)
            .insert_strong(
                ft.depth.create_view(&wgpu::TextureViewDescriptor::default()),
                &[],
            )
            .unwrap();
        (color_view, depth_view)
    })
    .unwrap();
world
    .with_mut::<Renderer, _>(renderer, |r| {
        r.set_render_target(&world, Some(color_view), Some(depth_view), None);
        r.render(&world);
    })
    .unwrap();
```

UI 面板是行为组件，所以挂载一个界面就是一次普通的 spawn：

```rust
# #[cfg(feature = "ui")]
# {
use unlit3d::prelude::*;

let mut world = LocalWorld::new();
world.spawn((UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),));
# }
```

## 输入

`input` 是输入处理中可移植的那一半：事件类型以及响应它们的行为组件，既不依赖 winit
也不依赖 egui。一帧把事件送入 `InputState` 资源，运行 `dispatch_input` 驱动这些行为，
然后在所有消费者都读过之后调用 `InputState::clear_events`——事件只被只读遍历，从不被
取走，因为同一帧可能有多个消费者。来自窗口库的事件转换位于相应 feature 之后：
`input::winit::WinitInput` 转发 `WindowEvent`，`ui::convert` 把本 crate 的事件转成
egui 的事件。

## 测试

```text
cargo xtask test                # 整个工作区
cargo nextest run -p unlit3d    # 只跑本 crate
```

GPU 集成测试把场景渲染到离屏目标并检查回读的像素。这些场景的多帧快照覆盖现在位于
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)：
它的无头路径把这些场景与
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
下的 SSIMULACRA2 快照比较。用 `git submodule update --init` 拉取 submodule；有意改动
后用 `cargo run -p unlit3d_examples --features snapshot -- --headless --scene all
--update` 重新生成，并审查图像差异。

## 另见

- [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md)
  —— 下层渲染器。
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)
  —— 组件所在的 world。
- [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)
  —— 基于这套 API 的可运行窗口程序。
- [设计文档](https://github.com/beicause/unlit3d/blob/main/docs/DESIGN.md)
  —— 架构与设计取舍。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
