[English](README.md) | 简体中文

# unlit3d

高层渲染 API：与 ECS 集成的层，每帧把一个由携带组件的实体构成的世界转换为 GPU
绘制命令。它连接 [`unlit_ecs`](../unlit_ecs/README.zh-CN.md) 与
[`wgpu_unlit_render`](../wgpu_unlit_render/README.zh-CN.md)，并把两者保留为直接
依赖——它们的条目通过各自的路径访问，大多数调用者需要的那些则由 `prelude` 重导出。

与本工作区其他部分一样，本 crate 处于**极早期开发阶段**；API 会自由变动。它紧贴
裸 `wgpu`：渲染器直接使用 `wgpu` 资源，你需要具备 WebGPU 知识才能用好它。

## 在工作区中的位置

`unlit3d` 是高层的那一半。`wgpu_unlit_render` 提供渲染器、资源图与内置 unlit 管线；
`unlit_ecs` 提供 world；本 crate 提供描述可渲染场景的组件、驱动它的帧循环结构，以及
平台胶水（winit、egui）。`wgpu_unlit_test_util` 是 dev-dependency。

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
具体管线。

`MeshSource::register_unlit_family` 注册内置 unlit 家族；`MeshSource::register_family`
注册调用者自己的家族，走的是与内置家族相同的路径。其他组件：`Transform`、`Camera`、
`RenderLoadOps`、`InstanceColor`，以及标记组件 `ZSortedDrawing`。

`ui::UiPanel` 本身就是一个持有闭包的行为组件，因此一个界面就是一个实体——一帧里可以有
与实体数量一样多的面板，而 `ui::UiSource` 驱动器会按查询顺序运行 world 中携带的所有
面板。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `ui` | 是 | `ui` 模块（作为帧源绘制的 egui 叠加层）以及它所使用的 `wgpu_unlit_render` egui 后端 |
| `winit` | 是 | `winit` 模块：`WindowSurface`，把 `Renderer` 呈现到窗口交换链；以及 `input::winit::WinitInput` |

使用 `--no-default-features` 时，本 crate 保留 ECS 组件、帧源、mesh 路径、管线抽象
与可移植的 `input` 模块——它们都不依赖 egui 或 winit。

## 用法

最小的帧骨架，假设 `device`/`queue` 与几何数据由外部提供。`spawn_context` 把 GPU
状态放进 world，`MeshSource` 作为源挂载，`Renderer` 是帧驱动器：

```rust,no_run
use unlit3d::prelude::*;
use wgpu_unlit_render::pipeline::UnlitOptions;
use wgpu_unlit_render::resources::ResourceGraph;

let mut world = LocalWorld::new();
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());

let mut mesh_source = MeshSource::new(&world, ctx);
mesh_source.register_unlit_family(&world);
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
let source = spawn_source(&mut world, mesh_source);
let renderer = world.spawn((Resource, Renderer::new(ctx)));

// Geometry and materials are allocated through the source, so they land in
// the frame's resource graph; the returned handles go on the entity.
let mesh = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        source.allocate_unlit_mesh(
            &world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        )
    })
    .unwrap();

world.spawn((Transform::default(), mesh, material, UnlitPipeline::new(key)));
```

UI 面板是行为组件，所以挂载一个界面就是一次普通的 spawn：

```rust
use unlit3d::prelude::*;

world.spawn((UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),));
```

Crate 级文档中有一个完整可编译的示例——生成上下文、分配网格与材质、绑定离屏目标并
渲染一帧。`unlit3d_examples` 是基于这套 API 的完整窗口化程序。

## 输入

`input` 是输入处理中可移植的那一半：事件类型以及响应它们的行为组件，既不依赖 winit
也不依赖 egui。一帧把事件喂进 `InputState` 资源，运行 `dispatch_input` 驱动行为组件，
然后在所有消费者都读完之后调用 `InputState::clear_events`——事件是只读遍历而非取走，
因为同一帧有不止一个消费者。来自窗口库的事件的转换放在对应 feature 之后：
`input::winit::WinitInput` 转发 `WindowEvent`，`ui::convert` 把本 crate 的事件转换为
egui 的事件。

## 测试

```text
cargo xtask test                # 整个工作区
cargo nextest run -p unlit3d    # 只跑本 crate
```

GPU 集成测试把场景渲染到离屏目标并检查回读的像素。这些场景的多帧快照覆盖现在位于
[`unlit3d_examples`](../../unlit3d_examples/README.zh-CN.md)：它的场景正是从原来这里的
测试移植而来，其无头路径用
[`unlit3d_asset_files`](../../unlit3d_asset_files/README.md)
下的 SSIMULACRA2 快照比较。用 `git submodule update --init` 拉取；确有意改动时带
`--update` 重新生成（`cargo run -p unlit3d_examples --features snapshot --
--headless --scene all --update`），并审查图像差异。

## 另见

- [`wgpu_unlit_render`](../wgpu_unlit_render/README.zh-CN.md) —— 其下的渲染器。
- [`unlit_ecs`](../unlit_ecs/README.zh-CN.md) —— 组件所在的 world。
- [`unlit3d_examples`](../../unlit3d_examples/README.zh-CN.md) —— 可运行的窗口化示例。
- [`docs/DESIGN.md`](../../docs/DESIGN.md) —— 设计文档。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
