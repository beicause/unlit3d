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
| `gltf` | 否 | `gltf` 模块：把一个 glTF 文档加载为 `UnlitGltf`，把它的图像、材质与网格修补进另一个世界的 `MeshSource`，并生成绘制它们的实体 |
| `reflect` | 否 | 给跨越 JSON 边界的组件派生 [`facet`](https://docs.rs/facet) 反射，以及 glam 与 ECS 值所经由的代理类型；引擎自身从不读取它 |

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

## 帧模型

一帧**不是**「一个主场景加若干附加物」。`Renderer` 自身不绘制任何东西：它持有该帧
的渲染目标，并按各帧源通过 `FrameOrder` 声明的顺序录制它们。

- 每个源在 `build_scene` 中构建自己的 `Scene`，随后渲染器按序把所有场景录制进一个
  在目标附件之上开启的 pass。因此一帧就是一个 encoder、一次提交。
- 内置的 mesh 渲染就是其中一个源 `MeshSource`，它不比调用者自己的源享有更多特权：内置
  unlit 家族通过与调用者家族相同的 `MeshSource::register_family` 注册，一切专属于它的
  东西——它的 key、实例记录、mesh 与材质辅助方法——都住在 `unlit` 模块里。
  渲染器里没有任何 mesh 专用的字段或绘制路径。
- GPU 状态——`wgpu::Device`、`wgpu::Queue` 与 `ResourceGraph`——作为资源组件存在于
  world 中，通过 `RenderContext` 寻址。`spawn_context` 负责生成它们并返回地址；
  `RenderContext::of` 可以从它四个实体中的任意一个反查回整个上下文，
  `RenderContextInfo` 则是同一幅图的纯数据形式，供只做报告的读取者使用。
  它还接收本帧的 `DeviceCapabilities`，由调用者从适配器推导——设备无法自述的信息
  随帧一起传递，而不是让每个源各自重新发现。
- **构建与录制是两个阶段。** 源在 `build_scene` 期间往图里注册资源并暂存上传；录制
  阶段只读取已经产出的场景，因此源自身状态与资源图之间永远不会产生借用冲突。
- **资源图每帧只在构建阶段维护一次。** `replace` 只做标记；源在自己的上传之后、组装帧
  之前维护一次，一趟之内回收已无任何持有的资源并按依赖序重建脏的绑定组。源用
  `RenderContext::maintain_scope` 指出这个时机：它返回一个在 drop 时维护资源图的守卫，
  因此「无物可画」时的每一处提前 return 都会自动维护，无需记住任何调用；源在组装帧的
  位置显式丢弃该守卫。不绘制任何东西的帧同样会维护；场景之外的路径——交换链尺寸变化、
  acquire——则显式维护，而不是把旧资源留到下一次场景构建。
- 源的 GPU 资源恰好在还有句柄指名它时存活，因此销毁源实体即丢弃其句柄，下一次
  `maintain` 回收余下部分。无需记住任何释放调用。

**每个源自带状态与其可跨帧复用的 `Scene`。** egui 的 `Context`、字体图集、UI 自己的
UBO 都归 UI 源所有；3D 的管线、网格池、元数据、剔除缓存都归 mesh 源所有。「3D 主场景」
只是第一个被录制的源产出的场景，而不是渲染器的特例。

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
- **源的 GPU 资源由源自己显式释放**，这避免了引用计数自动释放资源的复杂机制。源在
  资源图里注册的节点不随实体销毁自动回收，移除源时必须显式释放。

**构建与录制是两个阶段。** 构建阶段独占资源图以注册资源，并写入本帧要上传的数据；
录制阶段只读各源自己的 `Scene`，完全不碰资源图。合并成一个阶段就必须在产出 `Scene`
前读资源图，于是只能用内部可变性把借用冲突推迟到运行期——因此保留两阶段，换取编译期
保证。

</details>

<details>
<summary>为什么顺序由每个源显式声明，而不是靠创建顺序</summary>

顺序是必需方法，且 `FrameOrder` 不实现 `Default`。若给出默认值，「忘记声明顺序」与
「确实想沿用创建顺序」就无法区分，顺序意图又变回隐式。顺序相同的源会记录并发出警告；
源的顺序必须是源显式携带的序号，不能借用实体在原型内的行序：实体的行序不保证稳定，
移除组件时会被打乱。

`Scene` 内部已有的顺序语义（非 z-sorted 在前、再按管线与材质或视线轴深度）不变；源之间的
顺序不是被 pass 级状态逼出来的硬约束——`Scene` 已携带必要的 pass 状态信息，在录制时
都会重设 pass 状态——而是「UI 要合成在 3D 之上」等类似语义的需要。

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

`MeshSourceUnlitExt::register_unlit_family` 注册内置的 unlit 家族；`MeshSource::register_family`
注册调用者自己的家族，这与内置家族走的是同一条路——连家族自己拥有并声明的逐实例
顶点流也不例外（见[自定义着色器与逐实例数据](#自定义着色器与逐实例数据)）。其他组件
包括 `Transform`、`Camera`、`RenderLoadOps`、`unlit::InstanceColor`
以及 `ZSortedDrawing` 标记。

带 `ZSortedDrawing` 的实体会在不透明实体之后绘制，并按「网格包围盒中心沿相机视线轴的
深度」由远及近排序——见 `Camera::view_depth`——因此枢轴点偏离几何体的网格仍按它实际
绘制的几何体排序，而不是按实体原点。

一帧通过世界中第一个 **active** 的 `Camera` 绘制——`Camera::active` 为 `false` 的实体会被
渲染器跳过。因此一个世界可以持有多个相机，逐帧切换该标志即可在它们之间切换；没有
active 相机时，帧会被清空，什么都不绘制。

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
  顶点布局与其索引格式交给 key，key 再把它们各归其位——目标进 `UnlitVariant::surface`，
  顶点布局进 `channels`，索引格式（与拓扑）进 `strip_index_format`。索引格式之所以重要，
  是因为 strip 拓扑的管线必须声明其绘制所绑定的索引宽度，而只有网格知道这个宽度——格式
  由 source 挑：取该网格顶点数能容纳的最窄格式，并在没有 `base_vertex` 的设备上烘入池
  偏移时把它加宽。**按什么维度特化因此是家族的自由**：内置 unlit 按其策略加上目标、顶点
  布局与索引格式特化，自定义家族可以是任何东西。
- 蓝图**延迟求值**：只在缓存未命中、真正要编译时才向 key 索取变体。
- 家族编译出管线后，由 `RenderPipelineFactory` 把它转成渲染器要注册的
  `RegisteredRenderPipeline`——已编译管线与全局组的重建配方。它与 `unlit_wgpu` 的
  `RenderPipelineDesc` 是一对**输出/输入**，两者之间隔着一次编译。

`unlit3d` 只处理渲染管线，故其管线类型一律显式带 `Render` 字样（`GpuRenderPipeline`、
`RenderPipelineKey`、`RenderPipelineId`、`RenderPipelineFactory`、
`RegisteredRenderPipeline`）。

</details>

## 自动实例化

条目先被排序，使相邻者共用管线与绑定组；随后，绘制状态完全相等的连续条目——同管线、
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
眼睛的直线距离。

</details>

## unlit 管线

内置 unlit 管线作为普通家族注册，实体所用的着色器变体只含其网格实际拥有的通道。它
没有任何特权：调用者自己的家族通过同一个 `MeshSource::register_family` 注册，
拥有自己的逐实例顶点流，并通过同一套公开构造函数绑定本帧共享输入。见
[自定义着色器与逐实例数据](#自定义着色器与逐实例数据)。

<details>
<summary>内置变体支持什么，以及姿势数据放在哪里</summary>

- 变体跟随网格：位置、UV、顶点色通道是否存在，是否以压缩形式到达，是否蒙皮或形变，
  以及是否绑定材质——再加上调用方的策略与本帧的目标。五个逐实例字段（变换、颜色、关节
  基址、形变基址、元数据索引）恒被绑定，而不是逐个开关。
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

## 自定义着色器与逐实例数据

帧路径中没有任何东西是留给内置 unlit 着色器的：它作为一个普通家族住在 `unlit` 模块里。
调用者的家族通过与内置家族同一个
`MeshSource::register_family` 调用注册，并提供自己的管线——手写的
`wgpu::RenderPipeline`，或用本 crate 公开的着色器包组合出的 WESL 模块——需要时还提供
自己的逐实例顶点数据。

### 逐实例顶点数据

逐实例状态放在顶点流里而不是绑定组里，因为它随实体而异，而绑定组是按 mesh 绑定的。
因此每个家族都拥有自己的一条实例流，由 `InstanceData` 描述：

```ignore
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

内置 unlit 家族只是这个 trait 的一个实现（`UnlitInstance`，槽位 `INSTANCE_SLOT`，
步长 `size_of::<MeshInstance>()`）——与调用者实现的接口相同，背后没有任何私有路径。
它也是唯一构造内置记录 `MeshInstance` 的地方：那条记录属于 unlit 家族，而不属于共享
帧路径，因此自定义家族不会拿到任何由帧代造的记录——它写的是自己管线声明的格式。

### 蒙皮与形变状态

蒙皮与形变是两个独立概念，因此实例上下文带的是两个各自增长的数组和两个打包方法，
而不是一个笼统的「姿势」：

```ignore
impl InstanceContext<'_> {
    pub fn pack_joints(&mut self) -> u32;
    pub fn pack_morph_weights(&mut self, targets: u32) -> u32;
}
```

`pack_joints` 把实体的关节矩阵——即其 `SkinBinding` 指名的 `SkinPose`——追加到
本帧关节数组，并返回其切片起始下标；`pack_morph_weights` 对 `MorphBinding` 指名的
`MorphWeights` 做同样的事，并校验权重数量与网格 target 数一致。两者都不画的家族
两者都不调用；unlit 家族对蒙皮网格调用 `pack_joints`、对形变网格调用
`pack_morph_weights`，把返回的两个基址写进记录。

数组本身归 source 所有：由它持有、扩容，并在所有家族写完记录后每帧统一上传。家族
只在写记录时向其中追加，这正是它们作为通用共享设施、而非 unlit 专属状态的原因。

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

想要相机、globals 或姿势与元数据数组的自定义管线，通过公开构造函数绑定它们，而无需
自己重造布局：`GlobalResources::layout` 从 `GlobalBindings` 描述构造
`wgpu::BindGroupLayout`，`GlobalResources::rebuild` 返回渲染器注册的
`Rebuild` 配方，因此数组变化时该组会被重建。内置的 `UnlitFactory` 调用的正是
同一批函数。

## 加载 glTF 文档

`gltf` 模块（cargo feature `gltf`，默认关闭）把一个
[glTF 2.0](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html) 文档——`.glb`
或 `.gltf`——加载为 `UnlitGltf`。文档、缓冲与图像在构造函数里被急切地解析和解码，
每个节点的世界空间变换也提前算好，因此 `UnlitGltf` 是模型的**记录**，而不是 GPU
资源：它既不拥有 `World`，也不拥有 `MeshSource`。

它做的是修补你已经在渲染的那个世界：

- `insert_resources` 一次上传整份文档，返回持有全部句柄的 `GltfResources`；
  `spawn_node` / `spawn_default_scene` 接收的正是这个结构体。想一次只插入一类资源的调用方
  ——比如让两份文档共用一个纹理——也可以改用 `insert_image` / `insert_material` /
  `insert_mesh` 上传单个图像、材质或网格，或用批量版本（`insert_images`、
  `insert_materials`、`insert_meshes`）一次上传某一类全部资源（与文档自身的索引对齐），
  再自行拼出 `GltfResources`。
- 交出句柄只让其所指变得可回收：材质的绑定组与材质同寿，它采样的纹理视图也由材质
  持有，因此丢掉材质句柄就足以释放两者，下一次 `maintain` 回收它们。
- `spawn_node` / `spawn_default_scene` 生成绘制该节点网格（或默认场景可达的每个节点）
  的实体：每个 primitive 一个实体，各自携带节点的世界空间 `Transform`、已上传的
  `GpuMesh`、该网格的 `unlit::UnlitPipeline`、以材质基础色因子着色的 `unlit::InstanceColor`，
  以及——当网格读取基础色纹理时——对应的 `GpuMaterial`。`alphaMode: BLEND` 的
  primitive 还会带上 `ZSortedDrawing` 标记，于是渲染器在绘制完不透明几何之后按由远及近
  的顺序混合它。`alphaMode: MASK` 的材质则绘制二值覆盖：片元着色器丢弃 alpha 低于材质
  `alphaCutoff` 的片元（文档未给出时为 0.5），因此管线的 `UnlitOptions::alpha_cutoff` 被置位，
  生成的实体带上 `unlit::InstanceCutoff`，其值走逐实例流，该 primitive 既不需要混合也不需要排序。
- 同时带有 `JOINTS_0` 与 `WEIGHTS_0` 的 primitive 会上传其关节流并按蒙皮绘制：
  `spawn_node` / `spawn_default_scene` 为该节点的 skin 生成一个 `SkinPose` 实体，并在网格上
  放置 `SkinBinding`，因此写入该 pose 的矩阵就是调用方驱动骨架的方式。
  `UnlitGltf::skin_pose` 由关节节点的世界变换与 skin 的逆绑定矩阵构建出文档的静止
  pose（每个关节一个矩阵）作为起点，`UnlitGltf::skin_joint_count` 报告某节点的 skin 有几个
  关节。
- 网格若声明了位移位置的形变目标，该 primitive 会连这些位移一起上传并按形变绘制：
  `spawn_node` / `spawn_default_scene` 为该节点生成一个 `MorphWeights` 实体，并在网格上放置
  `MorphBinding`，因此写入该实体的权重就是调用方形变网格的方式。`UnlitGltf::morph_weights`
  返回节点的起始权重——节点自身给出 `weights` 时用它，否则用网格的，未加权的目标为零——
  并按「位移位置的目标个数」补齐或截断，这正是渲染器要求的长度。只位移法线或切线的目标
  不计入也不上传，因此当网格的所有目标都不位移本加载器所读的数据时，它会以未形变的方式
  绘制，而不是走形变路径。
- 材质为 `doubleSided` 的 primitive 从两面绘制：该管线变体去掉剔除模式，于是单面变体本会
  丢弃的背面会被光栅化。变体的其他部分都不变，因为 unlit 片元着色器不读法线——没有背面
  法线需要反转，也没有光照方程需要求值。
- 文档中的动画按需采样而非自动播放：`UnlitGltf::animation_count`、
  `UnlitGltf::animation_name` 与 `UnlitGltf::animation_duration` 描述它含有的片段，
  `UnlitGltf::apply_animation(world, clip, time, &spawned)` 在选定的时间求值其中一个。
  它把每条通道（`STEP`、`LINEAR` 或 `CUBICSPLINE`，旋转走球面插值）采样进被动画节点的局部
  变换，沿节点层级重新组合出世界矩阵，再把结果写到生成实体读取它的地方：每个被动画节点的
  `Transform`（以及继承其变化的子孙节点）、每个因关节移动而跟随的网格的 `SkinPose`、以及
  权重通道所指的每个 `MorphWeights`。它接受 `&World`，因此调用方在已经持有 world 的任何
  地方都能驱动它，并且不会碰传入 `&[GltfNode]` 之外的任何东西。

支持加载：位置、UV、顶点色、关节、权重与索引；基础色纹理及采样它们的材质；混合
（`alphaMode: BLEND`）、裁剪（`alphaMode: MASK`）与双面材质；蒙皮 primitive；位移位置的形变目标；
按世界变换累加的节点层级；以及驱动它们的动画。暂不支持：法线与切线形变、法线与切线。网格上传所用的管线 key 总是由
primitive 自身的属性推导（见 `UnlitGltf::pipeline_key`），所以它绘制所用的变体绝不会
要求网格不存在的流。

图像按「最接近解码出像素」的 GPU 格式上传，因此纹理保留自己的通道数与精度，而不会被一
律拓宽成 RGBA8：8 位布局上传为 `R8Unorm`/`Rg8Unorm`/`Rgba8UnormSrgb`，16 位上传为同宽
度的半浮点格式，32 位浮点上传为 `Rgba32Float`；三通道布局则补一个通道，因为 sRGB 与浮
点格式都没有三通道的。两个后果值得一提：灰度纹理把亮度放在红通道里——WebGPU 既没有亮度
格式也没有分量重排——因此 unlit 着色器的 `BaseColorChannels::Luminance` 与
`BaseColorChannels::LuminanceAlpha` 会把它展开成 RGB(A) 并把亮度从 sRGB 解码，
`UnlitGltf::pipeline_key` 正好为这类上传设置它们；以及 `Rgba32Float` 在缺
少 `Features::FLOAT32_FILTERABLE` 的设备上是 `unfilterable-float`，此时材质绑定不可过滤
的采样器，并由 `UnlitOptions::texture_filtering` 特化绑定组布局来匹配。

## UI

`ui::UiPanel` 本身就是一个持有闭包的行为组件，所以一个界面就是一个实体——一帧可以有
与实体数量相同的面板，`ui::UiSource` 驱动器会按查询顺序运行 world 携带的所有面板。

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

下面的帧骨架假设 `device`/`queue` 由外部提供。`spawn_context` 把 GPU 状态放进
world，`MeshSource` 作为源挂载，`Renderer` 是帧驱动器：

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::resources::ResourceGraph;

let (device, queue) =
    wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
let mut world = World::new();

// 1. Spawn the frame's GPU context, the built-in mesh source and the
//    frame driver, and register the built-in unlit family.
// `DeviceCapabilities` carries what the device cannot report about itself
// (its `base_vertex` support). Derive it from the adapter when one is at
// hand; `default()` is the WebGPU baseline, which is also what `noop` is.
let ctx = spawn_context(
    &mut world,
    device,
    queue,
    ResourceGraph::new(),
    DeviceCapabilities::default(),
);
let mut mesh_source = MeshSource::new(&world, ctx);
mesh_source.register_unlit_family(&world);
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
let source = world.spawn_source(mesh_source);
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
    material,
    UnlitPipeline::new(key),
));

// 4. Bind a render target and render one frame (uses the noop device, so it
//    produces a valid command buffer without touching a GPU). The frame's
//    depth and multisample attachments are allocated and kept by
//    `FrameAttachments`; the color attachment is this example's own.
let color = create_color_target(
    &world.get::<wgpu::Device>(ctx.device).unwrap(),
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
let color_view = world
    .with_mut::<Source, _>(source, |source| {
        source
            .as_mut::<MeshSource>()
            .unwrap()
            .register_texture_and_default_view(&world, color)
            .1
    })
    .unwrap();
world
    .with_mut::<Renderer, _>(renderer, |r| {
        attachments.bind(&world, r, color_view);
        r.render(&world);
    })
    .unwrap();
```

UI 面板是行为组件，所以挂载一个界面就是一次普通的 spawn：

```rust
# #[cfg(feature = "ui")]
# {
use unlit3d::prelude::*;

let mut world = World::new();
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
- **`InputState` 持有「自上次清空以来到达的事件」以及事件留下的状态**（修饰键、指针
  位置与按下的指针、光标、按下的按钮、焦点、窗口尺寸与缩放）。指针的按下状态按
  「接触点」逐个记录（`PointerContact`，由种类与 id 标识），因此多指同时按下时抬起
  一根不会误判为手势结束。
- **分发由调用方在帧循环里显式调用**，不由 `render` 内部自动进行。原因：回调内排队的
  结构变更需要 `&mut world` 才能落地，而 `render` 只拿得到 `&World`；自动分发会
  让回调排队的变更延迟到下一次外部 `apply`，且这个延迟对用户不可见。显式调用也让调用
  方掌控分发时机与顺序，与「调用者即系统」一致。
- 分发**直接遍历**行为组件即可，不需要先收集实体：回调借用的是不同实体的不同 cell，
  互不冲突。真正的限制来自 ECS 自身有意为之的取舍——回调不能重入借用自己那个组件，
  也不能重入同类分发；要保留状态就放到兄弟组件上。

UI 对输入的**捕获**（是否想独占指针/键盘）按 egui 的语义需要上一帧的结果：源把这两个
标志写回世界，游戏逻辑可以读它决定是否响应。本期只提供数据，不做自动拦截。

</details>

## 测试

```text
cargo xtask test                # 整个工作区
cargo nextest run -p unlit3d    # 只跑本 crate
```

GPU 集成测试把场景渲染到离屏目标并检查回读的像素。这些场景的多帧快照覆盖现在位于
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)：
它的 `tests/gpu_scenes.rs` 把这些场景与
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
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