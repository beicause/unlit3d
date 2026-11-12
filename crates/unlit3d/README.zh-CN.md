[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.md) | 简体中文

# unlit3d

高层渲染 API：与 ECS 集成的层，每帧把一个由携带组件的实体构成的世界转换为 GPU
绘制命令，由本帧的光照着色。它连接 `unlit_ecs` 与 `unlit_wgpu`，并把两者保留为
直接依赖——它们的条目通过各自的路径访问，大多数调用者需要的那些则由 `prelude` 重导出。

与本工作区其他部分一样，本 crate 处于**极早期开发阶段**；API 会自由变动。它紧贴
裸 `wgpu`：渲染器直接使用 `wgpu` 资源，你需要具备 WebGPU 知识才能用好它。

## 功能一览

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `ui` | 是 | `ui` 模块（作为帧源绘制的 egui 叠加层）以及它所使用的 `unlit_wgpu` egui 后端 |
| `winit` | 是 | `winit` 模块：`WindowSurface`，把 `Renderer` 呈现到窗口交换链；`presenter`，建在它之上的现成窗口循环；winit 输入转发；以及 `WinitHost`，把 winit 的回调作为行为组件驱动 |
| `gltf` | 否 | `gltf` 模块：把一个 glTF 文档（含 `KHR_draco_mesh_compression` 压缩的 primitive）加载为 `LitGltf`，把它的图像、材质与网格修补进另一个世界的 `MeshSource`，并生成绘制它们的实体 |
| `reflect` | 否 | 给跨越 JSON 边界的组件派生 [`facet`](https://docs.rs/facet) 反射，以及 glam 与 ECS 值所经由的代理类型；引擎自身从不读取它 |
| `mcp` | 否 | `mcp` 模块：world 通过 [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md) crate 所定义的 Model Context Protocol 服务器回答的渲染工具。蕴含 `reflect` 与 `gltf`，且不在 `wasm32` 上构建 |

使用 `--no-default-features` 时，本 crate 保留 ECS 组件、帧源、mesh 路径、管线抽象
与可移植的 `input` 模块——它们都不依赖 egui 或 winit。开启 `gltf` 则在同一套 ECS 与
mesh 路径之上增加 glTF 加载器，见下文。开启 `reflect` 会派生 JSON 桥所需的反射，引擎
其余部分不变。

## 这一层的定位

它把自身视作游戏引擎的一部分，因此要便于与其他功能（如物理、音频）集成，并便于拓展：

- 拓展性与移植性优先于便利：公开 API 优先使用 `dyn trait` 而不是静态分发，并在一定
  程度上考虑多线程，而不是假定它不存在。
- 常见路径要好用：CPU 侧场景数据同步至 GPU 缓冲并渲染，调用方无需管理上传。
- 底层控制仍然可用：调用者能绕过 CPU 数据直接更新 GPU 缓冲。
- 剔除采用 CPU 视锥剔除。

### 其他引擎的做法

<details>
<summary>为什么不用带变化检测与渲染提取的 ECS，也不用 OOP 场景树</summary>

**Bevy**，采用 ECS 架构。好的地方：模块化程度高，插件的拓展性好，ECS 能充分利用多
线程。不好的地方主要来自 ECS 的缺点和复杂性：组件之间的提取和衍生易出错，系统与系统之间耦合，继承和
复用不够明显，复杂对象状态管理困难，场景图用组件表示不如直接用树数据结构和对象表示
直观。

1. 组件变化检测机制复杂、易失效、易出错，组件依赖更新和派生机制几乎没有。如果每帧
   同步而不保留组件，开销可能较大；而如果保留组件，则涉及复杂的变化检测和依赖更新
   问题。例：`bevy_render` 的资源提取模式易出错，尤其是在组件增删时，容易泄漏衍生
   组件，容易漏掉更新或使变化检测失效导致过度更新。复杂性体现在 bevy 的渲染系统往往
   很庞大，可能要处理大量组件，并带有大量 `Changed`、`RemovedComponents` 查询。而在
   OOP 中，对象自身状态在内部维护。
2. 拓展性体现在系统的相加上，而不在组件上，组件缺乏 OOP 的继承性。例：bevy 的材质
   拓展需要用户为每个自定义材质注册一个插件，较繁琐。bevy 的材质所需的 GPU 句柄往往
   绑死了 `ShaderBuffer`、`Mesh`、`Image` 等类型的 `Handle`，用户无法自定义自己的资源
   句柄用于材质。而这在 OOP 中，用基类并拓展非常容易。

由于渲染资源类型单一，bevy 的 `RenderAssetUsages` 机制也易出错：主世界被提取了渲染
资源的数据后，再访问它就会报错。

**Godot**，采用 OOP、场景树和中心化渲染器。好的地方：易用性好，
Godot 的场景树轻松可以拆分和组合；API 可以非常高层（如 Godot 的节点树），也可以比较
底层（如 Godot 的服务器模式）。不好的地方：

1. 对于 Rust 来说，OOP 和继承不方便，强行模拟 OOP 很别扭。
2. 不擅长处理大量实体，遍历节点树开销较高。虽然可以使用 RenderingServer 绕过节点树，但理论上对于大量实体还是 ECS 的紧密储存性能更好。
3. 不像 ECS 那样自动和充分利用并行。并行是粗粒度的，需要手动调整：Godot 的并行体现
   在服务器、节点的每帧更新逻辑可以选择后台线程，服务器和节点内部逻辑不能并行，除非
   手动调用线程池。
4. 层层封装导致灵活性、可自定义性差。自定义后处理靠屏幕读取着色器或合成器钩子不友好。自定义整个渲染管线更是没办法。

</details>

## 帧模型

一帧**不是**「一个主场景加若干附加物」，也不是一个 pass：它是一串 pass。
`Renderer` 自身不绘制任何东西：它按 `FramePassOrder` 遍历 world 里的 `FramePass`
实体，在每个 pass 内按各帧源通过 `FrameSourceOrder` 声明的顺序录制其下的源。

渲染 world 就是一棵普通的实体树，而树本身**就是**帧的形状：一个 scope 看得见它扎根
的实体、其下的一切，以及 shared 子树，但看不见祖先，也看不见兄弟。

```text
world root                              根 scope 看得见所有实体
├── shared                              每个 scope 都看得见这棵子树
│   ├── wgpu::Device        + RenderContext
│   ├── wgpu::Queue         + RenderContext
│   ├── ResourceGraph       + RenderContext    所有纹理、缓冲、bind group
│   ├── DeviceCapabilities  + RenderContext
│   ├── FrameClock                             每帧推进一次，先于任何行为
│   ├── OpenUrls
│   └── ... 面板读写的应用状态
├── FramePass(FramePassOrder(0)) + RenderLoadOps
│   ├── Source(MeshSource)                    源自身也是一个 scope
│   │   ├── Camera
│   │   ├── Transform + GpuMesh + GpuMaterial + LitPipeline
│   │   └── ...
│   ├── Source(LineSource)                    笔画；混合的与网格按深度合成
│   └── Source(UiSource)
│       └── UiPanel
├── FramePass(FramePassOrder(1)) + RenderLoadOps
│   └── Source(BlitSource)                    采样上面那个 pass 的离屏目标
└── Renderer                                  自身不绘制，只遍历各个 pass
```

把树当成帧来读：渲染器按顺序遍历世界里的 `FramePass`，对每个 pass 再录制**它自己那个
scope** 下的 `Source`——因此源在树里的位置就决定了它进哪个 pass，同一个 pass 下的
两个源各画各的，谁也不必看见另一个。pass 的 scope 够得到其下的一切，嵌套的 pass 也
在内，因此嵌套 pass 里的源会被两个 pass 都够到：它按 pass 各构建一次，填进各自那份
场景。挂在 shared 子树里的源会被每个 pass 够到，这正是把一个覆盖层或全屏效果画进
每个目标、而不必在树里重复一遍的做法。没有被任何 pass 够到的源会被报告出来，且什么
也不画。GPU 状态放在 `shared` 而不是渲染器身上，于是任何源都能从自己的 scope 读到
它，不用往上爬树。pass 是源的**祖先**，而 scope 够不到祖先，这正是 `PassInfo` 存在
的原因：由 pass 告诉它的源画进什么目标。附件则完全不是
树节点——pass 只是在 `ResourceGraph` 里指名 view，所以它每帧都能换一张新的交换链
图像而树不变。

- pass 就是普通实体：它携带自己绘入的附件、开启时使用的 `RenderLoadOps`，以及场景
  被裁剪到的视口。视口是 pass 状态，所以渲染器在每个场景之前把它设回整个目标；场景的
  绘制所设置的裁剪矩形同样是 pass 状态，会一直留到 pass 结束，因此渲染器在录完所有源
  之后用整个目标盖掉它。否则一个以带裁剪的绘制收尾的 pass，在 GL 后端上只会解析那一
  小块——那里的多重采样解析是一次 blit，而 blit 会受裁剪矩形限制。其下的源就是它录制
  的源，因此一个 world 可以按帧的需要放任意多个 pass——一个画进离屏目标的 pass 与一个
  把该目标贴到交换链的 pass 是同一个 world 里的两个实体，而不是两个 world。
- pass 用自己实体的名字录制：wgpu 把它当作这个 pass 录制期间所报校验错误的标签打印出
  来，因此错误会指名出错的那个 pass，而不是帧里所有 pass 共用的一个常量。名字由
  `ScopeMut::spawn_named` 给出，否则用 bundle 自身类型的名字；实体没有名字时按
  `unlit3d::pass` 录制。
- pass 只指名 view，不拥有它们。附件来自 `FrameAttachments`、`WindowSurface` 或
  `PersistentTarget`，它们都在同一个 world 里，且句柄保持稳定，因此一个 pass 可以每帧
  换一个新的颜色 view 而不触发其下源的重建。交换链的颜色 view 每帧都变，而常驻目标的
  颜色 view 是它自己的纹理、只在 resize 时变——正因如此，截图才能在呈现之后回读这一帧。
- 每个源在 `build_scene` 中填充一个 `Scene`，随后渲染器把该场景录制进以这个 pass
  自身附件开启的那趟 pass。因此无论一帧有多少个 pass，它仍是一个 encoder、一次提交。
- 内置的 mesh 渲染就是其中一个源 `MeshSource`，它不比调用者自己的源享有更多特权：内置
  lit 家族通过与调用者家族相同的 `MeshSource::register_family` 注册，一切专属于它的
  东西——它的 key、实例记录、mesh 与材质辅助方法——都住在 `lit` 模块里。
  渲染器里没有任何 mesh 专用的字段或绘制路径。
- 共享源池子的 mesh 一律经由 `MeshSource::allocate_pooled_mesh` 分配，内置家族用的
  也是它：调用者声明自己的 mesh 占用哪几条流并填充它们，源负责分配顶点与索引区段、
  在设备没有 `base_vertex` 时把池偏移烘进索引，并交回一个完整的 `GpuMesh`。
  `MeshSource::allocate_lit_mesh` 是内置着色器对它的使用——由通道推导出流布局，再用
  `unlit_wgpu::mesh` 里的写入器打包。整块缓冲自有的 mesh（例如 UI 路径）改用
  `MeshSource::allocate_mesh` 分配。
- GPU 状态——`wgpu::Device`、`wgpu::Queue`、`ResourceGraph` 以及设备的那些事实——
  作为资源组件存在于 world 中，通过 `RenderContext` 寻址。`spawn_context` 把它们当作
  一个 `RenderDevice`（device、queue、`DeviceCapabilities` 与 `DeviceHealth` 一起）
  接收，生成后返回地址；`RenderContext::of` 可以从它四个实体中的任意一个反查回整个
  上下文。能力随帧一起传递，因为调用者是拿适配器推导出来的：设备无法自述的信息不该让
  每个源各自重新发现。
- **构建与录制是两个阶段。** 源在 `build_scene` 期间往图里注册资源并暂存上传；录制
  阶段只读取已经产出的场景。合并两者就必须在场景产出之前读资源图，只剩内部可变性把
  借用冲突推到运行期，因此保留两阶段以换取编译期保证。
- **每个源在自己的构建里整理一次资源图。** `replace` 只做标记；每个源在上传之后、
  读回刚重建的绑定组之前维护一次，在自己这一个点上回收已无任何持有的资源并按依赖序重建
  脏的绑定组。源用 `RenderContext::maintain_scope` 指出这个时机：它返回一个在 drop 时
  维护资源图的守卫，因此「无物可画」时的每一处提前 return 都会自动维护，无需记住任何调用；
  源在组装帧的位置显式丢弃该守卫。同一帧的多个源共享同一张图，因此一帧可能整理多次——
  维护是幂等的，且无事可重建的那次会直接跳过重建这一趟。不绘制任何东西的帧同样会整理；
  场景之外的路径——交换链尺寸变化、acquire——则显式维护，而不是把旧资源留到下一次场景构建。
- 源的 GPU 资源恰好在还有句柄指名它时存活，因此销毁源实体即丢弃其句柄，下一次
  `maintain` 回收余下部分。无需记住任何释放调用。

**源自带状态，场景归帧所有。** egui 的 `Context`、字体图集、UI 自己的 UBO 都归 UI 源
所有；3D 的管线、网格池、元数据、剔除缓存都归 mesh 源所有。`Scene` 则属于某个
*(pass, source)* 对，由渲染器持有：它是本帧的临时产物，而不是源的状态。一个被多个 pass
看见的源会按 pass 各构建一次，各自填进那个 pass 自己的场景，于是每个 pass 只录制自己
目标所要求的绘制。这些槽位跨帧保留以便稳定场景复用分配，而世界已够不到的配对会在帧末
交还缓冲。按目标保留 GPU 状态的源——UI 源的管线与材质表、blit 源的绑定组——以
`SurfaceKey` 为键，于是交替的 pass 各自复用条目，而不是每帧重建。「3D 主场景」只是
第一个被录制的源产出的场景，而不是渲染器的特例。

<details>
<summary>为什么上下文与源是 world 的成员，而不是渲染器的字段</summary>

这不是「把渲染器内部拆开」，而是让帧源与第三方源完全同权：新增或移除一个源就是
spawn/despawn 一个实体，源在自己的构建阶段直接取用共享上下文，不需要经过渲染器转交。

- 资源引用就是实体引用，靠调用者保存的 `Entity` 句柄取用。
- 由此帧级上下文的传递是「按实体引用取用」，而不是「把借用句柄层层递进」，源之间也就
  不会因为共用一个上下文而产生借用冲突。
- **反向的取舍**：若把上下文私有在渲染器里，源想同时持有「自己的可变借用」与「资源图
  的可变借用」就只剩两条路——把渲染器拆成专用入口，或用内部可变性把冲突推到运行期
  panic。前者让每个调用点都长出一个绕过借用检查的函数，后者用崩溃换方便。让上下文与
  源都成为世界的成员，这个冲突根本不产生，因此不需要任何为此设计的专用入口。

</details>

<details>
<summary>为什么顺序由每个源显式声明，而不是靠创建顺序</summary>

顺序是必需方法，且 `FrameSourceOrder` 不实现 `Default`。若给出默认值，「忘记声明顺序」与
「确实想沿用创建顺序」就无法区分，顺序意图又变回隐式。顺序相同的源会记录并发出警告；
源的顺序必须是源显式携带的序号，不能借用实体在原型内的行序：实体的行序不保证稳定，
移除组件时会被打乱。

单个场景之内，源产出的顺序（它的可见条目、它的实例 run）不变。源之间的
顺序不是被 pass 级状态逼出来的硬约束——`Scene` 已携带必要的 pass 状态信息，在录制时
都会重设 pass 状态——但「UI 要合成在 3D 之上」等类似语义不再只靠它：
[`Scene::record_composited`](unlit_wgpu::scene::Scene::record_composited) 把各源的
场景合成每 pass 一份绘制清单——按场景顺序先录顺次绘制，再合并所有场景的深度排序池，
最后是压尾绘制——由各源以
[`DrawOrder`](unlit_wgpu::scene::DrawOrder) 标注。

</details>

## 组件

一个可渲染实体携带 `GpuMesh`、`GpuMaterial` 与 `GpuRenderPipeline`。`GpuRenderPipeline` 携带的
是 *key* 而不是已编译的管线：某个实体需要哪条具体管线，取决于该帧的渲染目标、网格的
顶点布局，以及——对 strip 拓扑而言，其管线必须声明所绑定索引缓冲的宽度——网格的索引
格式；这些在生成实体时都不知道。*家族（family）* 弥合了这个缺口——它持有一个
`Variants` 缓存与一个 `RenderPipelineFactory`，从绘制本身推导出每个绘制的变体，每帧把它
解析为一条具体管线。

`GpuMesh` 刻意做得很小：它只保留剔除与解析时逐实体遍历会读的字段，绘制要绑定的缓冲区
则放在共享的 `MeshParts` 句柄之后。剔除与解析会访问每个实体，而列是连续内存，遍历时会把
整条 cache line 拖进缓存，可实际只用到剔除与解析的那几个字段；若把缓冲区内联，就等于为了
几个字节的包围盒而让每次遍历都把它们拖一遍。拆分的依据是读取频率而不是字段类别，所以新增
字段应放在读取它的那一侧。

`MeshSource::register_lit_family` 注册内置的 lit 家族；`MeshSource::register_family`
注册调用者自己的家族，这与内置家族走的是同一条路——连家族自己拥有并声明的逐实例
顶点流也不例外（见[自定义着色器与逐实例数据](#自定义着色器与逐实例数据)）。其他组件
包括 `Transform`、`Camera`、`RenderLoadOps`、`lit::InstanceColor`、
`lit::InstanceCutoff`、`lit::InstanceHullWidth`、`InstanceVisibility` 以及
`ZSortedDrawing` 标记。

带 `InstanceVisibility` 且 `visible` 为 `false` 的实体在剔除之前就被跳过，因此不会为它
特化变体、打包实例记录或登记句柄；不带该组件的实体照常绘制，其 `Default` 也是绘制。
该标志是逐实体而非逐网格的：一份 `GpuMesh` 通常被多个实体绘制，隐藏其中一个不能连带隐藏
其余；它是字段而不是「组件缺失」，因为实体的组件集合在生成时就已固定，靠移除组件来隐藏
意味着要销毁重建，而不是翻转一个标志。

带 `ZSortedDrawing` 的实体会在不透明实体之后绘制，并按「网格包围盒中心沿相机视线轴的
深度」由远及近排序——见 `Camera::view_depth`——因此枢轴点偏离几何体的网格仍按它实际
绘制的几何体排序，而不是按实体原点。这一顺序是 pass 的合成，不是源的：该条目加入
所有源共同贡献的深度排序池，与混合网格、深度排序笔画一起分层。

source 通过它所在 pass 指定的相机绘制：[`FramePass::with_camera`](pass::FramePass::with_camera)
指名相机实体，pass 的每个 source 都会经 `PassInfo::camera` 拿到它——整条 pass 一句话，
分屏就是两趟 pass 各配一个相机。pass 没指名时才回退到该 source **自己 scope 内**第一个
**active** 的 `Camera`——`Camera::active` 为 `false` 的实体会被跳过，而「第一个」取决于
世界当前的行序，所以在意取景的调用者应当在 pass 上指名。因为 scope 就是该 source 自己的
子树，一个世界可以持有多个 source，各自取景自己拥有的内容；同一个 scope 内也可以有多个
相机，逐帧切换该标志即可在它们之间切换。pass 没指名、scope 内也没有 active 相机时，
该 source 被清空，什么都不绘制。

相机采用引擎自己的约定：**右手系**、**Y 轴向上**的视图空间，投影到 WebGPU 的
**`[0, 1]`** 裁剪空间深度范围，并以 **reverse-Z** 绘制。相机把视图与投影分开保存：
`Camera::view_from_world` 与 `Camera::clip_from_view`。调用者可以用任意投影组合
它们——`glam::camera::rh` 同时提供透视与正交构造——再由 `Camera::clip_from_world`
合并成顶点阶段所需的矩阵。眼点不再单独存储，而是 `Camera::position`，即视图到世界
矩阵的平移。reverse-Z 正是内置管线 `Greater` 深度比较与深度清零 `0.0` 所期望的；
`Camera::view_depth` 读的不是它，因为该方法直接从视图矩阵取深度：正交相机的
z-sorted 绘制排序与透视相机完全一致。

<details>
<summary>为什么实体与管线缓存之间要隔一层家族</summary>

家族把「实体要什么」接到 `unlit_wgpu` 的变体缓存上：

- `RenderPipelineKey` 既定位家族（按 key 类型注册与查找），又把一次绘制解析成所需的变体
  （`variant`）。key 随组件走，一个家族因此能服务策略不同的实体；又因为 key 就是组件
  本身，渲染器不必对外发放家族句柄。
- 变体是从**绘制本身**推导的，而不是事先声明：`DrawContext` 把本帧的渲染目标、网格的
  顶点布局与其索引格式交给 key，key 再把它们各归其位——目标进 `LitVariant::surface`，
  顶点布局进 `channels`，索引格式（与拓扑）进 `strip_index_format`。索引格式之所以重要，
  是因为 strip 拓扑的管线必须声明其绘制所绑定的索引宽度，而只有网格知道这个宽度——格式
  由 source 挑：取该网格顶点数能容纳的最窄格式，并在没有 `base_vertex` 的设备上烘入池
  偏移时把它加宽。**按什么维度特化因此是家族的自由**：内置 lit 按其策略加上目标、顶点
  布局与索引格式特化，自定义家族可以是任何东西。
- 蓝图**延迟求值**：只在缓存未命中、真正要编译时才向 key 索取变体。
- 家族编译出管线后，由 `RenderPipelineFactory` 把它转成渲染器要注册的
  `RegisteredRenderPipeline`——已编译管线与它全局组的配方。它与 `unlit_wgpu` 的
  `RenderPipelineDesc` 是一对**输出/输入**，两者之间隔着一次编译。

`unlit3d` 只处理渲染管线，故其管线类型一律显式带 `Render` 字样（`GpuRenderPipeline`、
`RenderPipelineKey`、`RenderPipelineId`、`RenderPipelineFactory`、
`RegisteredRenderPipeline`）。

</details>

## 自动实例化

同一档位之内，条目先被排序，使相邻者共用管线与绑定组；随后，绘制状态完全相等的连续条目——同管线、
同绑定组、同缓冲与几何段——折叠为一次实例化 draw，实例范围按可见顺序覆盖整段。录制
一次 draw 要花一条命令和一次状态重绑，所以与前一实体共用网格的实体几乎是免费的。

<details>
<summary>为什么合并不透明绘制总是安全的，而 z-sorted 的永远不行</summary>

实例范围是合并后仍正确的原因：逐实例步进属性按实例的序数取，而每个家族的实例流
都按可见顺序打包，因此 `a..b` 这些实例读到的正是分开绘制时会读到的那些记录。
逐实例数据——内置家族的变换、基础色、关节与形变基址、裁剪值、元数据索引——都在这个
流里，所以合并不改变任何实例读到的数据。一段 run 也绝不会跨两个家族：每个家族只有
一条流，而它解析出的管线只属于该家族。

不透明绘制开启深度测试且不混合，其顺序不可观测，因此合并是安全的。透明（z-sorted）
条目永远不合并，彼此之间不合并，与任何其他条目也不合并：它们按从后到前的顺序混合，
绘制顺序**就是**结果，合并后的 draw 会改为按记录顺序光栅化各实例。它们的顺序来自每个
网格包围盒中心沿相机视线轴的深度，因此跟随网格实际绘制的几何体，而不是它的枢轴点或到
眼睛的直线距离。源不再自行排序：条目携带
[`DrawOrder::Sorted`](unlit_wgpu::scene::DrawOrder::Sorted)，由 pass 的合成把所有源
共同贡献的池从远到近排序，混合网格与不同源的深度排序笔画在此汇成一份列表。

</details>

## lit 管线

内置 lit 管线作为普通家族注册，实体所用的着色器变体只含其网格实际拥有的通道。它
没有任何特权：调用者自己的家族通过同一个 `MeshSource::register_family` 注册，
拥有自己的逐实例顶点流，并通过同一套公开构造函数绑定本帧共享输入。见
[自定义着色器与逐实例数据](#自定义着色器与逐实例数据)。

<details>
<summary>内置变体支持什么，以及姿势数据放在哪里</summary>

- 变体跟随网格：位置、UV、顶点色、法线通道是否存在，是否以压缩形式到达，是否蒙皮或形变，
  以及是否绑定材质——再加上调用方的策略与本帧的目标。逐实例字段（变换、颜色、关节
  基址、形变基址、元数据索引）恒被绑定，而不是逐个开关；仅在某个模式下才读的两个——
  裁剪值与外壳宽度——在该模式打开时才声明，因为顶点阶段从不读取的顶点属性正是校验层
  会抱怨的东西。
- 关节矩阵与形变权重**不在** mesh 自己的绑定组里。mesh 与实例是多对一：同一个 mesh
  可以被多个实体绘制，而每个实体的姿势通常不同。绑定组是按 mesh 绑定的，无法表达
  逐实例状态；逐实例各建一个绑定组同样不可取——那等于每帧每实例都重建绑定组。因此
  姿势被拆成两半，各归其位：
  - **数据进全帧共享的 SSBO**，绑定在 global 组。所有可见实例的关节矩阵拼成一个数组，
    形变权重拼成另一个。
  - **定位信息进实例流。** 每个实例的逐实例记录里带一个关节矩阵基址，另外单独带一个
    形变权重基址（两个 `Uint32`，因为蒙皮与形变相互独立），着色器用它找到自己的
    切片。记录里还带基础色、`alphaMode: MASK` 片元所比对的裁剪值，以及网格元数据索引。
    逐实例步进属性按实例序号寻址，
    所以合并为实例化 draw 不改变任何实例读到的数据——同一 mesh 的多个实例能折成一次
    draw，而各实例的姿势各归各，这正是自动实例化成立的前提。
- 姿势本身在高层是**组件**，放在独立实体上，mesh 实体用两个独立的引用组件
  （`SkinBinding`、`MorphBinding`）关联过去。这与「资源引用就是实体引用」一脉相承，
  并带来两个直接好处：
  - **共享即共享一个实体**：多个 mesh 引用同一个姿势实体就共用一个姿势，改一次全都
    动；要各自独立就各自引用不同的实体。两者是同一套机制，不需要额外的句柄类型。
  - **改姿势是一次组件写入**：无需任何 GPU 调用，渲染器在下一帧自动打包上传。与
    「逐帧上传对调用方透明」一致。
- **代价是引用必须给出**：mesh 的顶点流带骨骼索引、或带 morph targets 时，它的实体
  必须挂上对应的引用组件，否则渲染时 panic 而不是静默地按零号姿势绘制——静默降级会
  把「忘记关联」变成难以察觉的画面错误。另一条约束是权重的数量必须与该 mesh 的
  target 数量一致，因为着色器的循环上界是 mesh 自己的 target 数，越界读 storage 是
  运行期错误而非可捕获的 panic。

</details>

### 受光或无光

[`LitOptions::unlit`](unlit_wgpu::pipeline::LitOptions::unlit) 是调用方选项里的一个
开关，只决定一件事：片段阶段要不要累加本帧的光源。两种模式走同一个家族——网格通道、
实例流、绑定组布局、着色器包都是共用的——因此为一种模式分配的网格在另一种模式下照画。
两种模式都没有 prepass、没有阴影贴图、没有计算通道。

### 描边

内置了两种沿形状画线的方式，它们回答的是不同的问题：

- `LineSource` 画**笔画**：一个笔画是一个挂着 `Line` 组件的实体——一条线段记录，
  带世界空间中的两个端点、两端各自的颜色、一个半宽，以及两端是否为圆头，再外加记录
  本身装不下的绘制策略：[`LineOptions`](unlit_wgpu::line_shader::LineOptions) 声明
  入口着色器与拼进它的调用者代码片段、硬边切割、深度模板策略、混合与写入掩码。
  [`Line::opaque`](line::Line::opaque) 与 [`Line::blended`](line::Line::blended)
  给出两种常用策略；选项相同的笔画合为一次绘制，选项不同的各画各的。每条线段变成一个
  实例；顶点阶段把它展开成一条携带该宽度的屏幕空间四边形，片元阶段用胶囊的距离场覆盖
  它，因此无论多宽边缘都是平滑的。不透明策略改为在半覆盖处切割这条边缘——覆写画面的
  笔画没有颜色可以罩住背景——而切割会在目标自己有抗锯齿的地方让位：多采样目标把同一份
  覆盖率摊进 alpha-to-coverage 的样本掩码，边缘在那里依旧平滑。宽度以**物理像素**计，这正是关键：靠近相机的笔画与
  同一个笔画在远处一样粗细，而以世界单位计的宽度会随距离变细。source 就是一个普通的
  `FrameSource`——一个 world 的笔画就是它的 scope 这一帧看见的 `Line` 实体——并声明
  `FrameSourceOrder::LINE`，排在 mesh source 之后：每种笔画都测试网格写下的深度，所以
  本体必须已经进了深度缓冲，身后的笔画才会被挡住。是否也写深度由笔画自己的策略说了算——
  默认的混合笔画不写，默认的不透明笔画写，笔画之间因此能互相遮挡——而不动深度的笔画
  把它的中点深度交给 pass 的合成，与所有源的混合绘制一起从远到近排序，颜色按这个顺序层叠。
- 翻转外壳 `LitOptions::inverted_hull` 画本体周围的**带子**：同一份 mesh 再画一遍，
  沿自身法线外扩并剔除正面。宽度以**世界单位**计、来自实例，因此随距离变细。它是
  lit 家族的一个变体而不是自己的 source，所以两次绘制共享网格缓冲、落在同一个 pass 里；
  具体构造见 [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md#墨线描边翻转外壳)。
  它之上的一切只是组件：一个带网格、外壳管线与 `InstanceHullWidth` 的实体，就是一个
  带墨线的本体，而本体自身是同一份 mesh 走普通 lit 管线。

### 光源与着色

光源就是普通实体上的普通组件，因此摆放和移动它们与其他东西完全一样，一帧里通过
同样的场景遍历收集进来。内置提供四种，调用者用它所用的同一套 trait 扩展这个集合：

- `DirectionalLight`——太阳；平移无关紧要，只有旋转起作用。
- `PointLight`——一个带范围的位置。
- `SpotLight`——带内外锥角的点光，沿实体本地 −Z 发光。
- `QuadLight`——局部空间四个顶点构成的任意凸多边形，用于面光与柔和的窗光；它的
  `rect` 构造函数构造常见的矩形。

着色本身只是 Lambert；片元如何找到照到它的光源——聚类、方向光 uniform、多边形面光的
形状因子——见 [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md#光照前向聚簇的-lambert)。
这一层负责的是把光源做成组件，并逐帧把它们加起来。

方向光不参与聚类：它照亮每一个片元，因此放进一个 uniform 而不是写进每一格，最多
`MAX_DIRECTIONAL_LIGHTS` 盏，超出的部分丢掉最弱的。环境项也在同一个 uniform 里，
但它与别的灯一样是组件而不是 source 的字段：`AmbientLight` 就是普通组件，本帧把
source 作用域里的每一盏加起来，场景添加、搬动或移除它与其他东西完全一样。阴影不在
范围内，所以这条管线只是个光照累加器。

## 自定义着色器与逐实例数据

帧路径中没有任何东西是留给内置 lit 着色器的：它作为一个普通家族住在 `lit` 模块里。
调用者的家族通过与内置家族同一个
`MeshSource::register_family` 调用注册，并提供自己的管线——手写的
`wgpu::RenderPipeline`，或用本 crate 公开的着色器包组合出的 WESL 模块——需要时还提供
自己的逐实例顶点数据。

若想保留内置家族——它的布局、顶点流、逐实例记录——只换掉如何着色，则不必注册任何东西：
把 `unlit_wgpu::pipeline::Shader` 放进 `unlit_wgpu::pipeline::LitOptions::shader`，再构造
`lit::LitPipelineKey` 即可。它装着整整一块 WESL 模块，组合时顶替内置的 `lit.wesl`：自己写
`vs_main` 与 `fs_main`，import 想保留的阶段模块——`package::lit_vertex`、`lit_lighting`、
`lit_fragment`、`lit_bindings`——于是内置步骤照旧做它们的事，它们之间与之外才是调用者的代码。
布局仍由变体决定——这正是「只改一个字段」的原因。
自己实现一个家族是另一条路，适用于着色之外还有别的东西要变的情况。

### 逐实例顶点数据

逐实例状态放在顶点流里而不是绑定组里，因为它随实体而异，而绑定组是按 mesh 绑定的。
因此每个家族都拥有自己的一条实例流，由 `InstanceData` 描述：

```rust,ignore
pub trait InstanceData: 'static {
    fn stream(&self) -> InstanceStreamDesc;
    fn write(&mut self, context: &mut InstanceContext<'_>, out: &mut [u8]);
}
```

`stream` 声明顶点缓冲槽位与记录步长，`write` 为一个实体填出一条记录。剔除阶段
只把实体及其已解析的世界变换交给家族；记录里其余的字段——颜色、裁剪值、网格元数据
索引、蒙皮或形变基址——由家族自己从 world 读取，想读什么读什么。这就是自定义的
「收集」一侧：由家族决定收集哪些组件、记录长什么样。自身没有逐实例状态的家族传
`()`，其流为空。

内置 lit 家族只是这个 trait 的一个实现（`LitInstance`，槽位 `INSTANCE_SLOT`，
步长 `size_of::<MeshInstance>()`）——与调用者实现的接口相同，背后没有任何私有路径。
它也是唯一构造内置记录 `MeshInstance` 的地方：那条记录属于 lit 家族，而不属于共享
帧路径，因此自定义家族不会拿到任何由帧代造的记录——它写的是自己管线声明的格式。

### 蒙皮与形变状态

蒙皮与形变是两个独立概念，因此实例上下文带的是两个各自增长的数组和两个打包方法，
而不是一个笼统的「姿势」：

```rust,ignore
impl InstanceContext<'_> {
    pub fn pack_joints(&mut self) -> u32;
    pub fn pack_morph_weights(&mut self, targets: u32) -> u32;
}
```

`pack_joints` 把实体的关节矩阵——即其 `SkinBinding` 指名的 `SkinPose`——追加到
本帧关节数组，并返回其切片起始下标；`pack_morph_weights` 对 `MorphBinding` 指名的
`MorphWeights` 做同样的事，并校验权重数量与网格 target 数一致。两者都不画的家族
两者都不调用；lit 家族对蒙皮网格调用 `pack_joints`、对形变网格调用
`pack_morph_weights`，把返回的两个基址写进记录。

数组本身归 source 所有：由它持有、扩容，并在所有家族写完记录后每帧统一上传。家族
只在写记录时向其中追加，这正是它们作为通用共享设施、而非 lit 专属状态的原因。

<details>
<summary>家族的记录如何到达绘制</summary>

- 每帧 source 先让每个家族开始一份新的记录列表，然后按排序后的可见条目顺序遍历，
  为每个条目调用 `write`，并把它落在的下标记在条目上。因此一个家族的记录按可见
  顺序排列，每个条目一条，正符合合并后的实例化 draw 的预期。
- 每个家族的 `InstanceBuffer` 按需增长，每帧通过与本帧其他数据相同的池化 staging
  路径上传一次。
- 组装场景时，合并后的 draw 绑定其家族声明的槽位，实例范围从首个条目的记录下标开始；
  流为空的家族不绑定任何东西。
- 由于每个家族拥有自己的流，一段可合并的 run 绝不会横跨两个家族：管线身份已经把它们
  分开，而每个家族的记录在它自己的列表内连续。

</details>

### 本帧的共享输入

想要相机、帧 globals、视图或姿势与元数据数组的自定义管线，通过公开构造函数绑定
它们，而无需自己重造布局：`GlobalResources::layout` 从 `GlobalBindings` 描述构造
`wgpu::BindGroupLayout`，`GlobalResources::recipe` 返回渲染器注册的
`GlobalRecipe` 配方，因此数组变化时该组会被重建。内置的 `LitFactory` 调用的正是
同一批函数。

相机、globals 与视图不归 source 所有：帧为每个可见相机与视口各上传一份记录，配方
拿到的是正在组装的那次绘制的 `FrameUniforms`，而不是闭包捕获的 buffer。渲染器因此
按（管线、相机、视图）缓存一个组，同一个 source 经两台相机绘制时，各自绑定与那次绘制
匹配的组，而不是它自己的 buffer 上一次持有的那个。

着色器完全不读帧 uniform 的家族，可以彻底拥有自己的 group 0：
`GlobalRecipe::own` 把调用方自己的资源声明为这个组的输入，于是该组只在其中之一被
替换时重建，绝不会因为某次绘制换了相机而重建。自定义管线示例就是靠它画出一片不继承
帧相机的程序化天空。

## 加载 glTF 文档

`gltf` 模块（cargo feature `gltf`，默认关闭）把一个
[glTF 2.0](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html) 文档——`.glb`
或 `.gltf`——加载为 `LitGltf`。文档、缓冲与图像在构造函数里被急切地解析和解码，
每个节点的世界空间变换也提前算好，因此 `LitGltf` 是模型的**记录**，而不是 GPU
资源：它既不拥有 `World`，也不拥有 `MeshSource`。

压缩 primitive 在导入时用 `draco-core` 解码——一个纯 Rust 解码器——因此带
`KHR_draco_mesh_compression` 的文档在本引擎运行的每个目标上都能加载，包括 wasm32。
这条导入路径与「每张图像上传成什么 GPU 格式」都归 `gltf` 模块自身的文档管；
本节的职责是它往你已经在渲染的那个世界里修补了什么：

- 文档的集合用类型化 id 寻址——`ImageId`、`MaterialId`、`MeshId`、
  `PrimitiveId`、`NodeId` 与 `AnimationId`——节点下标因此不会误传给动画或图像。
  `images()`、`materials()`、`meshes()`、`primitives()`、`nodes()` 与
  `animations()` 迭代器给出文档实际携带的 id。
- `insert_resources` 一次上传整份文档，返回持有全部句柄的 `gltf::GltfResources`；
  `spawn_node` / `spawn_default_scene` 接收的正是这个结构体，每个句柄都带有它来自的
  文档条目的类型化 id。想一次只插入一类资源的调用方
  ——比如让两份文档共用一个纹理——也可以改用 `insert_image` / `insert_material` /
  `insert_mesh` 上传单个图像、材质或网格，或用批量版本（`insert_images`、
  `insert_materials`、`insert_meshes`）一次上传某一类全部资源（按文档顺序），
  再自行拼出 `GltfResources`。
- 交出句柄只让其所指变得可回收：材质的绑定组与材质同寿，它采样的纹理视图也由材质
  持有，因此丢掉材质句柄就足以释放两者，下一次 `maintain` 回收它们。
- `spawn_node` / `spawn_default_scene` 生成绘制该节点网格（或默认场景可达的每个节点）
  的实体：每个 primitive 一个实体，各自携带节点的世界空间 `Transform`、已上传的
  `GpuMesh`、该网格的 `components::GpuRenderPipeline`、以材质基础色因子着色的 `lit::InstanceColor`，
  以及——当网格读取基础色纹理时——对应的 `GpuMaterial`。`alphaMode: BLEND` 的
  primitive 还会带上 `ZSortedDrawing` 标记，于是 pass 的合成把它与各源的混合绘制一起
  按由远及近的顺序混在顺次绘制之后，且其管线不写深度，使它留给后续片元混合的像素仍可见。
  `alphaMode: MASK` 的材质则绘制二值覆盖：片元着色器丢弃 alpha 低于材质
  `alphaCutoff` 的片元（文档未给出时为 0.5），因此管线的 `LitOptions::alpha_cutoff` 被置位，
  生成的实体带上 `lit::InstanceCutoff`，其值走逐实例流，该 primitive 既不需要混合也不需要排序。
- 同时带有 `JOINTS_0` 与 `WEIGHTS_0` 的 primitive 会上传其关节流并按蒙皮绘制：
  `spawn_node` / `spawn_default_scene` 为该节点的 skin 生成一个 `SkinPose` 实体，并在网格上
  放置 `SkinBinding`，因此写入该 pose 的矩阵就是调用方驱动骨架的方式。
  `LitGltf::skin_pose` 由关节节点的世界变换与 skin 的逆绑定矩阵构建出文档的静止
  pose（每个关节一个矩阵）作为起点，`LitGltf::skin_joint_count` 报告某节点的 skin 有几个
  关节。
- 网格若声明了位移位置的形变目标，该 primitive 会连这些位移一起上传并按形变绘制：
  `spawn_node` / `spawn_default_scene` 为该节点生成一个 `MorphWeights` 实体，并在网格上放置
  `MorphBinding`，因此写入该实体的权重就是调用方形变网格的方式。`LitGltf::morph_weights`
  返回节点的起始权重——节点自身给出 `weights` 时用它，否则用网格的，未加权的目标为零——
  并按「位移位置的目标个数」补齐或截断，这正是渲染器要求的长度。只位移法线或切线的目标
  不计入也不上传，因此当网格的所有目标都不位移本加载器所读的数据时，它会以未形变的方式
  绘制，而不是走形变路径。
- 材质为 `doubleSided` 的 primitive 从两面绘制：该管线变体去掉剔除模式，于是单面变体本会
  丢弃的背面会被光栅化。变体的其他部分都不变，因为 lit 片元着色器不读法线——没有背面
  法线需要反转，也没有光照方程需要求值。
- 文档中的动画按需采样而非自动播放：`LitGltf::animation_count`、
  `LitGltf::animation_name` 与 `LitGltf::animation_duration` 描述它含有的片段，
  `document.apply_animation(&scope, clip, time, &spawned)` 在选定的时间求值其中一个。
  它把每条通道（`STEP`、`LINEAR` 或 `CUBICSPLINE`，旋转走球面插值）采样进被动画节点的局部
  变换，沿节点层级重新组合出世界矩阵，再把结果写到生成实体读取它的地方：每个被动画节点的
  `Transform`（以及继承其变化的子孙节点）、每个因关节移动而跟随的网格的 `SkinPose`、以及
  权重通道所指的每个 `MorphWeights`。它接受 `&Scope`，因此调用方可以与其他读取 world 的
  代码同时驱动它，并且不会碰传入 `&[GltfNode]` 之外的任何东西。
- 片段只解码一次并保留：通道指向的关键帧在第一次播放该片段的帧里收集，之后不再从文档的
  缓冲区逐帧重读。工作集——它覆写的局部变换、重新组合的世界矩阵，以及每节点的标志与权重
  缓冲——同样跨帧复用，只增长到其高水位一次。因此一帧只在向 world 写入新内容的地方分配。

网格上传所用的管线 key 总是由 primitive 自身的属性推导（见 `LitGltf::pipeline_key`），
所以它绘制所用的变体绝不会要求网格不存在的流。加载器读取什么、忽略什么，完整清单见
`gltf` 模块的支持范围。

图像按其位深的四通道 GPU 格式上传，源通道不足处就地拓宽——正是这一步让 8 位灰度纹理能在
采样器里解码自己的 sRGB 编码，如同 RGB 图像一直可以的那样。具体映射，以及设备可能拒绝过滤
的唯一格式（`Rgba32Float`），见 `gltf` 模块。

每次上传都带有 mip 链，在加载时就平均出来，而不是留给采样器去读一张只有基础级的纹理：
只有一级的纹理被缩小时，无论采样器怎么设都会走样。每一级是上一级的盒式平均值，并在「让平均
对光而言名副其实」的表示下取得——sRGB 图像在线性光下平均，半浮点图像在单精度下平均，单精度
图像就在自己的 texel 里平均。改为对存储编码平均会让该级亮度发生偏移，而 mip 链存在的意义正是
避免这个误差。文档构建多长的链由 `MipChain` 决定，用 `LitGltf::mip_chain` 设置。
## UI

`ui::UiPanel` 本身就是一个持有闭包的行为组件，所以一个界面就是一个实体——一帧可以有
与实体数量相同的面板，`ui::UiSource` 驱动器会按查询顺序运行 world 携带的所有面板。

面板只能在它自己的 egui context 所运行的地方构建，因为回调拿到的是属于正在构建它的那一趟
pass 的 `&mut egui::Ui`。因此「面板住在一个 world、绘制属于另一个 world」的 UI 以**数据**
的形式跨越这条边界：`ui::lay_out` 用调用方持有的 context 运行一个 world 的面板，返回
`ui::ExternalFrame`——三角化后的图元、纹理更新与平台输出，全是普通的自有值。之后帧按两个
world 约定好的方式跨越边界：它落进 `ui::UiSource` 自己持有的
`ui::ExternalFrame` 槽位，槽位因此留在负责绘制的那条线程上；绘制侧量出的目标则通过
`relay::Snapshot` 回来，面板所在的 world 在布局下一帧时读取它。两侧的 context 保持独立：共用会让两个 world 同时写同一份 memory 与字体图集，而 egui 把一次
`run_ui` 当作一个完整帧。纹理 id 则完全不改写：帧保留它那个 context 分配的 id，GPU 集成按
（来源, id）建键，于是两侧都从零分配也不会撞车；一帧在其槽位被更新之前一直由同一份三角化
结果绘制。`unlit3dx` 就在这两块之上搭建跨 world 的管线——面板所在 world 里的
`UiPanelHost`，它在该 world 自己的 tick 上以 job 的形式发送帧。

<details>
<summary>为什么面板是组件，以及最容易出错的单位约定</summary>

**界面本身就是行为组件，不是源上的闭包字段。** 这与「调用者即系统」的约定一致：源是
驱动器，它决定调用哪些面板、以什么顺序；面板要记状态就放在它自己的兄弟组件里（行为
组件运行期间被借用，不能重入借用自己）；面板拿到 `&World`，可以读写组件、也可以
`queue()` 结构变更。因此多个面板就是多个实体，可以按需增删，第三方也能定义自己的
面板类组件由源筛选——源本身不需要知道有哪些界面。

**单位约定是这套 API 最容易错的地方**：egui 的顶点与裁剪矩形使用逻辑点，而窗口事件
坐标是物理像素。因此投影用点（物理尺寸除以缩放因子），而 scissor 必须乘缩放因子——
两者单位相反。此外 egui 为多趟布局会**多次**调用面板闭包，因此面板必须幂等，或按趟数
判断该做什么；这是 egui 的既有语义，所有后端都一样。

UI 本身只需要 color 附件，不需要深度附件，但是管线的深度状态必须与 pass 的附件
**完全一致**（wgpu 按格式强校验，不看写入与比较设置）。在带深度附件的 pass 里（UI 与
mesh 共用同一 pass 是常见情形），UI 管线仍须声明**相同的**深度格式，只靠「不写入深度、
不深度测试」来避免干扰 mesh 的深度——「不需要深度」不等于「不声明深度」。「目标没有
深度附件」也是合法用法，因此管线的深度状态是可选的；可选的深度状态服务于**目标本身
没有深度附件**的情形，那时 pass 里的所有管线都必须不声明深度，并非 UI 独有：只由
[`BlitSource`](blit::BlitSource) 绘制的一帧同样没有深度附件。这一点与 egui 官方后端
一致：它默认不带深度状态，但在被给予深度格式时，仍会建出同一格式、不写深度、比较函数
为 `Always` 的状态。帧究竟带不带深度附件由调用方决定：
[`FrameAttachments::new`](attachments::FrameAttachments::new) 只在被要求时才分配深度附件，
因此源都不声明深度的窗口绘制进的就是不带深度附件的帧。

</details>

## 示例

下面的帧骨架假设 device 由外部提供。`spawn_context` 把 GPU 状态放进 world，
`MeshSource` 作为源生成，`Renderer` 是帧驱动器：

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::LitOptions;
use unlit_wgpu::resources::ResourceGraph;

let (device, queue) =
    wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
let mut world = World::new();

// 1. Spawn the frame's GPU context, the built-in mesh source and the
//    frame driver, and register the built-in lit family.
// `RenderDevice` is the device and what came with it: the queue, the
// `DeviceCapabilities` it was requested with (what it cannot report about
// itself, like `base_vertex` support), and its `DeviceHealth`. A context
// made from an adapter answers all four; `unwatched` builds one from a bare
// device and queue, as a test's `noop` device does.
let ctx = spawn_context(
    &mut world,
    RenderDevice::unwatched(device, queue),
    ResourceGraph::new(),
);
let mut mesh_source = MeshSource::new(&world.root_scope(), ctx);
mesh_source.register_lit_family(&world.root_scope());
let key = LitPipelineKey::new(LitOptions::standard());
let renderer = world.root_scope_mut().spawn(bundle![Renderer::new(ctx)]);

// 2. Spawn the frame's pass and mount the mesh source under it. A pass is a
//    scope: it records the sources below it, and each source in turn is a
//    scope of its own, so everything a source draws lives under its entity.
let pass = world
    .root_scope_mut()
    .spawn(Pass::new(FramePassOrder(0)));
let source = world
    .scope_mut(pass)
    .unwrap()
    .spawn(bundle![Source::new(mesh_source)]);
let scope = world.scope(source).unwrap();

// 3. Allocate geometry and a material through the mesh source, and spawn the
//    renderable entity under it, carrying the lit key.
let (mesh, material) = scope
    .query_one::<&mut Source>(source)
    .map(|mut source| {
        let source = source.get().as_mut::<MeshSource>().unwrap();
        let positions = [[0.0; 3]; 3];
        let uvs = [[0.0; 2]; 3];
        let colors = [[255u8; 4]; 3];
        let indices = [0u32, 1, 2];
        let mesh = source.allocate_lit_mesh(
            &scope,
            &key.options,
            LitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        );
        let device = source.device(&scope);
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
        let texture_view = source.register_texture_and_default_view(&scope, texture).1;
        let sampler = source.register_sampler(&scope, None);
        let material = source.allocate_lit_material(&scope, &key.options, texture_view, sampler);
        (mesh, material)
    })
    .unwrap();
world
    .scope_mut(source)
    .unwrap()
    .spawn(bundle![Transform::default(), mesh, material, GpuRenderPipeline::new(key)]);

// 4. Give the pass a render target and render one frame (uses the noop
//    device, so it produces a valid command buffer without touching a GPU).
//    The frame's depth and multisample attachments are allocated and kept by
//    `FrameAttachments`; the color attachment is this example's own.
let color = create_color_target(
    &ctx.device(&world.root_scope()).unwrap(),
    wgpu::TextureFormat::Rgba8UnormSrgb,
    1280, 720,
);
let attachments = FrameAttachments::new(
    &world,
    ctx,
    wgpu::TextureFormat::Rgba8UnormSrgb,
    (1280, 720),
    1,
    true,
);
let scope = world.scope(source).unwrap();
let color_view = scope
    .query_one::<&mut Source>(source)
    .map(|mut source| {
        source
            .get()
            .as_mut::<MeshSource>()
            .unwrap()
            .register_texture_and_default_view(&scope, color)
            .1
    })
    .unwrap();
world
    .root_scope()
    .query_one::<&mut FramePass>(pass)
    .map(|mut pass| {
        pass.get().set_attachments(&attachments, color_view);
    })
    .unwrap();
world
    .root_scope()
    .query_one::<&mut Renderer>(renderer)
    .map(|mut r| r.get().render(&world))
    .unwrap();
```

UI 面板是行为组件，所以装配一个界面就是一次普通的 spawn：

```rust
# #[cfg(feature = "ui")]
# {
use unlit3d::prelude::*;

let mut world = World::new();
// The panels of a UI source live under it: a frame hands the source its own
// scope and drives whatever panels it finds there.
let ui = world.root_scope_mut().spawn(bundle![Source::new(UiSource::new())]);
world.scope_mut(ui).unwrap().spawn(bundle![UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),]);
# }
```

## 输入

`input` 是输入处理中可移植的那一半：事件类型以及响应它们的行为组件，既不依赖 winit
也不依赖 egui。一帧经由 `InputWriter` 送入事件，它把每条事件投进每个订阅了它的世界
的缓冲。世界的缓冲就是它的 `InputEvents`：距上次 tick 以来到达的事件，加上这些事件
留下的 `InputState`（修饰键、指针、按键、窗口尺寸）。`dispatch_input` 把本世界的
队列取作本次 tick 的批次、折进状态并驱动这些行为。世界独占它还没 tick 的事件，因此
tick 频率低于输入到达频率的世界依然按顺序读到每一条，之后也不需要谁去清空。
`distribute_input` 把 `InputWriter` 复制进每个嵌套世界并在那里订阅，一条流即可供给
全部世界，而每个世界各建自己的状态；不在这个层级里的世界——由 runner 作为兄弟世界
启动的那个——则在自己的初始化里自行订阅。只读输入而完全不分发的世界改为每 tick 调用
一次 `refresh_input`，在任何读者读批次之前。来自窗口库的事件转换位于相应 feature 之后：
`input::winit::WinitInput` 转发 `WindowEvent`，`ui::convert` 把本 crate 的事件转成
egui 的事件。

<details>
<summary>命名规则，以及为什么分发要显式调用</summary>

- **事件类型不依赖 winit，也不依赖 egui**：核心类型自持，winit 与 egui 的转换放在各自
  feature 之后。这样游戏逻辑与 UI 共用同一条事件流，不需要两套事件。
- **事件回调是行为组件**，与 ECS 既有的行为组件范式同构：按事件类别调用所有对应行为
  组件。`OnInput` 收到全部事件，`OnKey`/`OnMouse`/`OnPointer`/`OnTouch`/`OnText`/
  `OnIme` 各收一类。鼠标族一律以 `Mouse` 命名（`MouseEvent`/`MouseButton`/
  `MouseButtons`/`OnMouse`），`Pointer` 专指**设备无关的「点输入」**：鼠标与触摸都
  产生 `PointerEvent`，因此同一段拖拽行为在桌面和触屏上都能用；`Touch` 保留手指独有的
  信息（触摸 id、压力）。反过来，触摸**不会**置位鼠标按键，`InputState` 里的按键状态
  是鼠标独有的。
- **`InputState` 只持有事件留下的状态**（修饰键、指针位置与按下的指针、光标、按下的
  按钮、焦点、窗口尺寸与缩放），不持有事件。它住在世界的 `InputEvents` 里，
  那份缓冲就是世界内所有读者触达它的方式：每个世界一份，
  由该世界自己的 tick 刷新。共享一份状态会让一个世界的读取取决于另一个世界的节奏，
  因此 `InputWriter` 不存任何状态，只做扇出。指针的按下状态按
  「接触点」逐个记录（`PointerContact`，由种类与 id 标识），因此多指同时按下时抬起
  一根不会误判为手势结束。
- **事件属于世界，而非某一帧**：送入一条事件就把它推进每个订阅世界的队列，由那个世界
  自己的 tick 取作本次的批次。若换成一份共享列表加每帧 `clear_events`，最慢读者的视野
  就会取决于最快者跑了多少次：120 Hz 的帧喂 60 Hz 的世界会丢掉一半事件，跳过一帧的
  世界则丢掉那一帧的全部事件。队列因此不设上限——为了守住上限去丢最旧的事件，恰好会
  重新引入队列本来要消除的丢失。
- **分发由调用方在帧循环里显式调用**，不由 `render` 内部自动进行。原因：回调内排队的
  结构变更需要 `&mut world` 才能落地，而 `render` 只拿得到 `&World`；自动分发会
  让回调排队的变更延迟到下一次外部 `apply`，且这个延迟对用户不可见。显式调用也让调用
  方掌控分发时机与顺序，与「调用者即系统」一致。
- 分发**直接遍历**行为组件即可，不需要先收集实体：回调借用的是不同实体的不同 cell，
  互不冲突。真正的限制来自 ECS 自身有意为之的取舍——回调不能重入借用自己那个组件，
  也不能重入同类分发；要保留状态就放到兄弟组件上。

UI 是否想独占指针/键盘是**egui 自己的答案**，不由源再写回世界：一帧布局完成后，源
那个 context 上的 `egui_wants_pointer_input` 与 `egui_wants_keyboard_input`
就描述了这一帧的声明。调用方通过世界里的 `Source` 值拿到那个 context——
`source.as_ref::<UiSource>()`——和取其它源的内部状态一样，游戏逻辑读它决定是否
响应。类型化访问返回 `Result`，源不是 `UiSource` 时错误会指出它实际持有的类型。
按 egui 的语义，这个答案描述的是刚布局完的那一帧。不做任何自动拦截。

</details>

## winit 集成

`winit` 模块分三层。`winit::surface` 是表现层：`WindowSurface` 配置窗口的交换链
并交出 `Frame`，`FrameAttachments` 持有该帧绘制所用的深度与多重采样目标。`winit::presenter` 把这两半
组装成现成的窗口循环（见[呈现一帧](#呈现一帧)）。`winit::event` 是桥接层：`ApplicationHandler` 的每个回调都有一个对应的行为组件族，
而 `WinitHost` 是应用唯一实现的 `ApplicationHandler`——它持有 world，把每个回调
分发给对应的族，自身不持有任何应用逻辑。`winit::builtin` 是一组现成的行为，其中没有
任何一条是应用走不了的路径。

回调组件就是普通的行为组件，按普通方式编写与测试：它拿到 `&World`、它所在实体与事件，
且不能触达自己那个组件类型，也不能重入宿主。结构变更经 `World::queue` 排队，在分发
之后由宿主 `apply` 时落地。回调签名刻意不带 `ActiveEventLoop`：该类型只能在回调内
构造，带它的组件永远无法单元测试。回调需要事件循环做的事改为请求组件——
`CreateWindowRequest` 与 `ExitRequest` 由宿主在分发后执行，`WinitWindow` 是组件
请求重绘时所用的窗口，而 `Resumed` 与 `DisplayHandle` 由宿主自己维护。

一帧之内的推进同样是行为。[`frame`](frame) 模块只有一个族
[`OnFrame`](frame::OnFrame)，由 [`dispatch_frame`](frame::dispatch_frame) 驱动；
帧行为拿到 `&World`、它所在实体，以及一个 `&mut`
[`Frame`](frame::Frame)——里面是这一帧的 delta、序号与尺寸。形状与事件族完全一致，
只是把「事件」换成了「这一帧的上下文」。用
[`OnFrame::new`](frame::OnFrame::new) 生成的行为不声明顺序，这些不声明顺序的行为构成一个
顺序带，带内的先后不做任何保证。若某个行为的读写被另一个行为依赖，就用
[`OnFrame::with_order`](frame::OnFrame::with_order) 配一个
[`FrameBehaviourOrder`](frame::FrameBehaviourOrder) 在两端都把顺序声明出来。库自身不声明
任何顺序值，谁先谁后由应用定义。窗口应用的宿主 world 与它驱动的每个 world 派发的是同一个族，
所以场景侧也用同样的方式组件化。用 [`ScopeMut::spawn`](unlit_ecs::ScopeMut::spawn)
生成行为。需要跳过本帧其余工作的行为写一个标志，由声明了更晚顺序的行为去读；需要结构
变更的行为把变更排队，因为回调只持有 `&World`。

```rust,no_run
# use unlit3d::winit::builtin::{ExitOn, ExitTrigger, WindowSetup};
# use unlit3d::winit::event::WinitHost;
# use winit::event_loop::EventLoop;
# use winit::window::Window;
# fn main() -> Result<(), winit::error::EventLoopError> {
let event_loop = EventLoop::new()?;
let mut host = WinitHost::new();
host.world_mut().root_scope_mut().spawn(WindowSetup {
    attributes: Window::default_attributes().with_title("unlit3d"),
});
host.world_mut()
    .root_scope_mut()
    .spawn(ExitOn(ExitTrigger::CloseRequested));
host.run(event_loop)
# }
```

### 呈现一帧

`winit::presenter` 是现成的窗口循环。它在 GPU 就绪后建一个 `Presenter` 组件，
再由三个行为驱动：`pump_presented` 让帧持续到来，`present_frames` 把 world 的
pass 画进交换链，`resize_presented` 跟随窗口。

`present_frames` 只在宿主没有跳过这一帧、且 winit 宿主没有报告应用挂起时呈现，所以
决定跳过的那一端——建场景、等 GPU——必须声明更小的 order；帧 order 在同一数值内不保序，
与 `PRESENT_ORDER` 撞值会让结果取决于排序。场景住在子 world 的运行时把自己的活交给 `pump_presented_with`：pump 会在
每次 dispatch 前在宿主 world 上跑它，子 world 的 job 与任务正是在那里排空。

画面走哪条路到窗口由 `PresentationPath` 决定：

| 路径 | 场景画进什么 | 代价 | 呈现后能否回读 |
|------|--------------|------|----------------|
| `Surface` | 交换链图像本身 | 一趟 pass | 否 |
| `Intermediate` | 一张 `PersistentTarget` 纹理，再由 blit pass 贴到交换链 | 两趟 pass | 是 |

`Surface` 是朴素窗口：pass 最少，但交换链图像在呈现时就被回收，而且它不是资源图节点，
所以谁都读不回来。`Intermediate` 多付一次 blit，换来一帧留在活过呈现的纹理里——截图、
快照测试、以及后续的后处理链要的就是这个。目标尺寸由它自己的 `IntermediateSize` 决定：
窗口的倍数（`Scale(1.0)` 跟随窗口，`Scale(0.5)` 按半宽半高渲染）或固定像素尺寸。

交换链本身如何等显示器，由 `SurfaceOptions` 决定，传给 `build_presenter` 与
`setup_presenter`。它的默认值——`Fifo` 加两个在途帧——把每一帧都绑在一次刷新上，
朴素窗口要的正是这个。`Immediate` 与 `Mailbox` 拿撕裂或多一帧延迟换掉等待，是游戏
用画质换输入延迟的手段。表面并不支持所有模式：WebGL2 与许多移动端驱动只给 `Fifo`，
所以表面缺少所请求的模式时降级到 `Fifo`，并打一条警告说明请求了什么、实际用了什么。
在途帧延迟决定一帧最多能在途等几次刷新：`1` 延迟最低，CPU 与 GPU 无从重叠，更高的值
偏向吞吐。

这些都是原语，presenter 只是把它们组合起来；自己搭帧循环的调用方直接使用它们：

| 想要 | 用什么 |
|------|--------|
| 画进窗口，没人读这一帧 | 用交换链附件的 pass（`WindowSurface` 加 `FrameAttachments`） |
| 画进窗口，并要读这一帧 | `PresentationPath::Intermediate`，或 `PersistentTarget` 配一趟带 `BlitSource` 的 pass |
| 完全没有窗口地画 | 用 `PersistentTarget` 的 pass，再 `readback_texture`；不涉及 presenter |
| 完全不要目标地画 | 不带附件的 pass：源照旧构建它们的场景，只是什么都不录制 |

presenter 因为要呈现而必须有窗口；后两种纯离屏形式不需要，快照测试与无窗口示例正是
这么画的。

presenter 的三个方法都只借 `&mut self`，所以它作为独立实体的组件存在：`release` 丢掉
交换链、其余一概保留，下一次 `present` 会自己重开一条——挂起的宿主继续呈现即可，
自己不必记任何状态。

MCP 截图按句柄从资源图里取纹理，因此只有在 `Intermediate` 下才看得到这一帧——交换链
图像不是节点。工具接受可选的 `target` 资源 id，既可以是纹理自己的 id，也可以是它任意
一个 view 的 id（画进离屏目标的 pass 手里通常只有 view）；不给时取 world 里唯一一张可
读纹理，链上有好几张时会报出歧义，这正是后处理链用来挑「截哪一级」的接口。

## 测试

```text
cargo xtask test                          # 整个工作区
cargo nextest run -p unlit3d --all-features  # 只跑本 crate
```

集成测试是一个二进制，它的 `required-features` 是 `gltf`、`mcp` 与 `ui`——其用例会用到的
功能。不带这些功能跑并不会报错：Cargo 会把整个 target 跳过，因此
`cargo nextest run -p unlit3d` 只报单元测试。要把 suite 放进这一趟，得加
`--all-features`（或把这三项都点名）。

GPU 集成测试把场景渲染到离屏目标并检查回读的像素。打开 `mcp` feature 后，一个端到端
测试会在一条线程上建起真实的 GPU world，用一对流把它服务出去，并与之说 JSON-RPC：
它会列出工具、读取 world、生成网格，再逐像素检查截图。这些场景的多帧快照覆盖现在位于
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)：
它的 `tests/gpu_suite.rs` 把这些场景与
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d_asset_files/blob/main/README.md)
下的 SSIMULACRA2 快照比较。在本仓库用 `git submodule update --init --checkout` 拉取
submodule（`--checkout` 是为了越过它的 `update = none`）；有意改动
后用 `SNAPSHOT_UPDATE=1 cargo nextest run -p unlit3d_examples` 重新生成，并审查图像差异。各测试层在整个工作区中的位置，以及 CI 所跑的内容，
见[根 README](https://github.com/beicause/unlit3d/blob/main/README.zh-CN.md#测试与基准)。

## 另见

- [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md)
  —— 下层渲染器。
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.zh-CN.md)
  —— 组件所在的 world。
- [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)
  —— 基于这套 API 的可运行窗口程序。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。