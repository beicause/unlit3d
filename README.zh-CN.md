[English](README.md) | 简体中文

# unlit3d

[![Build](https://github.com/beicause/unlit3d/actions/workflows/ci.yml/badge.svg)](https://github.com/beicause/unlit3d/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](https://github.com/beicause/unlit3d)
[![Cargo](https://img.shields.io/crates/v/unlit3d.svg)](https://crates.io/crates/unlit3d)
[![Documentation](https://docs.rs/unlit3d/badge.svg)](https://docs.rs/unlit3d)

构建在 `wgpu` 上的紧凑、可拓展的、有主见的 3D 渲染器，以及构建在其上的
ECS 层。内置 **Lambert** 管线——由平行光、点光、聚光与多边形光给出的简单漫反射光照——
移动端优先；跑在 WebGPU 与 WebGL2 基线上。
它直接使用并暴露 `wgpu` 资源，允许底层控制和拓展，带有很少的 CPU 端高级封装。

它主要面向拥有图形渲染知识的开发者和编程智能体。与主流的面向大众用户的游戏引擎不同，
它不做高级别的封装，你需要有 WebGPU 知识才能较好地运用它。它的依赖很少，编译也快。

项目处于**极早期开发阶段**。API 会自由变动；请把每个 crate 都视为进行中的产物。

## 功能一览

- **移动端优先，且跨平台。** 渲染 pass 是显式的——通常一个，需要合成时更多——配以
  瞬态深度与多重采样纹理、压缩顶点，没有 prepass、计算着色器与阴影。同一个
  帧循环既驱动窗口，也驱动无头离屏目标、Web 示例与 Android APK。
- **跑在 WebGL2 与 WebGPU 基线设备上。** 适配器在运行时挑选：优先 WebGPU，平台只有
  WebGL2 时回退过去。WebGL2 缺少 WebGPU 基线的若干部分——没有 storage buffer、没有
  `base_vertex`——于是着色器用 `textureLoad` 读取本帧的数组，网格的顶点偏移被烘焙进
  索引；而具备完整基线的设备仍走 storage buffer 与整缓冲绑定，不额外增加 pass 状态
  切换。走哪条路由适配器自身的 downlevel 能力决定，而非编译期 feature。
- **由调用者组合的场景。** 一帧是一串 pass，每个 pass 都是普通实体，携带自己绘入的
  view；每个 pass 录制其下的帧源，这些帧源各自贡献绘制并声明自己在该 pass 中的位置。
  内置网格渲染、egui 叠加层与调用者自己的源权限完全相同。
- **精简的 ECS。** 借鉴 OOP 重视对象状态，实体原型不可变，系统由外部驱动而非内置，
  行为是持有闭包的组件而非 system，参见[unlit_ecs](./crates/unlit_ecs/README.zh-CN.md)。
  可渲染实体把网格、材质与管线作为组件携带，游戏逻辑与绘制共用同一套模型。
- **按变体组合的 lit 管线。** 着色器变体只含网格实际使用的通道——位置、UV、顶点色、
  逐实例变换与颜色、基础色纹理、蒙皮、形变目标——并针对本帧的渲染目标特化。
- **Lambert 光照，前向聚类。** 受光绘制与无光绘制是同一个变体，只差一个开关，顶点流
  与 mesh 组都不变。聚类在 CPU 上完成，着色器因此不需要计算着色器，WebGL2 也走同一条
  路径。没有阴影。机制见
  [`unlit_wgpu` 的「光照」一节](crates/unlit_wgpu/README.zh-CN.md#光照前向聚簇的-lambert)。
- **自定义着色器与逐实例数据。** 调用者自己的家族通过与内置家族同一个调用注册，用自己的
  管线绘制——手写的 `wgpu` 管线，或用本库公开着色器包组合出的 WESL 模块——并拥有
  自己的逐实例顶点流，由内置 lit 家族所实现的同一个 `InstanceData` trait 描述。
- **可选的 glTF 加载。** `unlit3d` 的 `gltf` feature 把一个 glTF 2.0 文档加载为纯粹的
  模型记录——没有 `World`，没有 GPU 状态——并把它的图像、材质与网格修补进你已在渲染
  的世界的资源图，再卸载它们，并生成绘制文档默认场景的实体。节点层级、蒙皮与形变
  primitive、混合或裁剪材质、文档中的动画——在你选定的时间采样——以及
  `KHR_draco_mesh_compression` 压缩的 primitive，都会一并带过来。
  相机不会：渲染器用世界自己的 `Camera` 绘制，由调用方摆放。
- **常驻且池化的 GPU 资源。** 直接使用 `wgpu` 资源；跨帧保留，按需重建，并共享复用
  上传所经过的缓冲。
- **CPU 视锥剔除与自动实例化。** 屏幕外的网格不产生开销，状态相同的绘制折叠为一次
  实例化绘制。
- **egui 与可移植输入。** egui 可叠加于渲染之上，也可渲染到单独纹理；输入统一，由
  类似 OOP 风格的回调驱动。
- **确定性渲染，CI 快照测试。** 同一场景离屏渲染的结果每次都完全相同。CI 会把示例的
  每个场景渲染出来并与存储图像做 SSIMULACRA2 比对；两档 job 的划分见
  [持续集成](TESTING.zh-CN.md#持续集成)。
- **内置 MCP 支持。** 可将 MCP 服务器嵌入应用程序，让 AI Agent 读写组件、截图。服务器不携带
  任何引擎知识：每个 crate 各自注册作用于它所定义类型的工具。

## Crate 一览

| Crate | 职责 |
|-------|------|
| [`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) | 底层渲染器：资源图、顶点压缩、缓冲池、staging、声明式 `Scene`、内置 lit 管线、egui 后端。不认识 ECS。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 高层渲染 API：ECS 组件、帧源、输入、UI 与 winit 呈现，以及可选的 glTF 加载器，把模型修补进另一个世界的 `MeshSource`。 |
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | 高层所用的精简 archetype ECS：没有变化检测、事件、关系或调度器。它还带有把运行中的 world 经 stdio 暴露出去的 MCP 服务器，各 crate 各自为它扩展工具。 |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.zh-CN.md) | 无头 GPU 测试骨架：设备初始化、缓冲与纹理回读、SSIMULACRA2 快照。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 可切换场景的窗口化示例，其 `tests/gpu_suite.rs` 把场景与快照比较；也是 Android APK 所打包的两个示例之一。 |
| [`unlit3d_asset_bytes`](unlit3d_examples/asset_bytes/README.zh-CN.md) | `unlit3d_examples` 所绘制的模型，随身带在二进制里：`include_bytes!` 写在 `#[inline(never)]` 的函数体内，这些字节因此不进任何下游的元数据。 |
| [`unlit3dx_asset_bytes`](unlit3dx_examples/asset_bytes/README.zh-CN.md) | `unlit3dx_examples` 所演奏的音色库，同样随身携带；文件缺失时其 `build.rs` 换上一个空的 stub，而在 `UNLIT3D_REQUIRE_SOUNDFONT` 要求真货时直接失败。 |
| [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) | 把 glTF 文档渲染成图片的命令行工具，由 JSON 配置文件驱动、并可被自身选项覆盖；其 `tests/gpu_cli.rs` 是端到端快照检查。 |
| [`unlit3dx`](crates/unlit3dx/README.zh-CN.md) | 多 world 运行时：job 单向发出、订阅把状态带回来——状态用快照、事件用流——连接渲染子 world 与物理、音频、玩法 world，各占一条线程或由宿主的帧步进。 |
| [`xtask`](xtask/README.zh-CN.md) | `cargo xtask` 背后的任务执行器。 |
| [`unlit3d_benchmarks`](unlit3d_benchmarks/README.zh-CN.md) | 帧路径的基准测试：Criterion 吞吐量数字，以及 `profiling` 跨度报告的阶段耗时。 |

## 各部分的配合方式

`unlit_wgpu` 是基础，不依赖工作区中的任何其他 crate。`unlit3d` 构建在它和
`unlit_ecs` 之上，并把两者保留为直接依赖：`prelude` 重导出大多数调用者需要的条目，
其余条目仍可通过各自的 crate 路径访问。

两条原则决定了这种分层：**内置管线不享有特权**，它由调用者自建管线所用的同一批公开
设施组合而成；**帧源彼此平等**，内置网格源与调用者自己的绘制趟次没有任何差别。

各 crate 的 README 记述它自己那部分的设计取舍：
[`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) 讲资源图、声明式场景、管线特化与
逐帧上传；[`unlit3d`](crates/unlit3d/README.zh-CN.md) 讲帧模型、lit 管线、UI 与
输入；[`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) 讲 ECS 为何如此精简。

## 编码原则

- **优先用通用机制，不给内置功能特权。** 设计面向通用性，让用户可以自定义与拓展本库；
  内置实现（如 lit 渲染）不应有特权或内部专用路径，内部实现应向外移，以保持可定制与可拓展。
- **在出问题的那一处绕开后端 bug，并写下原因。** 驱动把合法代码编错时，就在绊到它的代码处
  绕开，把症状、成因与代价记进 [`WORKAROUND.zh-CN.md`](WORKAROUND.zh-CN.md)，
  让后来的人能分清那是刻意的怪写法还是笔误。

`cargo xtask` 的各任务及其参数见 [`xtask/README.zh-CN.md`](xtask/README.zh-CN.md)。

## 测试

测试分哪几层、怎么运行、各 crate 覆盖什么、基准测试，以及运行它们的 CI workflow，
都见 [`TESTING.zh-CN.md`](TESTING.zh-CN.md)。

## 工作区结构

```text
crates/unlit_wgpu/           渲染器、它的 WESL 着色器与 GPU 测试
crates/unlit3d/              与 ECS 集成的渲染 API
crates/unlit_ecs/            archetype ECS
crates/unlit_wgpu_test_util/ 共享的 GPU 测试骨架
crates/unlit3dx/             多 world 运行时：job、快照、线程化与步进的 world
unlit3d_examples/            窗口化示例、它的场景与场景的快照测试
  asset_bytes/               它嵌入的 glTF 模型，单独成 crate
unlit3d_examples_android/    Android activity 所加载的动态库
unlit3dx_examples/           多 world 示例：五个 world、一个窗口、一个 web 页面与一条 Android activity
  asset_bytes/               它嵌入的音色库，单独成 crate
unlit3dx_examples_android/   该示例供自己的 activity 加载的动态库
unlit3d_cli/                 glTF 转图片的命令行工具与端到端测试
unlit3d_benchmarks/          帧路径基准测试与剖析运行
android/                     把两个示例打包成 APK 的 Gradle 工程
xtask/                       cargo xtask 任务执行器
```

## 许可证

双许可：MIT 或 Apache-2.0，任选其一，如工作区 manifest 所声明。
