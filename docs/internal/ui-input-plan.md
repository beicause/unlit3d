# 实施计划：unlit3d 集成 UI（egui）并接入 winit 输入事件

> 状态：**已实施**。§9 的 12 个步骤全部实现、验证并提交（`main` 上 `0cb66c0..6ce33b7`，17 个提交；快照子模块 `wgpu_unlit_render_asset_files` 为 `a58efeb..70ae7a3`）。本文保留为设计记录，实施结果见 §13。
> 目标 crate：`unlit3d`（主体）、`wgpu_unlit_render`（UI 模块与 `Scene` 基础设施的小幅改进）。
> 前置阅读：`docs/DESIGN.md`（§上层API、§功能）、`crates/unlit_ecs/tests/behavior.rs`（行为组件范式）。
>
> **修订（一）**：§4 的「帧源与 GPU 上下文归 `Renderer` 所有」已被 **§4.8 取代**——改为把 `wgpu::Device`/`wgpu::Queue`/`ResourceGraph` 与帧源都放进 ECS，`GpuContext`/`FrameContext`/`SourceContext`/`with_source_mut` 随之**全部删除**。D2 已同步改写。§4.1/§4.3/§4.5/§4.7 中与本修订冲突的段落保留原文但已标注，以 §4.8 为准。
>
> **修订（二）**：下面的计划已全部实施完毕——§9 的 12 个步骤逐一标注「已完成」，D9/D10/D11/D13 均已定案。本文不再作为待办计划，而作为该设计的设计记录保留；实际结果、抉择落点与对本文的修正集中记在 **§13**。

---

## 1. 目标与范围

1. **UI**：让 `unlit3d` 能在同一帧、同一个 render pass 中，在 3D 绘制之后叠加 egui 界面。
   - 「UI 尽量在原有基础设施之上实现」：复用资源图（`ResourceGraph`）、`Scene` 绘制列表、内置 unlit 管线、`StagingBuffer`、`Renderer` 的帧循环；不新建一套平行体系。
2. **输入**：把 winit 的键盘 / 鼠标 / 触摸 / IME / 焦点等事件接入 ECS。
   - 事件回调是**包含闭包函数的行为组件**（与 `unlit_ecs/tests/behavior.rs` 中的范式一致）。
   - 触发事件时，按事件类别调用**所有**对应行为组件上的闭包。
   - UI 与游戏逻辑共用同一条输入流，避免两套事件。
3. **基础设施改进**（按需）：
   - `wgpu_unlit_render::scene`：让 `Scene` 拥有自己的 `wgpu` 句柄（去掉生命周期与 `launder` 洗白），使**多个 `Scene` 共存**、按顺序录制成为可能（见 §3、D1）。
   - `unlit3d::Renderer`：退化为纯帧级装置，**可挂载多个帧源**（`FrameSource`），每个源自带状态与自己的 `Scene`；`render` 顺序迭代 `Scene` 录制（见 §4、D2）。
   - **内置 mesh 渲染本身变成一个帧源**（`MeshSource`），与 UI 完全对称，渲染器里不留任何 mesh 专用绘制路径（见 §4.4）。
   - `wgpu_unlit_render::ui`：修掉 `TextureOptions` 未接线与 `scissor_rect` 漏乘 ppp 两个缺陷、放宽 `scene()` 的借用、把 UI 变体对渲染目标的特化做成可直接复用的入口；`depth_stencil` 改为 `Option`（D5）。
4. **示例**：更新 `unlit3d_examples`，演示「挂载 `MeshSource` + 挂载 `UiSource` + 若干 `UiPanel` 实体 + 键盘/指针行为组件」。

**非目标（本期不做，列入后续）**：多窗口 / 多 viewport、egui 的剪贴板与光标图标回写、AccessKit 无障碍、`ViewportCommand`、`wasm32` 下 IME、输入重映射配置、手柄 / 触摸手势的完整覆盖。

---

## 2. 现状（已核对的关键事实与位置）

### 2.1 渲染与帧结构

| 事实 | 位置 |
|---|---|
| `Renderer::render(&mut self, world)` 自己建 encoder、上传、组装场景、开 pass、提交 | `crates/unlit3d/src/renderer.rs:1184` |
| 无相机时提前返回（只做 clear），因此**当前没有「无相机但有 UI」的路径** | `crates/unlit3d/src/renderer.rs:1206-1210` |
| 无可见网格时也提前返回 | `crates/unlit3d/src/renderer.rs:1229-1233` |
| 渲染目标由 `color_view/depth_view/msaa_view` 三个 `ResourceId` 描述，`SurfaceKey` 由 `set_render_target` 缓存 | `renderer.rs:546`、`renderer.rs:1717` |
| 帧组装把图里的 bind group / buffer **克隆进 `self.*_cache`**，`Scene` 借用这些 cache | `renderer.rs:1243-1363`、`crates/unlit3d/src/scene.rs:391` |
| `Scene` 为了跨帧复用，用 `launder` + `recycle`/`reborrow` 做生命周期洗白 | `crates/wgpu_unlit_render/src/scene.rs:321-364` |
| `Device`/`Queue` 是 `Clone`（Arc）共享句柄，`PartialEq` 按身份 | `wgpu-30.0.1/src/api/device.rs:20-26`、`api/queue.rs:21` |
| 资源图的移除/清理入口 | `crates/wgpu_unlit_render/src/resources.rs:400-476` |

### 2.2 `wgpu_unlit_render::ui` 现状

`EguiIntegration`（`crates/wgpu_unlit_render/src/ui.rs:223`）已经是一个**不依赖 ECS、不依赖 winit**的纯后端：
`new(device, global_group: ResourceId, pipeline)` → `update(graph, queue, encoder, ctx, output, ppp)`（`:319`）→ `scene(&mut self, graph: &mut ResourceGraph) -> Scene`（`:413`）。
它把 UI 的纹理/采样器/材质绑定组/顶点索引缓冲都注册进调用方的 `ResourceGraph`，这正是「复用原有基础设施」的基础。

已发现的问题：

1. **`TextureOptions` 未接线（功能缺陷）**：`upload_geometry` 里每个 `UiDraw` 硬编码 `options: egui::TextureOptions::default()`（`ui.rs:391`），而 `epaint::ImageDelta` 自带 `options: TextureOptions`（epaint-0.36.2 `textures.rs`）。于是非默认采样方式（最近邻、Repeat、mipmap）的纹理会被错误地按默认线性采样。`sampler()`（`ui.rs:564`）已经是按 options 去重的，所以只需把 options 传到 draw 上。
2. **`scissor_rect` 漏乘 `pixels_per_point`（HiDPI 缺陷）**：`scissor_rect(rect)`（`ui.rs:715`）直接把 `egui::Rect`（**逻辑点**）当物理像素用，既没乘 ppp 也没 round。对照 egui-wgpu 的 `ScissorRect::new`（`egui-wgpu-0.36.2/src/renderer.rs:1141`，本次通过 `gh` 取得）：
   ```rust
   let clip_min_x = (pixels_per_point * clip_rect.min.x).round() as u32;   // 转物理像素
   …clamp(0, target_size)…
   ```
   推论：ppp≠1 时 UI 的裁剪矩形会偏小（内容被裁掉一块）。`update()` 已经拿到 `pixels_per_point`（`ui.rs:326`），只是没往下传。
3. `scene()` 需要 `&mut self, &mut ResourceGraph`，但它在语义上只读（`build_material` 会惰性建绑定组，那属于 `update` 阶段该做的事），`&mut graph` 阻塞了「UI 场景与 3D 场景借用同一个图」的组合方式。
4. `ui_options(device, srgb)` 只给基础变体，调用方要自己填 `color_target.format` / `multisample.count` / `depth_stencil.format`（`ui.rs:144`、`ui.rs:152-188`）。缺一个「按 `SurfaceKey` 特化」的入口。
5. **深度状态不可选（已验证的硬缺口）**：`UnlitOptions.depth_stencil` 是非可选的 `wgpu::DepthStencilState`（`pipeline.rs:199`），而 `UnlitPipeline::new` 一律传 `depth_stencil: Some(...)`（`pipeline.rs:465`）。没有任何字段或 `Option` 能表达「这条管线不带深度状态」。
   核对 wgpu-core 30.0.1：`RenderPassContext::check_compatible`（`wgpu-core/src/device/mod.rs:126`）在 `set_pipeline` 时把管线的 `depth_stencil`（`Option<TextureFormat>`，由 `depth_stencil_state.as_ref().map(|s| s.format)` 得到，`device/resource.rs:5016`）与 pass 的附件逐项比较，**不等即报 `IncompatibleDepthStencilAttachment`**。所以「有深度附件的 pass + 声明了深度状态的管线」是匹配的，而「**无**深度附件的 pass + 声明了深度状态的管线」会验证失败。
   反过来，现状的 `apply_surface`（`pipeline.rs:717`）在 `depth_stencil_format == None` 时刻意保留基础格式：
   ```rust
   if let Some(depth) = surface.depth_stencil_format { options.depth_stencil.format = depth; }
   ```
   并有测试 `apply_surface_without_a_depth_attachment_keeps_the_base`（`pipeline.rs:971`）把这个「保留」钉死。也就是说：**今天没有任何路径能画出「无深度附件的 pass」**——只能靠 `Renderer::set_render_target` 的文档要求（`renderer.rs:527`）回避。
   UI 是第一个会真正撞上它的使用者：一个纯 2D 的 UI overlay 并不需要深度附件，而 `WindowSurface` 恰好总是创建深度附件（`winit.rs:409`），所以示例不会暴露这个缺口，但「用一个只有 color 的自定义 target 画 UI」会。

### 2.3 ECS 行为组件与借用语义（决定输入分发的写法）

来自对 `unlit_ecs` 的完整核对（`src/world.rs`、`src/query.rs`、`tests/behavior.rs`）：

- 行为组件 = 持有闭包 / 函数指针的普通组件；调用范式是
  `world.with_mut::<OnX, _>(entity, |b| b.run(&world, entity, &event))`。
- 查询迭代**只借用当前行的那一个 cell**，不借用 archetype 列表，也不借用其他组件类型的 cell。因此回调里读/写**别的**组件（哪怕同一实体）是安全的。
- 借用冲突是 **panic**，不是 `None`；`with_mut`/`get` 返回 `None` 只表示「实体不存在」或「没有该组件」。
- 回调里**不能**重入同一实体的同一组件类型（panic），也**不能**做结构变更（`spawn`/`despawn`/`apply` 需要 `&mut self`）——结构变更走 `world.queue()`，由外层 `world.apply()` 落地。
- 因此分发的正确姿势是：**先把目标实体收集成 `Vec<Entity>` 快照，再逐个 `with_mut` 调用**（`tests/behavior.rs:417-434` 就是这个模式）。

### 2.4 egui 0.36.2 的关键 API（已核对源码）

- 入口是 `Context::run_ui(RawInput, impl FnMut(&mut Ui)) -> FullOutput`（**没有 `Context::run`**）；`Output.shapes` 用 `Context::tessellate(shapes, ppp)` 变成 `Vec<ClippedPrimitive>`。
- 输入全靠 `RawInput.events: Vec<egui::Event>`；`RawInput` 没有 `modifiers` 字段。窗口态通过 `screen_rect`、`viewports[ROOT].native_pixels_per_point`、`time`、`focused`、`max_texture_side` 提供。
- `Event` 共 19 个变体：`Copy/Cut/Paste/Text/Key/ModifiersChanged/PointerMoved/MouseMoved/PointerButton/PointerGone/Zoom/Rotate/Ime/Touch/MouseWheel/WindowFocused/AccessKitActionRequest/Screenshot`。
- `TexturesDelta.set: ahash::HashMap<TextureId, SmallVec<[ImageDelta;1]>>`，`ImageDelta { image, options, pos }`。
- 输出侧需要处理（本期可选）：`PlatformOutput.commands`（剪贴板）、`cursor_icon`、`ime`、`viewport_output`。

### 2.5 winit 0.30.13 的事实

- `ApplicationHandler::window_event` 已有，示例自行实现；事件类型齐全：`KeyboardInput { event: KeyEvent, is_synthetic }`、`ModifiersChanged`、`Ime`、`CursorMoved/CursorEntered/CursorLeft`、`MouseWheel`、`MouseInput`、`Touch`、`Focused`、`Resized`、`ScaleFactorChanged`、`PinchGesture/PanGesture/RotationGesture`、`ThemeChanged`、`Occluded`。
- `KeyEvent { physical_key: PhysicalKey, logical_key: Key, text: Option<SmolStr>, location, state, repeat, .. }`。
- `unlit3d` 的 `winit` 是 optional feature（`crates/unlit3d/Cargo.toml`）。

---

## 3. 重构之一：`Scene` 拥有自己的句柄（§9 阶段 1 第 1 步）

### 3.1 动机

现在的 `Scene<'a>` 里每个 `DrawEntry<'a>` 借用 `&'a wgpu::RenderPipeline`、`&'a BindGroup`、`BufferSlice<'a>`。这带来三件事：

1. `Scene` 无法跨来源组合：3D 场景借 `renderer.*_cache`，UI 场景借 `graph`，两个生命周期间要精确排序。**多个 `Scene` 无法共存**——这是 §4「多 Scene 顺序绘制」方案的直接障碍（构建第 i+1 个源时需要 `&mut graph`，而第 i 个源的 `Scene` 还借着它）。
2. `Renderer` 被迫维护 4 个 handle cache（`bind_group_cache` / `buffer_cache` / `vertex_slot_cache` / `scene_cache`）和 `launder` 洗白，只为了让借用成立。
3. 第三方「往帧里追加绘制」这件事实际上做不到（拿不到合法生命周期）。

而 `Renderer::render` **每帧本来就在 clone** bind group 与 buffer 进 cache（`renderer.rs:1259-1307`），所以把句柄放进去并不增加 clone 次数——只是把 clone 从 cache 挪到 `DrawEntry` 里。

### 3.2 改法

`crates/wgpu_unlit_render/src/scene.rs`：

```rust
/// One vertex-buffer slot: the slot, the buffer and the byte range bound.
pub type VertexBufferBinding = (u32, wgpu::Buffer, Range<u64>);

pub struct DrawEntry {
    pub pipeline: wgpu::RenderPipeline,                            // owned
    pub bind_groups: ArrayVec<(u32, wgpu::BindGroup), MAX_BIND_GROUPS>,
    pub vertex_buffers: ArrayVec<VertexBufferBinding, MAX_VERTEX_BUFFERS>,
    pub index_buffer: Option<(wgpu::Buffer, Range<u64>, wgpu::IndexFormat)>,
    pub scissor: Option<ScissorRect>,
    pub stencil_reference: u32,
    pub range: DrawRange,
}

pub struct Scene { draws: Vec<DrawEntry> }   // no lifetime parameter
```

- `Scene::record` 在录制时重建 slice：`pass.set_vertex_buffer(slot, buffer.slice(range.clone()))`；`PassState` 的去重键从 `BufferSlice` 改为 `(wgpu::Buffer, Range<u64>)`（`Buffer`/`BindGroup` 都是 Arc 句柄，比较身份即可）。
- 构建器：`with_vertex_buffer(slot, buffer: wgpu::Buffer)`、`with_vertex_buffer_range(slot, buffer, range)`、`with_bind_group(index, group)`、`with_index_buffer(buffer, range, format)`。
- 删掉 `Scene::recycle`/`reborrow`/`launder`（`scene.rs:321-364`）。
- 因为每个 `FrameSource` 自持一个 `Scene`（§4），**不需要** `append`/`extend`：录制时按顺序对每个 `Scene` 调用一遍 `record` 即可。

`crates/unlit3d/src/renderer.rs`：删掉 `bind_group_cache` / `buffer_cache` / `vertex_slot_cache` / `scene_cache` / `entry_handle_cache` 中的句柄 cache 部分（`EntryHandles` 只保留计数与索引信息，或整体简化）；`assemble_scene` 直接产出 owned 的 `DrawEntry`。

### 3.3 代价与替代方案

- 代价：每次 `set_*` 去重比较时多一次 Arc clone（与现状相同）；`DrawEntry` 变大（几个指针），无堆分配这一点保留。
- **替代方案 B（已否决）**：保持借用式 `Scene`。代价不只是「UI 有特权」，而是 §4 的「多 `Scene` 顺序绘制」在借用规则下无法成立（见 D1）。

---

## 4. 重构之二：帧源（FrameSource）—— 内置 mesh 渲染与 UI 都建立在它之上（§9 阶段 1）

### 4.1 决定（采纳用户方案，取代原先的 FrameLayer）

**原方案（已弃用）**：把「图层」做成一种行为组件（`register_layer::<C>()` + `FrameLayer` trait，两阶段 `prepare`/`draw`）。它引入了「图层」这一新概念、一层泛型样板（`LayerOf<C>`/`AnyLayer`），而真正需要的**顺序保证**仍要靠 `Vec` 手工维护——概念成本大于收益。

**新方案（采纳）**：一帧由**多个帧源各自产出一个 `Scene`** 拼成，`render` 按顺序录制它们；**FrameLayer 概念取消**。`Renderer` 退化为纯帧级装置，不持有任何绘制逻辑。

**并且（用户补充，关键）**：**内置的 mesh 渲染本身就是一个帧源**（`MeshSource`），与 UI 完全对称。`Renderer` 里不再有任何 mesh 专用的绘制路径或特权字段；「3D 主场景」只是**第一个被录制的源**产出的场景，而不是渲染器的特例。这正是 `AGENTS.md`「内置实现不应拥有特权和内部专用实现」在帧结构上的落实。

分工（**归属见 §4.8**：帧源与共享上下文都是世界的成员，而不是渲染器的私有字段）：

| | 内容 |
|---|---|
| 帧级 | 附件与 `surface`、encoder/pass/submit、render target 的绑定状态 |
| 共享上下文 | `device`/`queue`/资源图（作为资源，供所有源与第三方取用） |
| 3D | `families`/`pipelines`/mesh 与 index/vertex 池/metadata/instance 缓冲/剔除与排序/其 `Scene` → `MeshSource` |
| 2D UI | egui `Context`/其 UBO/其 `Scene` → `UiSource` |

顺序由每个源的 `order()` **显式声明**（§4.6）：`MeshSource` 是 `FrameOrder::MESH`、`UiSource` 是 `FrameOrder::OVERLAY`，于是得到「3D 之后叠 UI」。顺序相同时按源显式携带的创建序号作 tie-break，并在此时 `log::warn!`。

### 4.2 这一方案要求 `Scene` 自持句柄（与 D1 强耦合）

这不是偏好问题，是**编译期约束**：

- 若 `Scene<'a>` 仍借用资源图（现状），则第 i 个源产出的 `Scene` 会一直借着 `&'a graph`；而第 i+1 个源的构建需要 `&mut graph`（注册纹理/缓冲）。两者不能同时存在 → 只能「产出一个、立刻录一个」，「全部产出完再统一录制」不成立。
- 3D 主 `Scene` 也在借用图，它会把后续所有源的构建全部堵死。
- 改成 `Scene` 自持句柄后（D1=A），**录制阶段完全不接触资源图**，于是「先全部产出，再统一录制」成立。

已用探针实测这一结构可编译可运行（`.tmp/src_probe`，真实 `wgpu` 设备 + 真实 encoder/pass，两个源 + 一个主 `Scene`）。

### 4.3 仍然成立的结论（类型草案见 §4.8）

完整的类型草案见 §4.8。这里只保留**仍然成立**的结论（原先那套 `GpuContext`/`FrameContext`/`FrameTarget`/`SourceContext` 借用句柄结构，以及 `Renderer::mount`/`SourceId`/`source_as`/`source_id_of`/`with_source_mut` 的方法形态，均已废弃）：

- **源的 setup 期只需要 `device`/`queue`/`graph` 三个帧级字段**（逐方法核对见 §4.5 的表），其余全是源自己的状态。所以共享上下文就是这三者，不多不少。
- **只有 `graph` 需要独占借用**；`device`/`queue` 是 `Clone`（Arc）共享句柄，只需 `&`。
- **源不必自己存 `device`/`queue` 的克隆**，构造只描述「这个源要什么」，不搬运句柄。
- **其余帧级字段不外露**：颜色/深度/MSAA 视图与 surface 是 pass 的状态；`load_ops` 每帧从世界读，不是渲染器字段；源集合自身不外露（源若要挂载别的源，应走延后命令队列，不能在遍历中改集合）。
- **`AnySource` 保留**，但退化为无方法 supertrait（`Any + FrameSource`），见 §4.8。

### 4.4 内置 mesh 渲染：`MeshSource`

`crates/unlit3d/src/mesh_source.rs`（或 `scene.rs` 旁），把现在 `Renderer` 里的 3D 部分整体搬进来：

| 从 `Renderer` 搬出 | 字段 |
|---|---|
| uniform 与缓冲 | `camera_buf`/`globals_buf`/`metadata_buf`、`globals`、`instance_buffer`/`instance_capacity` |
| staging | `camera_staging`/`globals_staging`/`metadata_staging`/`instance_staging` |
| mesh 存储 | `index_pool`/`index_pool_id`、`vertex_pool`/`vertex_pool_ids` |
| metadata | `metadata`/`free_metadata`/`metadata_capacity`/`metadata_dirty` |
| 管线 | `pipelines`、`families` |
| 每帧缓存 | `visible_meshes_cache`/`visible_cache`/`packed_instances_cache`、`pipeline_handle_cache`、`scene` |
| 搬出的方法 | `allocate_mesh`/`allocate_unlit_mesh`/`allocate_unlit_material`/`allocate_material`/`remove_mesh`/`remove_material`、`register_family`/`register_unlit_family`、`render_resources`/`rebuild_dirty_global_groups`、`upload_camera`/`upload_globals`/`upload_instances`/`upload_metadata`、`vertex_node`/`sync_vertex_node`/`sync_pool_node`/`ensure_instance_buffer`、`collect_and_sort_visible` |

`MeshSource::build_scene` 就是现在 `render` 里从「找 load_ops/camera」到「组装完 3D 绘制」的那一段（`renderer.rs:1184-1363`），改造后：

1. 读 `Camera`；**没有相机就把自己的 `scene` 清空并返回**（不再 early-return 整个帧——帧的 clear 由 `Renderer` 的 pass 完成）；
2. `globals` 推进、`upload_globals`/`upload_camera`/`upload_metadata`（图维护 `rebuild_dirty_global_groups` 在此，或提前到帧首）；
3. `collect_and_sort_visible`（无可见网格时 `scene` 清空，同样不再 early-return）；
4. `ensure_instance_buffer`/`upload_instances`，组装 `DrawEntry` 进 `self.scene`。

`Renderer` 的 `set_render_target`/`attachments`/`clear_pass` 留在帧级：它们描述的是**pass**，不是任何一个源的绘制。

### 4.5 源的字段归属（拓展 trait 与 `with_source_mut` 已废弃）

图既然是世界里的资源组件，setup 期就是普通的 ECS 访问（§4.8），因此原先设计的 `MeshSourceExt` 拓展 trait 与 `with_source_mut` 通用入口都没有存在理由。

仍然有用的是下面这张表：它逐方法核对了源在 setup 期**实际用到哪些帧级字段**，既确认共享上下文只需 `device`/`queue`/`graph`，也界定了「哪些字段搬进 `MeshSource`、哪些从上下文取」。

| 方法 | 用到的帧级字段 | 搬进 `MeshSource` 后 |
|---|---|---|
| `allocate_mesh`/`allocate_mesh_with_metadata` | `graph`、`queue` | 其余（`metadata`/`free_metadata`/`metadata_dirty`）是 `MeshSource` 自己的字段 |
| `allocate_unlit_mesh` | `device`、`graph`、`queue` | `index_pool`/`index_pool_id`/`vertex_node`/`sync_vertex_node` 是自己的 |
| `register_texture_and_default_view` | `graph` | **无自身状态** |
| `register_sampler` | `device`、`graph` | 无自身状态 |
| `allocate_material`/`allocate_unlit_material` | `device`、`graph` | 无自身状态 |
| `remove_mesh` | `graph` | `metadata`/`free_metadata`/`metadata_dirty`/`index_pool`/`vertex_pool` 是自己的 |
| `remove_material` | `graph` | 无自身状态 |

结论：**setup 期确实只需要 `device`/`queue`/`graph`**，其余全是源自己的状态。

### 4.6 源的绘制顺序：每个源必须显式声明，歧义时警告

**问题**。「挂载顺序 = 绘制顺序」把顺序绑死在**挂载的那一刻**，运行期无法调整；而且顺序的**意图**（哪些必须在前、哪些必须在后）在代码里看不出来，只能靠「先写 mount，后写 mount」隐式表达。

**已有的顺序语义（不能破坏）**：
- `Scene` 内：非 z-sorted 在前、z-sorted 在后，再按 `PipelineId`、再按材质键或深度（`unlit3d/src/scene.rs:306-321`）。
- `Scene` 之间：**目前只有录制顺序**。
- `ZSortedDrawing`（`components.rs:275`）与 `PipelineId`（`pipeline.rs:199`）是既有的两种「用户显式表达顺序」的先例。

**关键事实（已核对源码，修正了我先前的判断）**：scissor 与 stencil reference 都是 **pass 级状态**，`wgpu` 没有 reset 调用（`scene.rs:24-28`）。但 **`PassState` 是 `Scene::record` 的局部变量**（`scene.rs:282`），所以：
- **同一个 `Scene` 内部**：scissor 会向后泄漏，因此「被裁剪的绘制放最后」这条约定**只约束一个 Scene 内部的顺序**；
- **不同 `Scene` 之间**：每次 `record` 都从 `PassState::default()` 开始，所以**上一个源设的 scissor 不会泄漏到下一个源**。UI 的 `scissor` 只影响 UI 自己那个 `Scene` 内它之后的绘制。

**这个区分很重要**：源之间的顺序**不是**被 scissor 逼出来的硬约束，而是「UI 要合成在 3D 之上」的**语义要求**，正适合用显式的顺序值表达。

**决定：`FrameSource::order()` 是必需方法（不给默认实现），歧义时 `log::warn!`。**

```rust
/// Where a source draws, relative to every other source.
///
/// Lower values record first. Deliberately has no `Default`: every source
/// states its ordering intent, and a silent default is exactly what an
/// explicit order exists to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameOrder(pub i32);

impl FrameOrder {
    /// The built-in mesh source's order.
    pub const MESH: Self = Self(0);
    /// A source that composes over the meshes, such as a UI overlay.
    pub const OVERLAY: Self = Self(100);
}

pub trait FrameSource: 'static {
    // ...
    /// Where this source records, relative to the others.
    ///
    /// Required, not defaulted: a source that stayed silent would be ordered
    /// by mount position alone, which is the implicit behaviour this exists to
    /// replace. Use [`FrameOrder::MESH`], [`FrameOrder::OVERLAY`], or a value
    /// of your own.
    fn order(&self) -> FrameOrder;
}
```

**为什么去掉默认实现**（用户要求）：
- 默认值会让「忘记写 `order`」和「确实想用挂载顺序」无法区分，于是顺序意图又变回隐式——正是要消除的问题。
- `FrameOrder` **不实现 `Default`**，`FrameSource::order` **必需**，两处一起确保每个源都表态。已实测：去掉默认实现后，不写 `order` 的源**编译不过**，不会静默退化。
- 代价：每个源多写一行。对一个「排序有正确性含义」的机制，这是合理的显式成本。（自定义源若真的只想要挂载顺序，可以 `fn order(&self) -> FrameOrder { FrameOrder::MESH }` 之类显式表态，但那是**写明**的意图。）

`Renderer` 侧（**§4.8 修订后：挂载即 `spawn`，没有 `mount`/`SourceId` API**）：
```rust
// 挂载就是生成一个源组件实体。
let entity = world.spawn((Source(Box::new(MeshSource::default())),));
// 覆盖声明的 order（等价于原来的 mount_at）：spawn 时把字段设成给定值。
// 运行期改 order（等价于原来的 set_order）：改源自己的字段，下一帧生效。
```

**排序与歧义警告**。`order` 是 `Source` 组件的字段（默认取自源自己的 `order()`，可被覆盖）。**注意 §4.8 代价 2**：ECS 行序会因 `despawn` 的 `swap_remove` 而重排，所以**不能拿查询顺序当挂载顺序**；排序键是 `(order, mount_index)`，`mount_index` 是 spawn 时赋的单调递增 `u64`：

```rust
fn record_order(world: &LocalWorld) -> Vec<Entity> {
    let mut sources: Vec<(FrameOrder, u64, Entity)> = world
        .query::<&Source>()                       // 列里带 order 与 mount_index
        .map(|(entity, s)| (s.order(), s.mount_index, entity))
        .collect();
    sources.sort_by_key(|(order, mount_index, _)| (*order, *mount_index));
    warn_ambiguous_order(&sources);               // 见下
    sources.into_iter().map(|(_, _, e)| e).collect()
}
```

**每帧重算即可，不再需要 `order_dirty`/缓存**（§4.8 的连带简化）：源是组件，增删是普通的结构变更，渲染器无法在挂载时被通知；但源的数量是个位数量级，`sort_by_key` 每帧跑一次的开销可忽略，而缓存需要一套失效登记（原设计的 `order_dirty` 正是这种登记）。**取舍**：若源数量将来变大，可按「世界里源的数量是否变化」跳过重排。

歧义的定义与处理（**用户确认**）：
- **歧义 = 两个及以上源声明了相同的 `FrameOrder`**。此时先后仍由 **`mount_index`** 静默决定（`sort_by_key` 稳定），但这些源的相对次序其实是**声称出来的巧合**，不是设计意图。
- **只警告，不 panic**：相同 `order` 有时是合法的（例如两个确实等价的叠加层），强制唯一会把用户逼到编造无意义的数字。用它来**提示**而不是**禁止**。
- **警告内容**要能直接定位问题：同一组的**源数量**、那个 **`FrameOrder` 值**、以及该组各源的 **`Entity`**（按 `mount_index`）。形如：
  ```
  2 sources declared the same FrameOrder(0): entities [4v1, 5v1]; recording them
  in mount order — give them distinct orders to choose explicitly
  ```
- **不要在稳态帧刷屏**：`warn_ambiguous_order` 自身按「上次警告的歧义签名」去重（记住上次各歧义组的 `(order, Vec<Entity>)`，相同就不重复输出）。源集合变化（spawn/despawn/改 order）会让签名变化——那正是**该**重新警告的时刻。
- **歧义解决后再引入会重新警告一次**；把 `order` 设成**相同值**是 no-op、不改签名、不重复警告（`.tmp/ord2_probe` 已实测）。
- `despawn` 一个源也会改变源集合，因此同样触发重算与警告检查（`ord2_probe` 对 `unmount` 的实测结论等价适用）。

**通过 `log` facade 输出**（用户选择）：
- 用 `log::warn!`，不用 `eprintln!`。理由：库的标准做法，可被应用侧的 logger 过滤/收集；`log 0.4.34` **已在依赖图中**（wgpu/egui/epaint/naga 都依赖它），所以这是**零新增编译成本**的既有依赖，只需在 `crates/unlit3d/Cargo.toml` 加一行显式依赖。
- **这条选择还与一条新加的 workspace lint 一致**（已实测）：工作区根的 `[workspace.lints.clippy]` 现在含 **`print_stderr = "warn"`** 与 **`print_stdout = "warn"`**，而所有 5 个 crate 都 `[lints] workspace = true`。所以**在库代码里用 `eprintln!` 会直接产生 clippy 警告**——`log` facade 不只是风格偏好，而是唯一不违反该 lint 的库内日志方式。
  - 现有 3 处 `eprintln!` 已经会触发该 lint：`unlit3d_examples/src/main.rs:193/236`（示例二进制）与 `wgpu_unlit_test_util/src/lib.rs:318`（在非默认的 `snapshot` feature 之后，测得 `--features snapshot` 时触发）。它们属于「示例/测试工具」而非库 API，本计划**不改造**它们；实施时若要让工作区 `cargo clippy` 干净，需要单独决定（加 `#[allow(clippy::print_stderr, reason = "...")]` 或改用 `log`）。**这不是本期 UI/输入任务的必需项**，但会被 `cargo clippy` 报出来，先记在这里以免误以为是本次改动引起的。
    - **修正**：实施时核对发现工作区成员里并没有 `eprintln!`，各处（含示例与测试工具）早已使用 `log`；见 §13。
- 测试可见性：`log` 是 facade，需要测试里装一个捕获 logger 才能断言（`.tmp/ord2_probe` 就是这么做的，用 `log::set_boxed_logger` + 自定义 `Log` 实现收集到 `Vec<String>`）。**注意**：`log::set_boxed_logger` 需要 `log` 的 **`std`（或 `alloc`）feature**；库本身只用 `log::warn!`，用默认 feature 即可，但**测试/示例若要装捕获 logger，需要开 `features = ["std"]`**。

**为什么不做「源之间显式依赖」**：`order` 是值的全序，简单、可预测、易调试；依赖图对「一层 UI 叠在 3D 上」这个需求过重。若将来需要，`Vec<SourceId>` 的顶层顺序也能用 `set_order` 表达。

**顺序的其他表达途径（保留原样，不冲突）**：
- 源**内部**的绘制顺序仍由它产出的 `Scene` 的 `draws` 顺序决定（`MeshSource` 内部那份排序逻辑不变）。
- 同一个源挂载两次（多实例）时，各自的 `order` 与挂载顺序一起决定先后；若两次都声明同一个 `order`，会触发上面的歧义警告。

**与 `load_ops` 的关系**：`load_ops` 是 pass 级的 clear，发生在**任何源录制之前**，与源顺序无关，因此不受影响。

**已实测**（`.tmp/ord2_probe`，7 条断言）：无默认实现时源必须表态；不同 order **零警告**；相同 order 恰好**一条**警告且含数量/值/ids；**连续 100 帧零日志**；歧义解决后不再警告、重新引入则再警告一次；三元歧义一条警告列全三个 id；`set_order` 设成相同值是 no-op 且不警告。


### 4.7 `Renderer::render` 的重构

**§4.8 修订后**：`ctx` 不再是装着借用句柄的 `FrameContext`，而是一个只装 `Entity` id 的 `Copy` 句柄；源自己从世界取图与 device。

```
render(world):
  1. 读帧级 load_ops；建 encoder（本帧唯一一个）
  2. 组装本帧 target（surface + attachments 的 width/height）
  3. 【构建阶段】for src in query::<&mut Source>() { src.build_scene(world, ctx) }
       源在自己的 build_scene 里 get_mut::<ResourceGraph>(ctx.graph) 取图
     ← 无任何借用残留：ctx 只是 id，encoder 是局部变量
  4. 【录制阶段】按 (order, mount_index) 排序后录制：
         for e in record_order(world) { with Source(e).scene() -> record(&mut pass) }
  5. submit
```

关键修正：
- **两处 early-return 消失**（原 `renderer.rs:1206`、`renderer.rs:1229`）。帧**总是**录制并按 `load_ops` clear，有没有相机、有没有可见网格、有没有源都一样；「无相机只 clear」这个既有行为由「所有源都产出空场景」自然得到（测试 `a_frame_without_a_camera_only_clears`（`renderer.rs:2727`）应继续通过）。这同时修掉了「纯 UI 应用画不出东西」。
  - 注意措辞：这里要去掉的是「无内容就提前返回」，**不是**要规定「一帧永远只能有一个 pass」。本期把内置 mesh 与 UI 录进同一个 pass，是因为二者本就该在同一 pass 合成；将来某个源要自己的 pass（如阴影、后处理）不应被这条挡住。
- **录制顺序由 `order` 决定**（§4.6）：`MeshSource` 用 `FrameOrder::MESH`、`UiSource` 用 `FrameOrder::OVERLAY`，所以「3D 之后叠 UI」是**声明**出来的，不靠 spawn 语句的先后。相同 `order` 时退回 `mount_index`，并 `log::warn!` 提示。
- **顺序每帧重算**（§4.6）：源是组件，增删不会被渲染器在挂载时感知；源数量是个位数量级，排序开销可忽略。
- 借用安全：第 3 步每个源各自短命借用图（`build_scene` 返回即归还），来源之间不共享借用；第 4 步只读各源的 `Scene`，完全不碰图（D1=A 后 `Scene` 自持句柄）。
- **`rebuild_dirty_global_groups`（`renderer.rs:1482`）随 `pipelines` 一起进 `MeshSource`**；它在 `build_scene` 内、在使用全局组之前完成即可，不必再提到帧首。



### 4.8 帧源与 GPU 上下文放进 ECS（本节为归属设计的权威说明）

**问题（用户提出）**。为什么不把帧源、以及 `wgpu::Device`/`wgpu::Queue`/`ResourceGraph` 这些 GPU 上下文直接放进 ECS？那样源就是普通实体/组件，图与 device 就是资源组件，源在自己的 `build_scene` 里 `world.get_mut::<ResourceGraph>(graph_entity)` 自己取——于是 `GpuContext`/`FrameContext`/`SourceContext`、`with_source_mut`、以及 `MeshSourceExt` 拓宽 trait **全都不需要存在**。它们存在的唯一理由是「图私有在 `Renderer` 里，而源也在 `Renderer` 里，取图就得拆自己的借用」。

**原先否决它的理由（已作废）**。§4.1 里写「把源的状态放进世界，会导致『源每帧要 `&mut ResourceGraph`，又要从世界查询自己』的双重借用纠结」。这条**经实测不成立**：

- 图与源是**不同的 cell**，一个 `get_mut::<ResourceGraph>(graph_entity)` 与一个 `query::<&mut Source>()` 可以同时存在；
- 更彻底地说，**源根本不必被递进图**——它自己在 `build_scene` 里取即可。此时驱动器只持 `&world`，连「拆借用」这个动作都不存在，也就没有 `with_source_mut` 的必要。

**实测证据**（`.tmp/borrowprobe`、`.tmp/selfprobe`、`.tmp/cmdprobe`、`.tmp/ordprobe`，均用真实 `unlit_ecs`）：

| 探针 | 验证内容 | 结果 |
|---|---|---|
| `borrowprobe` A | `Renderer` 拆字段借用（现行 `with_source_mut` 形状），`&mut T` + `&mut Graph` 同时给出；类型不符返回 `None` 不 panic | OK（可作对照基线） |
| `borrowprobe` B | 图是资源组件、源是组件，驱动器**持有** `&mut graph` 的同时 `query::<&mut Source>()` | OK（不同 cell） |
| `borrowprobe` C | 驱动器**持有** `&mut graph` 时再嵌套取一次图 | PANIC（`already borrowed while it is being write`）——**故 B 可行的前提是源各自取图，而不是驱动器握着图发下去** |
| `selfprobe` | 图/device 都是资源，**源自己**在 `build_scene` 里取；驱动器只持 `&world`，另有一个源在同一函数内驱动 `UiPanel` 行为组件 | **完全可行，无任何借用冲突** |
| `cmdprobe` | 排队一条命令：先按 `root` 释放源在图里的节点，再 `despawn` 源 | OK（`queue()` + `apply()`） |
| `ordprobe` | `despawn` 后行序是否稳定 | **不稳定**：`[a,b,c]` 删 `a` → `[c,b]`（`swap_remove` 把末行填进洞） |

**决定：采纳用户方案——`device`/`queue`/`graph` 作为资源进 ECS；帧源也作为组件进 ECS。**

**但要把两件事分开说清**，因为实测表明它们的收益不同：

- **把 GPU 上下文放进 ECS**：**这才是消掉 `with_source_mut` 的那一步**，且与源放在哪**无关**。理由见下（`hybrid` 选项 B 已实测）。
- **把帧源也放进 ECS**：这是**另一个独立选择**，不是消掉 `with_source_mut` 的必要条件。它有自己的收益（生命周期、扩展性）与代价（见「代价」一节）。用户方案里两者都提了，本计划两者都采纳，但**不要把后者的收益记在前者的账上**。

**为什么「上下文进 ECS」就足以消掉 `with_source_mut`**（这是本轮最关键的修正）：

`with_source_mut` 之所以必须存在，唯一原因是 **`graph` 是 `Renderer` 的私有字段且要 `&mut`**：想让调用方同时拿到 `&mut source` 和 `&mut graph`，就只能把 `&mut self` 拆成两个字段借用（`let Self { sources, graph, .. } = self`）。一旦 `graph` 不再住在 `Renderer` 里、而是通过 `&LocalWorld` 取（`world.get_mut::<ResourceGraph>(ctx.graph)`），这个冲突**根本不存在**，于是：

```rust
// 实测可行的写法（.tmp/hybrid 选项 B）：没有任何借用拆分
struct Renderer { context: RenderContext, sources: Vec<Box<dyn AnySource>> }

impl Renderer {
    fn source_as_mut<T: FrameSource>(&mut self, i: usize) -> Option<&mut T> {
        let any: &mut dyn Any = &mut *self.sources[i];
        any.downcast_mut::<T>()
    }

    /// setup 期：源来自渲染器，图来自世界，两者可同时持有。
    fn allocate_mesh(&mut self, world: &LocalWorld) {
        let ctx = self.context;              // RenderContext 是 Copy（只装 Entity id）
        let mesh = self.source_as_mut::<MeshSource>(0).unwrap();
        mesh.allocate_unlit_mesh(world, ctx);
    }
}
```

关键点是 **`RenderContext` 只装 `Entity` id（`Copy`）**，不是装着 `&mut graph` 的借用句柄。所以「先把它拷出来」是零成本的，之后 `&mut self.sources` 与 `&world` 互不相干。**这也顺带说明：`GpuContext` 那个「`&`+`&mut` 混合的借用句柄」本身就是问题来源；换成 id 就不需要拆借用了。**

**（诚实说明）`with_source_mut` 并不是「绕过借用检查」**。它是普通的字段解构，恰恰是**恢复**编译器借用检查的手段，没有 `unsafe`、没有 `RefCell`、没有内部可变性。所以本节的正确表述是：**它不是被「绕过」了，而是变得不再必要**——一个不再需要的公开 API 按 AGENTS.md「没用的 API 要及时删掉」应当删除，仅此而已。

**为什么仍然采纳「帧源也进 ECS」**（独立于上面的收益）：

1. **符合 AGENTS.md 的「不给内置功能特权」**。源若由 `Renderer` 私有持有，则「挂载/卸载/替换源」只有渲染器能做，第三方要插自己的源必须经 `mount`；源作为组件后，第三方加源就是 `world.spawn`，与内置 `MeshSource` 完全同权。
2. **符合 `unlit_ecs` 的行为组件范式**。`InputState` 已是资源实体、`UiPanel` 已是行为组件；帧源作为组件是同一套约定。
3. **setup 期 API 直接简化**：类型化访问退化为 `world.get_mut::<Source>(e)` + downcast（见下），不再需要 `mount`/`SourceId`/`source_as`/`source_id_of` 这一整套簿记。

**落在 ECS 里的形状**：

```rust
// 帧级 GPU 上下文：资源实体，外加一个只装 id 的发现入口
world.spawn((Resource, device));    // wgpu::Device：Clone 的 Arc 句柄
world.spawn((Resource, queue));     // wgpu::Queue
world.spawn((Resource, graph));     // ResourceGraph（!Send，正好落在 !Send 世界）
world.spawn((Resource, RenderContext { device, queue, graph }));  // 全是 Entity，Copy

// 帧源：组件。dyn 装在包装结构里，这样一个查询就能驱动所有具体类型。
world.spawn((Source(Box::new(MeshSource::default())),));
world.spawn((Source(Box::new(UiSource::new())),));
```

```rust
/// A frame source, behind an erased handle so one query sees every concrete
/// source type.
///
/// Method-free on purpose: `Any` supplies downcasting, the `FrameSource`
/// supertrait supplies the trait API, and the blanket impl means implementors
/// write no boilerplate at all.
pub trait AnySource: Any + FrameSource {}
impl<T: FrameSource> AnySource for T {}

/// The ECS component.
///
/// The trait API is reachable directly (`source.0.order()`); typed access
/// upcasts to `dyn Any` — `&mut *source.0 as &mut dyn Any` — and downcasts.
pub struct Source(pub Box<dyn AnySource>);

pub trait FrameSource: 'static {
    /// Assemble this frame's [`Scene`], fetching what it needs from `world`.
    ///
    /// `ctx` carries only entity ids, so it never conflicts with the source's
    /// own borrow of `world`.
    fn build_scene(&mut self, world: &LocalWorld, ctx: RenderContext);

    /// The scene built by the last [`FrameSource::build_scene`].
    fn scene(&self) -> &Scene;

    /// Where this source records, relative to the others. Required. (§4.6)
    fn order(&self) -> FrameOrder;
}
```

**已实测**（`.tmp/hybrid`，真实 `unlit_ecs`）：① `Source(Box<dyn AnySource>)` 组件能被**单个** `query::<&mut Source>()` 驱动，两个**不同具体类型**的源（`MeshSource`/`UiSource`）都正确构建（`graph: ["mesh_buffer", "ui_texture"]`）；② 排序经 `s.0.order()` 读到 `0`/`100` 并正确分派——**trait 方法在 `dyn AnySource` 上直接可用**；③ 类型化访问经 `&mut *source.0 as &mut dyn Any` 上转再 `downcast_mut::<MeshSource>()` 成功（追加 `"wireframe"` 后读回），**类型不符返回 `None` 不 panic**。

两点实现细节（都已在探针里踩到并确认）：
- **`AnySource` 必须是 `Any + FrameSource` 的无方法 supertrait**。若只写 `Any` 并加 `as_frame_source()` 之类的方法，调用方就得先 `as_frame_source().order()`，白白绕一层；若 `AnySource` 有方法却不是 `FrameSource` 的 subtrait，则 `Box<dyn AnySource>` 上**连 `order()` 都调不了**（探针实测的编译错误）。
- **依赖 trait upcasting**（`dyn AnySource → dyn Any`），Rust 1.86 起稳定，本仓用 1.98.1，可用。

**`Renderer` 还剩什么**：只剩「帧的装配」——建 encoder、按 `(order, mount_index)` 取各源的 `Scene` 按序录制、submit，以及 render target 的绑定状态（内置 mesh 与 UI 同属一个 pass，但这是它们的合成需求，不是对源的普遍限制）。它不再持有 device/queue/graph/sources 中的任何一个，退化为**几乎无状态**的帧驱动器。

**`mount`/`unmount`/`SourceId` 全部删除**：
- **挂载 = `world.spawn((Source(..),))`**；卸载 = `world.despawn(entity)`；稳定句柄 = `Entity` 本身（已是「索引 + generation」，比自造计数器更严谨）。
- **顺序调整**（原 `set_order`）= 直接改源自己携带的 `order` 字段；`order()` 仍提供默认来源。
- **`mount_at(order, source)`** = spawn 时把 `order` 字段设为给定值。

**必须处理的三个代价**（不能回避）：

1. **卸载要显式清理图节点**。`despawn` 没有钩子，所以「释放源在图里注册的节点」不能靠 `Drop`。对策：卸载走**一条排队命令**（`cmdprobe` 已验证：命令里先 `graph.remove_drop(root)` 再 `despawn`，`queue()` + `apply()` 落地）。这是「调用方即系统」的显式代价，与 `InputState::clear_events` 同类。
2. **ECS 行序不稳定，不能当挂载顺序**（`ordprobe`：`[a,b,c]` 删 `a` 得 `[c,b]`，`swap_remove` 把末行填进洞）。所以 `Source` 组件要带一个单调递增的 `mount_index: u64`，排序键取 `(order, mount_index)`；`Vec` 下标那种隐式顺序表达在 ECS 里不存在。
3. **类型化访问多一步 downcast**。源在 `Box<dyn AnySource>` 里，所以 `world.get_mut::<MeshSource>(e)` 取不到它，必须 `get_mut::<Source>(e)` 再上转为 `&mut dyn Any` 后 downcast。代价是一行 `(&mut *source.0 as &mut dyn Any).downcast_mut::<MeshSource>()`；换来的是一个查询能驱动异构源。

**与 §4.7 的关系**：`render` 的两阶段结构**保留**（构建全部源 → 按序录制）。「构建阶段」变为「对每个源调 `build_scene(&world, ctx)`，源自己取图」；「录制阶段」按 `(order, mount_index)` 逐源取 `scene()` 按序录制。**两阶段之间不残留任何 `&mut graph`/`&mut encoder` 借用**——`ctx` 只是 id，encoder 是局部变量。

**这一修订同时删掉 §4.5 的旧设计**：`MeshSourceExt` 的动机是「用户手里只有 `&mut Renderer`，`source_as_mut` 借走整个渲染器后拿不到图」。上下文进世界后，setup 期就是普通的 ECS 访问，两个借用分属不同 cell：

```rust
let ctx = *world.get::<RenderContext>(ctx_entity).unwrap();   // Copy
let mut graph = world.get_mut::<ResourceGraph>(ctx.graph).unwrap();
let mut source = world.get_mut::<Source>(mesh_entity).unwrap();
let mesh = (&mut *source.0 as &mut dyn Any).downcast_mut::<MeshSource>().unwrap();
mesh.allocate_unlit_mesh(&device, &mut graph, &queue, /* .. */);
```

`MeshSourceExt`、`with_source_mut`、`source_id_of` **全部删除**；「`renderer.rs` 里不出现 mesh 代码」这一目标，由「mesh 代码本来就在 `MeshSource` 组件里」自然达成。

**未决（D13）**：`RenderContext` 是否要带「本帧的 render target」（`SurfaceKey` + 物理尺寸）。倾向用**单独的每帧资源**由帧循环写入（D13 倾向 C），因为 `RenderContext` 应保持 `Copy` 且常驻，而 target 每帧可变。**实施到 §9 阶段 1 第 2 步时定。**

**验证状态**：本轮新增并实跑通过的探针：`.tmp/borrowprobe`、`.tmp/selfprobe`、`.tmp/cmdprobe`、`.tmp/ordprobe`、`.tmp/hybrid`（含选项 B：上下文进 ECS、源留在渲染器，仍不需要 `with_source_mut`）。

**本节取代的旧设计**：`GpuContext`/`FrameContext`/`FrameTarget`/`SourceContext`、`Renderer::mount`/`mount_at`/`set_order`/`unmount`/`source`/`source_as`/`source_id_of`/`with_source_mut`、`SourceId`、`on_mount`/`on_unmount`、`order_dirty` 缓存。它们的原文已从 §4.1/§4.3/§4.5/D2/D3/D7 中删除，理由记在本节，以免读者以为这些 API 仍需实现。



## 5. UI：`unlit3d::ui` —— egui 帧源（§9 阶段 2）

### 5.1 先改进 `wgpu_unlit_render::ui`

1. **接线 `TextureOptions`（修 bug）**：`apply_textures` 时把每个 `TextureId` 的 `ImageDelta.options` 记到 `HashMap<egui::TextureId, egui::TextureOptions>`，`upload_geometry` 用它填 `UiDraw.options`（`ui.rs:391`）。同时按 `TextureOptions` 建采样器（现有 `sampler()` 已经是按 options 去重的，保留）。
   - **实施后记**：本条修好后，既有的 `egui_ui.webp` 快照被有意重拍（旧图记录的正是被修掉的 bug），见 §13。
2. **`scene()` 放宽借用**：`pub fn scene(&self, graph: &ResourceGraph) -> Scene`；把「惰性建 material」从 `scene` 移到 `update`（`build_material` 需要 `&mut self` + `&mut graph`，正好在 update 阶段）。
3. **按 `SurfaceKey` 特化的入口**（供 unlit3d 复用，同时不剥夺自定义能力）：
   ```rust
   /// The UI variant's options for a frame that draws into `surface`.
   pub fn ui_options_for_surface(device: &wgpu::Device, srgb_to_linear_output: bool,
                                 surface: SurfaceKey) -> UnlitOptions;
   ```
   内部 = `ui_options` + `apply_surface`。
4. **深度状态可选化（见 D5）**：把 `UnlitOptions.depth_stencil` 改成 `Option<wgpu::DepthStencilState>`，使无深度附件的目标能拿到无深度状态的管线。这会波及内置 unlit 的默认值、`apply_surface`、以及 `unlit3d` 的测试基建；本期至少修 UI 路径。
   - **修正**：本条已按 D5 实施。实施 §5.1 期间另发现本文有两处判断有误（egui 键映射、工作区 `eprintln!` 数量），见 §13「对计划本身的修正」。
   - **关键约束（已实测，勿弄反）**：同一个 pass 内**所有**管线的深度声明必须与 pass 的附件**格式一致**，wgpu 只比 `Option<TextureFormat>`，**不看** `depth_write_enabled`/`depth_compare`。因此共用 pass（UI + mesh，且目标带深度）时，UI 管线**仍须声明相同的深度格式**，只靠 `write=false`+`compare=Always` 来不干扰 mesh 的深度；`None` 只用于**目标本身没有深度附件**的 pass，那时 mesh 管线也必须是 `None`。
   - 现有 `apply_ui_settings`（`ui.rs:180-181`）设的 `depth_write_enabled=false` + `CompareFunction::Always` **仍然正确**，不要改成 `None`；要补的是「它只在目标带深度（或只有 color）时分别给出对应设置」这一分支。
   - 与官方 `egui-wgpu` 一致（`crates/egui-wgpu/src/renderer.rs`）：其 `depth_stencil_format` 默认为 `None`，但一旦给出格式，就建出**同格式** + `depth_write_enabled: Some(false)` + `depth_compare: Some(Always)` 的状态——即官方同样把「UI 不测不写深度」与「管线不声明深度」分开处理。

### 5.2 `unlit3d::ui::UiSource`

#### 5.2.1 界面本身是**行为组件**，不是 `UiSource` 上的一个闭包字段

**修正**：初版让 `UiSource` 持有 `ui: Box<dyn FnMut(&LocalWorld, &mut egui::Ui)>`，那是**单块的、绕开 ECS 的**设计——一个 `UiSource` 只能有一个界面，界面的归属不在世界里，也不符合 `unlit_ecs` 的行为组件范式。改为：

**界面是组件 `UiPanel`，`UiSource` 只负责「逐一驱动世界里所有 `UiPanel`」。**

```rust
/// A UI panel: the interface itself, held as a behaviour component.
///
/// Mirrors `WorldCallback` in `unlit_ecs/tests/behavior.rs`, with the UI
/// surface as the second argument instead of the entity being run.
pub struct UiPanel(Box<dyn FnMut(&LocalWorld, Entity, &mut egui::Ui)>);

impl UiPanel {
    pub fn new(f: impl FnMut(&LocalWorld, Entity, &mut egui::Ui) + 'static) -> Self {
        Self(Box::new(f))
    }
}

impl UiSource {
    /// Run every entity that carries a [`UiPanel`], in query order.
    fn run_panels(&self, world: &LocalWorld) -> egui::FullOutput {
        self.ctx.run_ui(input, |ui| {
            for (entity, mut panel) in world.query::<&mut UiPanel>() {
                panel.run(world, entity, ui);
            }
        })
    }
}
```

这与 `unlit_ecs` 的约定完全一致（`behavior.rs:1-14`）：
- **闭包形态**：`Box<dyn FnMut(&LocalWorld, Entity, &mut Ui)>`，与 `WorldCallback` 同构，只是第三个参数是 UI 表面而非「被驱动的实体」。
- **调用方即系统**：`UiSource` 是「驱动器」，它在 `build_scene` 里调用这些组件；**调用哪个、什么顺序由它决定**（查询顺序 = 原型顺序）。
- **状态放兄弟组件**：面板要记状态（展开/收起、输入框内容）就放在**它自己的兄弟组件**里——行为组件在运行期间是被借用的，不能重入借用自己（`behavior.rs:12-14`、`a_behaviour_cannot_reborrow_its_own_component`）。
- **回调可读写世界**：面板拿到 `&LocalWorld`，可以 `get`/`query`，也可以 `queue()` 结构变更（由帧循环的 `apply()` 落地，§6.4）。`run_ui` 只借 `&self`（egui `context.rs:794`），闭包是 `FnMut(&mut Ui)`，所以面板内部访问世界**没有额外借用冲突**。

**收益**：多个面板 = 多个实体（各带自己的兄弟状态），可以按需 spawn/despawn；第三方可以定义自己的「面板类」组件（如 `UiOverlay`/`UiDebug`）并让 `UiSource` 通过过滤器选择——`UiSource` 本身不需要知道有哪些界面。

#### 5.2.2 `UiSource` 的字段

```rust
pub struct UiSource {
    ctx: egui::Context,               // 字体图集等状态，跨帧保留
    integration: EguiIntegration,     // 复用 wgpu_unlit_render::ui
    scene: Scene,                     // 本帧的 UI 绘制，跨帧复用同一分配
    camera: wgpu::Buffer,  camera_id: ResourceId,   // screen_view 用的相机 UBO
    globals: wgpu::Buffer, globals_id: ResourceId,  // 帧 globals UBO
    global_group: Option<ResourceId>, // 相机+globals 的绑定组
    surface: Option<SurfaceKey>,      // 当前管线特化对应的目标
    input: Option<Entity>,            // InputState 资源实体（见 D10）
    start: Instant,                   // RawInput.time
}
```

**注意没有 `ui` 闭包字段**——界面在世界的 `UiPanel` 组件里。这样 `UiSource::new()` **不需要参数**。

挂载（**§4.8 修订后：源是组件，`mount`/`SourceId` 已删除**；构造不需要 `&mut Renderer`）：

```rust
// 源本身：构造只描述「用哪个 egui 上下文」；UBO/管线在 setup 期经世界建（§4.8）。
world.spawn((Source(Box::new(UiSource::new())),));

// 界面：普通实体 + 行为组件。多面板就是多个实体。
world.spawn((UiPanel::new(|world, entity, ui| {
    egui::Window::new("panel").show(ui.ctx(), |ui| ui.label("hello"));
}),));
```

`UiSource` 若只想驱动**部分**面板（例如分层），用过滤器或先收集 id：

```rust
// `query_filtered::<Entity, With<C>>` 不取任何列，零借用代价。
let ids: Vec<Entity> = world.query_filtered::<Entity, With<UiPanel>>()
    .map(|(entity, _)| entity).collect();
```

**多趟（multi-pass）注意**：egui 的 `run_ui` 在某个 pass 调 `request_discard` 做多趟布局时，会**多次调用**闭包（`context.rs:770-771`、`:868`）。所以：
- 每次 pass 都从零重建查询迭代器是**正确**的（两版驱动形式都已实测，见 D12）；
- 但面板闭包会**跑多次**，因此它必须是**幂等**的，或把「本趟才该做的事」放在 `ctx.memory` / 兄弟组件里按趟数判断。这是 egui 的既有语义（`egui-wgpu` 等所有后端都如此），不是本设计引入的。

`build_scene` 流程（**§4.8 修订后签名**：`fn build_scene(&mut self, world: &LocalWorld, ctx: RenderContext)`——`ctx` 只装 `Entity` id，是 `Copy` 的）：
1. 从世界读输入资源（见 §6）拿到本帧事件、窗口尺寸（物理像素）、缩放因子、焦点；
2. 若本帧 target 的 surface != `self.surface`：重建 UI 管线（`ui_options_for_surface` → `UnlitPipeline::new`）和全局绑定组，用 `graph.replace` 换掉节点（沿用 `WindowSurface::resize` 的幂等替换写法）；
3. 组装 `egui::RawInput`：
   - `screen_rect = Some(Rect(min=0, size = 物理尺寸 / ppp))`
   - `viewports[ROOT].native_pixels_per_point = Some(ppp)`
   - `events = 本帧事件转换（§6.6）`
   - `time / focused / max_texture_side = device.limits().max_texture_dimension_2d`
4. `self.ctx.run_ui(input, |ui| { for (entity, mut panel) in ctx.world.query::<&mut UiPanel>() { panel.run(ctx.world, entity, ui) } })` → `FullOutput`——**逐一驱动世界里的 `UiPanel`**（§5.2.1）；
5. `integration.update(&mut graph, queue, encoder, &egui_ctx, output, ppp)`（上传纹理 + 顶点/索引，走帧 encoder；`graph`/`queue`/`encoder` 由 `ctx` 的 id 从世界取，见 §4.8）；
6. 写 `camera`（`ui::screen_view(viewport_points)`，viewport 用**点**）与 `globals`；
7. 把 UI 绘制取到自己的 `Scene`：`self.scene.clear(); self.scene.extend(self.integration.scene(&graph));`（`scene()` 在 §5.1 改成 `&self, &ResourceGraph`；D1=A 后 `Scene` 无生命周期，`extend` 可用，且 `self.scene` 的 `draws` 分配跨帧复用）；
8. `self.surface = Some(target.surface);`

`scene()` 只需 `&self.scene`（无参数、无借用）——这正是它成为 `FrameSource` 的直接收益。

**注册/释放**（§4.8 修订）：setup 期经世界注册 UBO 与全局绑定组节点；释放不靠钩子（`despawn` 没有钩子），而是一条排队卸载命令，先 `graph.remove_drop(root)` 再 `despawn`（`cmdprobe` 已验证）。**因此 `UiSource::new` 不接收渲染器**，与 `MeshSource::new` 一样是纯构造。

运行时改 egui 配置（`Context` 的 style/字体/zoom）：取到 `Source` 组件后 downcast 成 `UiSource`（§4.8 代价 3），不必重建源——这正是「源自持状态」的用处。**改界面**则是增删 `UiPanel` 实体。

### 5.3 与渲染器的交互点

- UI 需要「帧目标的物理尺寸」：来自 `Renderer::attachments().width()/height()`（`render_attachments.rs`）。**放哪见 D13**（倾向：单独的每帧资源，由帧循环在 `render` 前写入，而不是塞进 `Copy` 的 `RenderContext`）。
- UI 需要「sRGB」判定：`SurfaceKey.color_format.is_srgb()`（与 `WindowSurface::color_format` 的语义一致，`winit.rs:374`）。
- UI 的 `screen_view` 用**逻辑点**（已核对，不是猜测）：`epaint::ClippedPrimitive` 的文档明确写着 “Everything is using logical points”（epaint-0.36.2 `lib.rs:140`），且 `Tessellator` 里 `pixels_per_point` 只用于抗锯齿羽化（`feathering = options.feathering_size_in_pixels / pixels_per_point`，`tessellator.rs:1330`）与像素对齐取整（`round_to_pixels` = `(v * ppp).round() / ppp`，emath `gui_rounding.rs:68`），**从不缩放顶点坐标本身**。egui-wgpu 同样按点投影：其 WGSL `position_from_screen` 除以 `r_locals.screen_size`，而该 uniform 写的是 `screen_size_in_points = size_in_pixels / pixels_per_point`（`egui-wgpu-0.36.2/src/renderer.rs:134、925`）。
  因此：
  - `screen_view([width_px / ppp, height_px / ppp])`——这与现有 `gpu_ui.rs` 测试一致（其 `viewport` 常量是 `WIDTH/HEIGHT` 且 ppp 在测试里为 1.0，所以旧的 `screen_view` 刚好蒙对）；
  - 而 **scissor 必须乘 ppp**（见 §2.2 第 2 条），两者单位相反，是这套 API 最容易搞错的地方。
- `MeshSource` 的 `Globals` / metadata 等资源与 UI 无关，UI 自持 UBO，避免与 3D 相机共用同一 UBO（同一 pass 内不能有两个相机值）。两个源各有自己的 `Scene`，互不干扰。

### 5.4 feature 与模块

`crates/unlit3d/Cargo.toml` 已有 `egui` 依赖（非 optional）。建议新增 `ui` feature（**默认开启**），`ui = ["dep:egui", "wgpu_unlit_render/egui"]`，`egui` 改为 optional，把 `pub mod ui` 门控；`pub mod source` 与 `pub mod mesh_source`（若需要单独门控 `unlit`）不依赖 egui，常开。`input` 模块始终可用。取舍见 **D11**。

---

## 6. 输入：事件与行为组件（§9 阶段 3）

### 6.1 模块划分

```
crates/unlit3d/src/input/
    mod.rs        -- 事件类型、状态资源、行为组件、分发
    winit.rs      -- #[cfg(feature = "winit")] winit WindowEvent -> InputEvent 翻译
crates/unlit3d/src/ui/
    mod.rs        -- UiSource、UiPanel（行为组件）
    convert.rs    -- InputEvent -> egui::Event（与 ui feature 同门控）
```

事件类型**不依赖 winit，也不依赖 egui**：核心类型自持，winit / egui 的转换放在各自 feature 之后。

### 6.2 事件与状态类型（草案）

```rust
/// A key, addressed by physical position (layout-independent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key { Escape, Space, Enter, Tab, Backspace, Delete, ArrowUp, ArrowDown,
               ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, /* A–Z, 0–9, F1–F35, ... */
               Other(u32) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum PointerButton { Primary, Secondary, Middle, Back, Forward, Other(u16) }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)] pub struct Modifiers { pub alt: bool, pub ctrl: bool, pub shift: bool, pub mac_cmd: bool, pub command: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum TouchPhase { Started, Moved, Ended, Cancelled }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum WheelUnit { Line, Pixel, Page }

/// 一类事件；`InputEvent` 是全部事件的并集。
pub struct KeyEvent   { pub key: Key, pub pressed: bool, pub repeat: bool, pub modifiers: Modifiers }
pub enum PointerEvent {
    Moved { position: [f32; 2] },
    Button { position: [f32; 2], button: PointerButton, pressed: bool, modifiers: Modifiers },
    Left,
    Wheel { delta: [f32; 2], unit: WheelUnit, phase: TouchPhase, modifiers: Modifiers },
    Zoom(f32),
    Rotate(f32),
}
pub struct TouchEvent { pub id: u64, pub phase: TouchPhase, pub position: [f32; 2], pub force: Option<f32> }
pub struct ImeEvent { pub kind: ImeKind }   // Preedit { text, active_range } / Commit(String) / ...
pub struct TextEvent(pub String);

pub enum InputEvent {
    Key(KeyEvent), Pointer(PointerEvent), Touch(TouchEvent),
    Text(TextEvent), Ime(ImeEvent),
    ModifiersChanged(Modifiers), FocusChanged(bool),
}
```

### 6.3 状态资源

```rust
/// The frame's input: the events that arrived since they were last cleared,
/// plus the state they left behind.
pub struct InputState {
    events: Vec<InputEvent>,
    pub modifiers: Modifiers,
    pub cursor: Option<[f32; 2]>,
    pub buttons: bitflags,          // held pointer buttons
    pub touches: Vec<(u64, [f32;2])>,
    pub focused: bool,
    pub size_px: (u32, u32),
    pub scale_factor: f32,
}
impl InputState {
    pub fn events(&self) -> &[InputEvent];
    pub fn push(&mut self, event: InputEvent);   // winit 适配器用；同时更新状态
    pub fn clear_events(&mut self);              // 帧末调用
}
```

作为资源实体存在：`world.spawn((Resource, InputState::default()))`；`WinitInput` 适配器持有该 `Entity`。

事件列表**在一帧内被多个消费者读取**（分发器 + UI 源），因此设计为「只读遍历 + 帧末显式清空」，而不是「take 掉」。帧循环顺序（winit 保证输入事件先于 `RedrawRequested`）：

```
for each WindowEvent: winit_input.feed(world, input_entity, &event)   // push
RedrawRequested:
    input::dispatch_input(world)     // 调用行为组件闭包
    world.apply()                    // 落地行为组件排队的结构变更
    renderer.render(world)           // UI 源读取同一批事件
    input_state.clear_events()       // 清空（在 render 之后）
```

由谁调用见 **D4**。

### 6.4 行为组件与分发

行为组件（每个都提供 `new(closure)` 与 `run`，与 `tests/behavior.rs` 的 `OnFrame`/`OnInput` 完全同构）：

```rust
pub struct OnInput   (pub Box<dyn FnMut(&LocalWorld, Entity, &InputEvent)>);    // 所有事件
pub struct OnKey     (pub Box<dyn FnMut(&LocalWorld, Entity, &KeyEvent)>);
pub struct OnPointer (pub Box<dyn FnMut(&LocalWorld, Entity, &PointerEvent)>);
pub struct OnTouch   (pub Box<dyn FnMut(&LocalWorld, Entity, &TouchEvent)>);
pub struct OnText    (pub Box<dyn FnMut(&LocalWorld, Entity, &TextEvent)>);
pub struct OnIme     (pub Box<dyn FnMut(&LocalWorld, Entity, &ImeEvent)>);
```

分发算法（`pub fn dispatch_input(world: &LocalWorld) -> bool`，返回是否有事件被投递）：

```
1. 若找不到 InputState 资源：直接返回。
2. 取出本帧事件（所有权见 D6；见 §6.4.1 的实测结论）。
3. 对每个事件，按类别呼叫对应的行为组件（§6.4.1 给出「直接遍历 vs 收集实体」的结论与理由）。
4. 结束。结构变更由回调自身 world.queue() 排队，调用方在 dispatch 之后 world.apply()。
```

要点与边界（写进文档注释）：
- 回调内**不能**碰自己那个实体的同一个组件类型（`unlit_ecs` 的借用 conflict 是 panic）；`OnInput` 与 `OnKey` 同时挂在一个实体上互不冲突（不同 cell）。
- 回调内**也不能**重入同类分发（见 §6.4.1 实测第 4/5/6 条）。
- 「对应关系」表：

  | 事件 | 收到的组件 |
  |---|---|
  | `Key` | `OnKey`, `OnInput` |
  | `Pointer` | `OnPointer`, `OnInput` |
  | `Touch` | `OnTouch`, `OnInput` |
  | `Text` | `OnText`, `OnInput` |
  | `Ime` | `OnIme`, `OnInput` |
  | `ModifiersChanged` / `FocusChanged` | `OnInput` |

#### 6.4.1 为什么不能直接在遍历时调用？—— 实测结论

这是本计划里我原先写错、被实测纠正的一处，因此单独记录。**已用探针程序实测**（`.tmp/ecs_probe`，用 `catch_unwind` 区分 OK/PANIC，并单独编译验证借用检查）。

**先纠正一个广为流传的说法**：我最初以为「收集实体」是为了避开借用冲突。实测表明**不是**——直接遍历在绝大多数情况下完全可行：

| # | 场景 | 结果 |
|---|---|---|
| 1 | `for (e, mut b) in query::<&mut OnKey>()`，回调改**别的**组件（`Marker`） | **OK** |
| 2 | 直接遍历，`OnKey` 一轮、`OnInput` 一轮（一个事件喂两类组件） | **OK** |
| 3 | 回调内**嵌套**分发**别的**组件（`OnInput`） | **OK** |
| 7 | 直接把 `&mut` 查询的 **items** collect 进 `Vec`（再遍历） | **OK**：每个 item 借的是**不同实体**的 cell，互不冲突 |
| 9 | 一帧多个事件，直接遍历 | **OK** |
| 8 | 直接遍历 + 回调 `queue().despawn(self)` | **OK** |
| F/K | 直接遍历 + 回调 `queue().spawn` / 读资源实体 | **OK** |

而下面这些**两种写法都 panic**（所以「收集实体」并不能救它们）：

| # | 场景 | 结果 |
|---|---|---|
| C/5 | 回调读**自己**的 `OnKey`（`world.get::<OnKey>(self)`） | **PANIC** |
| E | 回调用 `query::<&OnKey>` 枚举同伴 | **PANIC** |
| J/4 | 回调内嵌套分发**同一**组件 `OnKey` | **PANIC** |
| — | 驱动器**持有** `Ref<InputState>` 时回调写 `InputState` | **PANIC** |

**那么「收集实体」到底买到了什么？** 只有一个、但很关键的区别：**调用方能否在两次回调之间 `apply()`**。

```rust
// 编译错误 E0500：closure requires unique access to `*world` but it is already borrowed
for (entity, mut b) in world.query::<&mut OnKey>() {
    (b.0)(&world, entity);
    world.apply();                 // ← apply 要 &mut world，而迭代器持有 &world
}

// 编译通过：两次 with_mut 调用之间没有借用存续
let keys: Vec<Entity> = world.query::<&OnKey>().map(|(e, _)| e).collect();
for entity in keys {
    let _ = world.with_mut::<OnKey, _>(entity, |b| (b.0)(&world, entity));
    world.apply();                 // ← OK
}
```
已实测：同一段 `apply` 放进 `for` 循环体内**编译失败**（E0500），放进 collect 后的循环体内**编译通过**。原因是 `QueryIter` 从 `&'w World` 派生并跨迭代存续，而 `with_mut` 的借用只在单次调用内。

**结论与本计划的决定**：
- **默认采用「直接遍历」**（`for (entity, mut behaviour) in world.query::<&mut OnKey>()`），因为它更短、无分配，且覆盖了全部实际需求（上表第 1/2/3/9 条）。原计划第 3 步的 collect 是**多余的**，删掉。
- **代价**：调用方不能在回调之间 `apply()`。对本计划无害——§6.5 的帧循环是「一帧分发完 → `apply()` 一次」，而不是「每个回调后 `apply()`」。这个限制要写进 `dispatch_input` 的文档注释。
- **唯一的例外**：若将来需要「每个事件（而非每帧）之后立刻落地结构变更」，那时再改为 collect。可以两种都提供：`dispatch_input`（直接遍历，默认）+ `dispatch_input_deferred`（收集实体 + 允许回调间 apply），但那属于过度设计，**本期不做**。

#### 6.4.2 `unlit_ecs` 是否需要改进？

**结论：本计划不需要改 `unlit_ecs`。** 实测证明当前 API 足以表达输入分发，且其限制是**有意的设计取舍**（`world.rs:22-25` 的文档：「借用冲突是 panic，不是编译错误……这是不做访问分析的代价」）。

具体逐条：

1. **「回调不能读自己那个组件」**——这不是缺陷，而是「行为组件被借用来运行」的必然结果，`tests/behavior.rs:307-317` 有专门测试钉住它（`a_behaviour_cannot_reborrow_its_own_component`），且文档明确说「要保留状态就放在兄弟组件里」（`behavior.rs:12-14`）。输入 API 只需照此约定：回调想记状态，放在 `OnKey` 之外的组件上。
2. **「回调不能重入同类分发」**——同上，属于同一取舍。
3. **「驱动器持有 `Ref<InputState>` 时回调不能写它」**——这条**会影响 §6.4 第 2 步的设计**：只要我们在遍历**之前**把事件 `Vec` 取出来（克隆或 take），`InputState` 的 cell 就不再被持有，回调可以安全读写它。这正是 D6 要选的。
4. **`apply` 与迭代互斥**——`apply` 需要 `&mut self` 是**正确且必需**的：结构变更会 `swap_remove` 移动行（`archetype.rs:99-106`），若允许在借用存续时发生，就有悬垂风险。用「编译期拒绝」换「无 unsafe」，值。
5. **可能的可选改进（不属于本计划，记录备用）**：`QueryIter` 若提供 `fn entities(&self) -> impl Iterator<Item = Entity>` 或一个「只取实体不借 cell」的 `EntityQuery`，则「收集实体快照」可以在遍历中完成，从而**同时**拥有直接遍历的简洁与回调间 `apply` 的自由。但实测第 9 条表明 `query_filtered::<Entity, With<C>>()` 已经能做到这一点（`With<C>` 只匹配 archetype、不借 cell，实测 OK）——所以**连这个改进都不必要**：

```rust
// 已经是「只取实体、零 cell 借用」的写法，无需改 ECS：
let keys: Vec<Entity> = world.query_filtered::<Entity, With<OnKey>>().map(|(e, _)| e).collect();
```
   唯一的小缺憾是它比 `query::<&OnKey>()` 多一层 `With` 包装，属可读性问题而非能力问题。

因此：**不改 `unlit_ecs`**；若用户希望把「实体快照」变成一等公民的便利写法，可另开一个小任务（例如给 `QueryIter` 加一个 `into_entities()` 适配器），但不阻塞本计划。

### 6.5 winit 适配器（`input/winit.rs`）

```rust
/// Feeds a winit window's events into the world's [`InputState`].
pub struct WinitInput { entity: Entity }        // 资源实体
impl WinitInput {
    pub fn new(world: &mut LocalWorld) -> Self;              // spawn (Resource, InputState)
    pub fn state(&self) -> Entity;
    /// Translate and push the events of `event` that concern input.
    /// Returns whether anything was pushed.
    pub fn on_window_event(&self, world: &LocalWorld, event: &WindowEvent) -> bool;
}
```

映射表（`WindowEvent` → `InputEvent` / 状态更新）：

| winit | 处理 |
|---|---|
| `KeyboardInput { event, .. }` | `event.state` → `pressed`；`repeat`；`logical_key`/`physical_key` → `Key`（**以物理键为准**，保证 WASD 与布局无关；`logical_key` 仅在 `Key::Character` 时用于 `Text`，而 `Text` 优先取 `KeyEvent.text`） |
| `ModifiersChanged(m)` | 更新 `InputState.modifiers` + 发 `ModifiersChanged` |
| `CursorMoved { position, .. }` | 更新 `cursor` + `PointerEvent::Moved` |
| `MouseInput { state, button, .. }` | 更新按下的按钮位 + `PointerEvent::Button`（位置取当前 `cursor`） |
| `MouseWheel { delta, phase, .. }` | `LineDelta`/`PixelDelta` → `PointerEvent::Wheel { unit, delta, phase }` |
| `PinchGesture { delta, .. }` | `PointerEvent::Zoom(delta as f32)` |
| `RotationGesture { delta, .. }` | `PointerEvent::Rotate(delta as f32)` |
| `Touch(t)` | `TouchEvent { id: t.id, phase, position: t.location.to_logical(scale).into(), force }` |
| `Ime(Ime::Preedit/Commit/DeleteSurrounding/Enabled/Disabled)` | `ImeEvent` |
| `Focused(b)` | 更新 `focused` + `FocusChanged` |
| `Resized(size)` / `ScaleFactorChanged { inner_size_writer, scale_factor }` | 更新 `size_px` / `scale_factor` |
| `CursorLeft` | `PointerEvent::Left` |

坐标统一为**物理像素、左上原点**（winit 的物理坐标），与 UI 的 `screen_rect` 换算（除以 ppp）在 UI 层做；这样游戏逻辑拿到的坐标与窗口像素一一对应。`WindowEvent::Resized` 与 `ScaleFactorChanged` 会同时更新 `WindowSurface`，两者互不干扰。

**示例的帧循环**（`unlit3d_examples/src/main.rs` 改造）：

```
window_event(event):  input.on_window_event(&scene.world, &event);   // 先喂输入
                      ... Resized 处理（相机侧面 + WindowSurface::resize）
RedrawRequested:      scene.draw()
about_to_wait:        request_redraw()
draw():               world.apply();                 // 落地上一帧排队的变更
                      input::dispatch_input(&world); // 调用行为组件
                      world.apply();                 // 落地回调排队的变更
                      renderer.render(&world);       // UI 源读同一批事件
                      input_state.clear_events();    // 帧末清空
```

### 6.6 egui 转换（`ui/convert.rs`）

`InputEvent` → `egui::Event` 的纯函数（可单测，无需 GPU）：

| 输入 | egui |
|---|---|
| `Key` | `Event::Key { key, physical_key: Some(..), pressed, repeat, modifiers }`；`pressed` 时若 `text` 非空再补一条 `Event::Text` |
| `Pointer::Moved` | `Event::PointerMoved(Pos2)`（**点** = 物理 / ppp） |
| `Pointer::Button` | `Event::PointerButton { pos, button, pressed, modifiers }`（含映射 `PointerButton`） |
| `Pointer::Wheel` | `Event::MouseWheel { unit, delta, phase, modifiers }` |
| `Pointer::Zoom/Rotate` | `Event::Zoom` / `Event::Rotate` |
| `Touch` | `Event::Touch { device_id: TouchDeviceId(0), id, phase, pos, force }`；**并按 egui 文档附加** `PointerMoved` / `PointerButton{Primary}` / `PointerGone` |
| `Text` | `Event::Text` |
| `Ime` | `Event::Ime(ImeEvent::Preedit/Commit/DeleteSurrounding)` |
| `ModifiersChanged` | `Event::ModifiersChanged` |
| `FocusChanged` | `Event::WindowFocused`；同时写 `RawInput.focused` |

同时设置 `RawInput.screen_rect`（点）、`viewports[ROOT].native_pixels_per_point`、`time`、`max_texture_side`。

> **修正**：计划曾以为 egui 的 `Key` 只有 6 个字母、其余字母只能靠 `Event::Text` 兜底；`egui-0.36.2/src/data/key.rs` 实际定义了全部 26 个字母 `A`–`Z`，映射覆盖全部字母（见 §13）。

### 6.7 与 UI 的「输入捕获」关系

egui 是否想独占指针/键盘，需要**上一帧**的结果：`Context::egui_wants_pointer_input()` / `egui_wants_keyboard_input()`（egui-0.36.2 `context.rs:2973/2986`）。

- `UiSource` 在 `build` 里把这两个标志写回世界（`InputState` 上的字段，或单独的 `UiCapture` 资源）。
- 游戏逻辑的行为组件可以读它决定是否响应。**本期只提供数据，不做自动拦截**；自动拦截（例如 UI 捕获时不再分发键盘）列为后续增强。

---

## 7. 示例改造（`unlit3d_examples`）

在现有立方体示例上增加：

1. `WinitInput` 资源 + `input::dispatch_input` 调用。
2. `world.spawn((Source(Box::new(MeshSource::new(...))),))` 与 `world.spawn((Source(Box::new(UiSource::new())),))`——各自用 `FrameOrder::MESH` / `FrameOrder::OVERLAY` 声明顺序（不靠挂载先后）。界面本身是**实体**：`world.spawn((UiPanel::new(|world, entity, ui| { ... }),))`，面板显示 FPS / 帧计数（状态放兄弟组件）、一个复选框控制立方体自转、一个滑块控制转速。**再 spawn 第二个 `UiPanel`** 演示多面板是多个实体。
3. 两个行为组件演示事件回调：`OnKey`（空格切换自转）、`OnPointer`（按住左键拖动改变相机方位角，读 `InputState.cursor`）。
4. 保持 `Esc` 退出（可继续由示例自己处理，或改成一个 `OnKey` 行为组件并 `event_loop.exit()`——后者需要行为组件能拿到 event loop，故示例里仍由 winit 分支处理）。

---

## 8. 验证与测试

本期以**单元测试**与 **GPU 快照集成测试**为主（用户要求）。现状基建已经就绪，不需要新建：

- 单元测试：`unlit_ecs/tests/behavior.rs` 是行为组件范式的样板（用 `Rc<Cell<..>>` 计数、断言调用次数与到达的实体）。
- GPU 快照：`crates/wgpu_unlit_test_util` 已提供 `assert_image_snapshot`（`lib.rs:290`，SSIMULACRA2 打分，`DEFAULT_MIN_SCORE = 85.0`，用 lossless WebP 存到 `tests/snapshots/`），`unlit3d` 的 dev-dependencies 已开 `features = ["snapshot"]`（`crates/unlit3d/Cargo.toml`），且 `crates/unlit3d/tests/snapshots/` 已存在（已有 `ecs_unlit_cube.webp`、`ecs_animated/`）。
- 读回像素与启发式断言：`common/mod.rs` 已 re-export `read_texture_bytes`/`texel_bytes`/`count_pixels_off_background`/`Frame`，`gpu_ecs.rs` 是用法样板；`bind_offscreen_target` 绑定离屏目标。

**排序原则**：能用「数值/计数断言」钉住的性质，就不要依赖快照（快照容忍度高、失败信息差）；快照只用于「整体观感」——即**投影正确、混合正确、纹理正确、遮挡正确**这类一图胜千言的性质。

### 8.1 `unlit3d` 单元测试（无需 GPU）

`crates/unlit3d/src/` 内的 `#[cfg(test)]`，与现有 `renderer.rs`/`scene.rs` 的测试同风格。

1. **`FrameSource` 机制**（§4.8、D2/D3/D6）
   - `mount_index` vs `order()`：显式 `order` 决定顺序；**同 `order` 内按 `mount_index`**（不是查询顺序——`despawn` 会重排行序，见代价 2）。
   - 改源的 `order` 字段下一帧生效；设**相同值**是 no-op。
   - 挂载 = `spawn`、卸载 = `despawn`：断言卸载后不再被录制，且卸载命令已把图节点 `remove_drop` 掉（图 `len()` 回落）。
   - 类型化访问：`get_mut::<Source>(e)` + `downcast_mut::<MeshSource>()` 命中返回 `Some`、**类型不符返回 `None`**（不 panic）。
   - 异构源：两个**不同具体类型**的源能被**同一个**查询驱动（`Source(Box<dyn AnySource>)` 的意义，`.tmp/hybrid` 已实测）。
   - 构建阶段「先全部 `build_scene`、再统一录制」这个两阶段顺序（用一个记录调用序列的假源断言）。
2. **顺序歧义警告**（§4.6、D8）
   - 需要装捕获 logger（`log::set_boxed_logger` + 自定义 `Log` 收集到 `Vec<String>`；**要求 `log` 的 `std`/`alloc` feature**）。断言：
     - 不同 `order` → **零**警告；
     - 相同 `order` → **恰好一条**，且含该组源数量、`FrameOrder` 值与各 `Entity`；
     - 连续多帧 → **不重复**警告（只在源集合/顺序变化时检查）；
     - 歧义解决后不再警告；重新引入再警告一次；
     - 三元歧义一条警告列全三个 id。
3. **`UiPanel` 驱动**（§5.2.1、D12）— 无需 GPU，用一个假的 UI 表面
   - 一个 `UiSource`（或等价的驱动函数）驱动**多个** `UiPanel` 实体，按查询顺序执行。
   - 面板经传入的 `&LocalWorld` 读到自己的**兄弟组件**；面板写兄弟组件成功。
   - 面板内 `queue().spawn` 在 `world.apply()` 后落地。
   - **多趟**：闭包被调用两次时，两种驱动形式（闭包内直接 `query` / 先收集 id）都仍访问到全部面板。
   - `query_filtered::<Entity, With<C>>` 的过滤生效（只驱动子集）。
4. **输入模块**（§6、D4/D8/D9）
   - `dispatch_input` 的**类别路由**：若干实体分别挂 `OnKey`/`OnPointer`/`OnTouch`/`OnText`/`OnIme`/`OnInput`，断言每个事件到达的实体与**次数**（仿 `behavior.rs` 的 `Rc<Cell<..>>` 计数）。
   - `OnInput` 收到全部事件；`OnKey` 只收 `Key`；两者在同一次分发中都被调用。
   - 空事件列表 / 无 `InputState` 资源 / 无行为组件 → 不 panic，`dispatch_input` 返回 `false`。
   - 回调 `queue()` 的结构变更在 `world.apply()` 后生效。
   - `clear_events` 后事件为空；`InputState` 的**状态字段**（modifiers/cursor/buttons/focused/size/scale）**不**被 `clear_events` 清掉。
   - `winit → InputEvent` 翻译（`#[cfg(feature = "winit")]`）：构造 `WindowEvent` 直接断言，覆盖物理键与逻辑键、修饰键、滚轮两种 delta（Line/Pixel）、触摸、IME、焦点、`ScaleFactorChanged`。
5. **`ui/convert.rs`：`InputEvent → egui::Event`**（§6.6）
   - 纯函数断言。特别覆盖**触摸必须附带 pointer 事件**（`PointerMoved` + `PointerButton{Primary}` + `PointerGone`），否则 egui 收不到触摸点击。
   - `Key` → `egui::Key` 只映射 egui 有的键；字母靠 `Event::Text`（egui 的 `Key` 只有 B/L/M/N/Q/Y 六个字母，`key.rs:123-146`）。
   - **修正**：括号里这条判断是错的——`egui-0.36.2/src/data/key.rs` 定义了全部 26 个字母 `A`–`Z`，映射覆盖全部字母（见 §13）。

### 8.2 GPU 快照集成测试（`crates/unlit3d/tests/`）

新建 `crates/unlit3d/tests/gpu_ui_source.rs`（与既有 `gpu_ecs.rs`、`animated_scene.rs` 并列），复用 `common/mod.rs` 的 `Ctx::headless()` / `test_world` / `read_texture_bytes` 等。每次都是「热身一帧丢弃、再渲染一帧读回」的两帧模式（`gpu_ui.rs` 的既有做法），因为 egui 首帧不知道字体尺寸。

**A. 只渲染 UI（不挂 `MeshSource`，也没有相机）** — 这是最容易坏、也最能暴露集成问题的场景
1. `ui_only_draws_without_a_camera`：世界**没有** `Camera`、`Renderer` **没有**挂 `MeshSource`，只挂 `UiSource` + 两个 `UiPanel`。断言 UI 真的画出来了（`count_pixels_off_background > 0`）。
   - 这条同时验证 §4.7 去掉两处 early-return（否则纯 UI 一片空白）。
   - **快照**：`ui_only.webp`。
2. `ui_only_with_two_panels`：同一世界里两个 `UiPanel` 实体，各自画在不同位置；断言两块区域都被覆盖，且**各自的像素属于各自的区域**（分区 `count_pixels_off_background`）。
3. `ui_only_clears_to_load_ops`：`RenderLoadOps` 的 clear 色出现在未被 UI 覆盖的区域（确认 pass 的 clear 仍按 `load_ops` 执行，UI 是叠加而非替换）。
4. `ui_only_multisampled_and_srgb`：MSAA（`SAMPLES = 4`）与 sRGB 目标下 UI 正确（`SurfaceKey` 驱动管线特化）；断言 sRGB 目标的像素值与预期色一致（不是「反色」或「暗一档」）。
5. `ui_only_at_high_pixel_density`：`pixels_per_point = 2.0` 时 UI 的**位置与尺寸**都对。
   - 这条专门为 `scissor_rect` 漏乘 ppp 的 bug（§2.2、§5.1）而写：**断言裁剪边界附近的像素**（例如红方块的**右下角内 1–2 px 必须仍为红**），而不只是中心点——现有测试只断言中心，因此漏掉了这个 bug。

**B. 同时渲染 mesh + UI** — 验证两个源在同一 pass 内协作
6. `mesh_and_ui_in_one_frame`：挂 `MeshSource` + `UiSource`（`FrameOrder::MESH` / `OVERLAY`）。一个立方体 + 一个半透明 UI 面板**叠在立方体上方**。
   - 断言三件事：立方体区域仍是 3D 的样子（未被 UI 覆盖处）、UI 区域是 UI 的颜色、**半透明 UI 下方的立方体被混合**（该处像素既非纯立方体色也非纯 UI 色）。
   - **快照**：`mesh_and_ui.webp`。
7. `mesh_and_ui_respects_source_order`：把 `UiSource` 的 `order` 改成排在 `MeshSource` **之前**（改源组件的 `order` 字段），断言 UI 被 3D 盖住/顺序确实反转——**这是 `FrameOrder` 生效的端到端证据**，而非只有单元测试。
8. `ui_does_not_clip_the_mesh`：UI 有 scissor 矩形，但 3D 绘制**在 UI 之前**、且 `PassState` 是每个 `Scene::record` 的局部量（§4.6），所以 3D 不应被 UI 的 scissor 裁掉。断言立方体的完整轮廓都在。
   - 这条钉住我先前判断错误的那处语义，防止将来有人把 `PassState` 提到 pass 级。
9. `mesh_and_ui_survive_a_second_frame`：连渲染两帧（含 `world.apply()`），断言第二帧与第一帧**逐像素一致**（相机的 `globals` 会推进，所以相机静止、不用 `time` 的 UI 才可比——若不一致，说明某处跨帧状态泄漏）。这条复用现有「跨帧缓存复用」的测试思路（`renderer.rs:2741`）。

**C. `wgpu_unlit_render` 侧回归**
10. `Scene` 句子柄化后：`scene.rs` 的 mock-pass 测试需同步改写（`DrawEntry` 字段变了）；`tests/gpu_ui.rs` 用的是旧 API，要跟着改并**保持通过**（它是 UI 后端本身的回归网）。
11. §5.1 的两个 bug 修完后，在 `wgpu_unlit_render/tests/` 补：`TextureOptions` 真的按 `ImageDelta.options` 生效（用一个 Nearest + Repeat 的纹理断言采样行为），以及 `scissor_rect` 的 ppp 取整/clamp（对照 `egui-wgpu` 的 `ScissorRect::new`，`renderer.rs:1141`）。

### 8.3 快照文件与门槛

- 快照落在 `crates/unlit3d/tests/snapshots/`：`ui_only.webp`、`mesh_and_ui.webp`（其余用数值断言，不新增快照）。
- 用默认阈值（`DEFAULT_MIN_SCORE = 85.0`）。若某条因驱动差异不稳定，**先怀疑测试本身不确定**（例如用了 `egui::Context` 的 frame time、随机布局），再考虑调阈值；调阈值要写清理由。
- 快照只在绝对必要处使用：**投影/混合/遮挡**这类「看图才知道对不对」的性质。位置、计数、顺序一律用数值断言。

### 8.4 收尾

按项目约定先 `cargo clippy` 再 `cargo fmt`。注意工作区 lint 含 `print_stderr`/`print_stdout`（§4.6），库代码里不要用 `eprintln!`。

---


## 9. 实施顺序

> **本节全部步骤均已实施完毕**（结果见 §13）。下面 12 个步骤逐一标注「已完成」；步骤描述保留为当时的实施意图记录。

三个阶段，**渲染侧重构 → UI → 输入**（用户指定：先 UI 后输入）。理由：

- 渲染侧重构是一切的前提：`Scene` 句子柄化、帧源机制、`MeshSource` 搬家。
- **UI 在输入之前**：UI 先能画出来并拍快照，输入随后才接进来——这样「UI 渲染是否正确」与「输入是否翻译正确」两类失败不会互相掩盖。UI 的 `build_scene` 先读一个空的/手工填的 `InputState` 即可跑通渲染路径。
- `MeshSource` 搬家是本期最大的一次机械改动（约 800 行），独立成一个提交，避免与 UI 的失败混在一起。

每一步都可独立提交、独立通过测试。

### 阶段 1：渲染侧重构（无 UI、无输入）

**1. `wgpu_unlit_render::scene`：`Scene` 自持句柄（§3、D1）** — **已完成**
- `DrawEntry` 三个字段改为 owned（`wgpu::RenderPipeline`/`BindGroup`/`Buffer` + `Range<u64>`）；`Scene` 去掉生命周期参数。
- 删 `recycle`/`reborrow`/`launder`；`record` 里重建 `BufferSlice`；`PassState` 去重键改为 `(Buffer, Range<u64>)`。
- 新增 `Scene::extend`（供 UI 场景并入；`MeshSource` 自身直接 push）。
- 同步改 `wgpu_unlit_render` 自带测试（`scene.rs` 的 mock-pass 测试）与 `unlit3d/src/scene.rs`、`renderer.rs` 的组装点。
- **验收**：§8.2 第 10 条——`cargo test -p wgpu_unlit_render` 全绿（`gpu_ui.rs` 跟着旧 API 改造后仍通过）；`unlit3d` 的 GPU 测试（`gpu_ecs`/`animated_scene`/`custom_pipeline`）仍通过。
- 提交点：**只动 `Scene`、不改 `Renderer` 对外行为**，可单独验证「句子柄化没改变画面」。

**2. `unlit3d::source`：帧源机制 + 顺序（§4.6、§4.8、D2/D3/D6/D8/D13）** — **已完成**
- 新增 `source.rs`：`FrameSource` trait（`build_scene(&World)`/`scene`/**必需的 `order`**）、`FrameOrder`（**无 `Default`**）、`mount_index: u64`、以及 GPU 上下文的资源组件。
- **按 §4.8**：`GpuContext`/`FrameContext`/`SourceContext`/`FrameTarget`、`with_source_mut`、`source_as`/`source_id_of`、`SourceId`、`mount`/`unmount`/`set_order`、`on_mount`/`on_unmount` 的**钩子形态**一律不写。`Source(Box<dyn AnySource>)` 组件、`RenderContext`（只装 `Entity`）、`mount_index` 要写。
- **保留**：`AnySource: Any + FrameSource` 的无方法 blanket impl（异构源要被同一查询驱动，`.tmp/hybrid` 已实测）；类型化访问用 trait upcasting，不在 `FrameSource` 上加 `as_any_mut()`。
- GPU 上下文落为资源组件：`device`/`queue`/`graph` 各一个资源实体，并有一个把三者 id 收在一起的常驻资源（`RenderContext`）；本步顺带定下 D13（target 走「每帧资源」，见该节倾向 C）。
- 卸载：提供一条排队命令（先 `graph.remove_drop(root)` 再 `despawn`），`queue()` + `apply()` 落地；`next_source_id`/`order_dirty` 都不再存在（§4.6 改为每帧重算顺序）。
- `crates/unlit3d/Cargo.toml` 加 `log = "0.4"`（已在依赖图中，零新增编译成本）；测试装捕获 logger 时需 `features = ["std"]`。
- `render` 改为「构建所有源 → 按 `(order, mount_index)` 录制」（§4.6、§4.7），去掉两处 early-return；帧总是录制并按 `load_ops` clear（**去掉的是「无内容就提前返回」，不是「限制一帧只能有一个 pass」**）。
- **本步结束时 `Renderer` 仍保留原有 3D 路径**（尚未搬走），把「3D 主场景」当成一个内部源或第 0 个 `Scene`——先让机制本身通过测试，搬家留给下一步。
- **验收**：§8.1 第 1–2 组（帧源机制 + 顺序歧义警告）全过；**现有全部测试仍须全绿**（对外 API 未变）。

**3. `unlit3d::mesh_source`：把 3D 整体搬进 `MeshSource`（§4.4、§4.8）** — **已完成**
- 新建 `mesh_source.rs`：`MeshSource` 持有原 `Renderer` 的 3D 字段（`families`/`pipelines`/mesh 与 index/vertex 池/metadata/instance/剔除缓存/自身 `Scene`）与其全部方法（§4.4 的表）；`build_scene` = 原 `render` 的 3D 段，上下文自己从世界取。
- **不写 `MeshSourceExt` 拓展 trait**（§4.8）：setup 期取 `world.get_mut::<Source>(entity)` 后 downcast 成 `&mut MeshSource`，与 `world.get_mut::<ResourceGraph>(ctx.graph)` 分属不同 cell，可同时持有。`renderer.rs` 里不出现 mesh 代码这一目标由「mesh 代码本就在 `MeshSource` 里」自然达成。
- `Renderer` 收缩为纯帧级：只剩下 target 绑定与「按序构建 + 录制 + submit」；`device`/`queue`/`graph`/源都不再是它的字段。`set_render_target`/`attachments`/`clear_pass` 留下。
- **迁移点**：`register_unlit_family` 与 mesh/material 分配移到源上——`tests/common/mod.rs:33` 的 `create_renderer` 与示例需改成「先 `world.spawn` 上下文资源与源，再注册家族」，这是本步需要改动调用方写法的地方。
- **验收**：`cargo test -p unlit3d` 全绿（含 `a_frame_without_a_camera_only_clears`、`rendering_a_world_twice_reuses_every_per_frame_cache`、排序/池/家族特化等既有断言）；示例能编译运行且画面与重构前一致。
- 提交点：**这一步之后渲染侧重构结束**，架构目标（内置 mesh 与其他源对称、无特权路径）已达成。

### 阶段 2：UI（先于输入）

**4. `wgpu_unlit_render::ui`：修基础设施（§5.1、D5）** — **已完成**
- `TextureOptions` 接线（`ui.rs:391` 的 bug）、`scissor_rect` 补乘 ppp（`ui.rs:715` 的 bug）、`scene(&self, &ResourceGraph)` 放宽借用、`ui_options_for_surface` 入口。
- `UnlitOptions.depth_stencil` 改为 `Option`（唯一改变既有公开行为的一处，D5）。
- **验收**：§8.2 第 11 条——`cargo test -p wgpu_unlit_render --features egui`；两个 bug 各需**新断言**（`TextureOptions` 用 Nearest+Repeat 纹理；scissor 断言**裁剪边界附近**的像素，现有测试只断言中心点所以覆盖不到）。

**5. `unlit3d::ui`：`UiPanel` + `UiSource`（§5.2、D12）** — **已完成**
- `UiPanel` 行为组件（§5.2.1）与 `UiSource`（setup 期经世界建 UBO/管线；`build_scene` 组装 `RawInput` → `ctx.run_ui` **逐一驱动所有 `UiPanel`** → `integration.update` → 取 `Scene`）。
- `ui = ["dep:egui", "wgpu_unlit_render/egui"]` feature（D11）。
- `FrameOrder::OVERLAY` 声明顺序。
- **验收**：§8.1 第 3 组（`UiPanel` 驱动，无需 GPU）+ **§8.2 的 A 组（只渲染 UI 的快照集成测试）全部通过**，特别是 `ui_only_draws_without_a_camera`（纯 UI、无相机）与 `ui_only_at_high_pixel_density`（ppp 边界）。

**6. `crates/unlit3d/tests/gpu_ui_source.rs`：mesh + UI 同帧（§8.2 B 组）** — **已完成**
- 单独立一步，因为它验证的是**两个源在同一 pass 内协作**（顺序、混合、scissor 不越界、跨帧复用），与第 5 步的「UI 自己能不能画」是不同的失效面。
- **验收**：§8.2 第 6–9 条全过；新增快照 `mesh_and_ui.webp`。

### 阶段 3：输入

**7. `unlit3d::input`：事件类型、`InputState`、行为组件、`dispatch_input`（§6.1–6.4、D4/D8/D9）** — **已完成**
- 事件与状态类型、资源实体、行为组件（`OnInput`/`OnKey`/`OnPointer`/`OnTouch`/`OnText`/`OnIme`）、`dispatch_input`。
- **验收**：§8.1 第 4 组（类别路由、无资源/无组件/空事件不 panic、`queue()` 后 `apply` 生效、`clear_events` 的边界）。

**8. `unlit3d::input::winit` + `ui/convert.rs`（§6.5、§6.6）** — **已完成**
- `WindowEvent → InputEvent` 翻译；`InputEvent → egui::Event` 转换（触摸附带 pointer 事件）。
- **验收**：§8.1 第 4 组（winit 翻译）+ 第 5 组（egui 转换）。

**9. 把输入接进 UI** — **已完成**：`UiSource` 的 `build_scene` 从 `InputState` 组装 `RawInput`（`screen_rect`/`native_pixels_per_point`/`events`/`focused`），并写回 egui 的捕获状态（§6.7）。
- **验收**：补一条 GPU 测试——喂一个 `PointerButton{Primary}` 事件到按钮所在坐标，断言按钮**真的被按下**（像素变化或面板里的状态组件变化）。这是「输入 → UI」的端到端闭环。

### 阶段 4：示例与文档

**10. 示例改造（§7）** — **已完成**：挂 `MeshSource` → 挂 `UiSource` → spawn 两个 `UiPanel` → 接输入行为组件；`Esc` 仍由 winit 分支处理。

**11. 文档** — **已完成**：`crate` 级文档与 `docs/DESIGN.md` 的「实施计划」勾选项。**注意：`DESIGN.md` 未经许可不修改**，只在最后提醒用户更新（本期新概念：帧源 `FrameSource`、内置 mesh 也是源、`UiPanel` 行为组件、输入行为组件、多 `Scene` 顺序绘制）。

**12. 收尾** — **已完成**：按项目约定先 `cargo clippy` 再 `cargo fmt`（§8.4）。


## 10. 需要抉择的点（具体到文件、行与后果）

每条都给出：**问题**（在哪一行、为什么是问题）、**选项**、**改动面**、**决定/倾向**。编号连续，不用 `′` 之类派生编号。

**已定**（用户决定）：D1（`Scene` 自持句柄）、D2（**帧源与 GPU 上下文都放进 ECS**，内置 mesh 也是源——§4.8 修订）、D3（`build_scene` 命名；`source_as`/`on_mount` 部分作废）、D4（输入分发由调用方显式调用）、D5（`depth_stencil` 改 `Option`）、D6（`build_scene`/`scene` 两方法）、D7（**不再需要——源进 ECS 后直接取图**，§4.8 修订）、D8（`FrameSource::order` + `mount_index` tie-break，§4.8 代价 2）、D12（界面是 `UiPanel` 行为组件，`UiSource` 逐一驱动）。
**已定**（实施时落定，见 §13）：D9（事件列表所有权 → A）、D10（UI 源拿输入的方式 → A）、D11（feature 划分 → A）、D13（`RenderContext` 是否带每帧 target → C）。

### D1 `Scene` 是否改为自持句柄 — `[已定：选 A]`

**问题**。`crates/wgpu_unlit_render/src/scene.rs:162` 的 `DrawEntry<'a>` 三个字段都带生命周期：`pipeline: &'a wgpu::RenderPipeline`、`bind_groups: ArrayVec<(u32, &'a wgpu::BindGroup), 8>`、`vertex_buffers: ArrayVec<(u32, BufferSlice<'a>), 16>`、`index_buffer: Option<(BufferSlice<'a>, IndexFormat)>`。为了让跨帧复用成立，配套了 `Scene::recycle`/`reborrow`/`launder`（`scene.rs:321-364`）。

**决定：选 A（改为自持句柄）**，且这不是偏好而是**用户方案的编译期前提**（见 §4.2）：多个 `Scene` 共存时，若它们都借用资源图，则「先全部构建、再统一录制」无法成立。选 B（不改）会让 §4 的整个设计失效，退回到我原先那条已被否定的 FrameLayer 路线。

**选项 A（已采纳）**：`DrawEntry` 自持 `wgpu::Buffer`/`wgpu::BindGroup`/`wgpu::RenderPipeline`（三者都是 Arc 句柄、`Clone`、按身份 `PartialEq`，已核对 `wgpu-30.0.1/src/api/{buffer,bind_group,render_pipeline}.rs` 的 `impl_eq_ord_hash_proxy!`），`Scene` 去掉生命周期参数。
- 改动面：`scene.rs` 的 `DrawEntry`/`Scene`/`PassState`/`record`/`launder`；`unlit3d/src/scene.rs:415` 与 `renderer.rs:1353/1381` 的 cache 生命周期；`wgpu_unlit_render/tests/gpu_unlit.rs:456-478`、`gpu_ui.rs:158`；`ui.rs:451`。
- 额外收益：`renderer.rs` 可删掉 `bind_group_cache`/`buffer_cache`/`vertex_slot_cache` 三个字段及 `EntryHandles`（约 -60 行），`renderer.rs:2765-2777` 的容量断言测试随之简化。
- 代价：`Scene::record` 里 `BufferSlice` 要在录制时重建（`buffer.slice(range)`）；`DrawEntry` 从「几个指针」变成「几个 Arc 句柄 + 2 个 `Range<u64>`」，内存略增，但每帧 clone 次数不变（现在也在 clone）。
- **额外收益（因 §4 而变关键）**：`recycle`/`reborrow`/`launder` 全部删除；`Renderer` 的 `scene_cache: Scene<'static>`（`renderer.rs:175`）变成普通 `Scene` 字段，每个源也各自持有一个 `Scene` 并可跨帧复用其 `draws` 分配。

### D2 帧源的归属与注册方式 — `[已定：帧源与 GPU 上下文都放进 ECS（§4.8）]`

**问题**。UI 的状态与它的绘制应该放在哪里、如何与渲染器关联、如何保证顺序；以及**内置的 mesh 渲染是否也走同一条路**。

**决定（用户提出，已采纳）**：**帧源是组件，共享 GPU 上下文是资源**（§4.8）；每个源自带状态与其 `Scene`；`render` 按顺序录制多个 `Scene`。**FrameLayer 概念取消。** 并且——**内置的 mesh 渲染本身就是一个帧源（`MeshSource`）**，与 UI 完全对称，不在渲染器里留任何 mesh 专用绘制路径。详见 §4、§4.4、§4.8。

**为什么内置 mesh 也要变成源**：`Renderer` 现有的 3D 字段（`families`/`pipelines`/mesh 池/metadata/instance/剔除缓存/`scene_cache`）在「多源」世界里是**特权字段**——只有渲染器自己能用，第三方复制不了。把它们整体搬进 `MeshSource` 之后：
- 3D 与 UI 走**完全相同**的路径（spawn 源 → `build_scene` → `scene()` → 按序录制），没有「主场景 vs 附加场景」之分；
- 用户想自定义 3D 路径（换剔除策略、加自己的批处理、画阴影）时，可以**不挂 `MeshSource`**，挂自己的实现；
- `Renderer` 缩小为纯帧级装置（附件/target/encoder/pass/submit），职责单一。

**关于「把源放进世界」的既有疑虑（已作废）**：早期方案曾以「源放进世界会导致『源每帧既要 `&mut` 资源图、又要从世界查询自己』的双重借用纠结」为由否决它。**该说法经实测不成立**——图与源是不同的 cell，二者可同时借用；更彻底地说，源根本不必被递进图，它自己取即可。详见 §4.8 的探针表格。

**已实测**：`.tmp/src_probe`（真实 wgpu 设备、真实 encoder/pass，2 个源 + 主 Scene）验证「构建阶段用图 → 录制阶段只读各源 `Scene`」可编译可运行。

### D3 `FrameSource` 的 API 形状 — `[已定：build_scene 命名 + 必需的 order；其余见 §4.8]`

**问题**。初版 `FrameSource` 有一个真实的命名缺陷：**`build` 看不出功能**。它做两件事——按需分配/上传 GPU 资源、组装本帧绘制列表——名字应体现「产出本帧的 `Scene`」。

**决定**：方法名 `build` → **`build_scene`**（与只读的 `scene()` 构成「写—读」一对）。完整签名见 §4.8。

**已废弃的部分**（原设计，随源进 ECS 而不再需要，见 §4.8）：
- 访问器形态 `source()`/`source_mut()`/`source_as::<T>()`/`source_id_of()`：源是组件，遍历只需 `query::<&Source>()`，类型化访问改为「取组件后 downcast」。
- `on_mount`/`on_unmount` 钩子：`despawn` 没有钩子，注册/释放改为「setup 期直接访问 + 一条排队卸载命令」（§4.8 代价 1）。
- `AnySource` **保留**，但退化为无方法 supertrait（`Any + FrameSource`）。

**仍然成立的**：注册/释放 GPU 资源必须**显式**进行（`Drop` 拿不到图），只是载体变了。

### D4 输入分发的调用方 — `[已定：选 A]`

**决定：选 A——调用方在帧循环里显式调用 `dispatch_input`。**

**问题**。`dispatch_input(world)` 由谁调用？这直接决定「回调里 `queue()` 的结构变更何时落地」。

**选项 A：调用方在帧循环里显式调用**（已采纳）。
```rust
input::dispatch_input(&world);   // 调用行为组件
world.apply();                   // 落地回调排队的变更
renderer.render(&world);
input_state.clear_events();
```
- `apply` 必须 `&mut world`（`unlit_ecs/src/world.rs:337`），而 `render` 只拿 `&LocalWorld`；所以「谁调用 dispatch」实际上就是「谁有机会 `apply`」。
- 与 `unlit_ecs` 的「调用者即系统」哲学一致（`lib.rs:26-28`），且 `tests/behavior.rs:293-304` 就是这个模式。

**选项 B：`Renderer::render` 内部自动分发**（已否决）。
- 用起来最省事，但 `render` 只有 `&LocalWorld`，**无法 `apply`**，回调排队的所有结构变更都要等到下一次外部 `apply`——一个「按下按钮生成一个实体」的回调会延迟一帧生效，且这个延迟对用户不可见。
- 还让「用户想自己控制分发顺序/时机」变得不可能。

`dispatch_input` 返回 `bool`（是否有事件被投递），方便调用方决定是否需要 `apply`。

### D5 `UnlitOptions.depth_stencil` 是否改为 `Option` — `[已定：选 A]`

**问题**（已在 §2.2 第 5 条核实，并**已用探针实测复现**）。`pipeline.rs:199` 是非可选的 `wgpu::DepthStencilState`，`pipeline.rs:465` 一律 `Some(...)`；`apply_surface`（`pipeline.rs:717`）在 `depth_stencil_format == None` 时刻意保留基础格式，并有测试 `apply_surface_without_a_depth_attachment_keeps_the_base`（`pipeline.rs:971`）钉住这一行为。

**实测证据**（`.tmp/probe`，Vulkan 后端，`ui_options` + 只有 color 的 pass）：
```
wgpu error: Validation Error
  In a CommandEncoder / In a set_pipeline command
    Render pipeline targets are incompatible with render pass
      Incompatible depth-stencil attachment format: the RenderPass uses a texture
      with format None but the RenderPipeline with 'wgpu_unlit_render::unlit' label
      uses an attachment with format Some(Depth24PlusStencil8)
```
即：**今天无法在只有 color 附件的 target 上使用 unlit 管线**（不限于 UI——任何 unlit 变体都不行）。机制上见 wgpu-core 30.0.1 的 `check_compatible`（`device/mod.rs:126-155`）与管线 `pass_context` 的构造（`device/resource.rs:5009-5022`）：判定**只比 `Option<TextureFormat>`**，与 `depth_write_enabled`/`depth_compare` 无关。

**双向实测**（`.tmp/depthprobe`，Vulkan/llvmpipe，含「故意不匹配 color 格式」的**对照组**以证明探针确实能捕获校验错误；注意必须在 `submit` 之后才能收到校验错误，仅录制不报）：

| pass | 管线 `depth_stencil` | 结果 |
|---|---|---|
| 有 `Depth24PlusStencil8` | `None` | ❌ `the RenderPass uses a texture with format Some(Depth24PlusStencil8) but the RenderPipeline uses an attachment with format None` |
| 无深度 | `Some(Depth24PlusStencil8)` | ❌ 反向同一条错误 |
| 有 `Depth24PlusStencil8` | `Some(同格式)`, write=false, Always | ✅ |
| 有 `Depth24PlusStencil8` | `Some(同格式)`, write=true, Greater | ✅ |

**推论（本期的正确做法）**：UI 与 mesh 共用带深度的 pass 时，UI 管线声明**同一个**深度格式 + 不写不测即可，**不需要** `Option` 来表达「UI 不要深度」。`Option` 的用武之地是**目标本身没有深度附件**，那时 pass 内**所有**管线（含 mesh）都必须不声明深度。所以 D5 的理由是「让管线能表达『这个 pass 没有深度附件』」，而不是「UI 想要特殊待遇」——后者是一个容易写错的方向。

**今天谁受影响**：`WindowSurface` 总是建深度附件（`winit.rs:409`），所以示例与现有测试都撞不到。撞到的是「自定义 target 只有 color，想叠 UI/2D」——恰是 UI 最常见的用法之一（`Renderer::set_render_target` 的文档 `renderer.rs:527` 只要求「color 与 depth 至少有一个」，但实际实现做不到 color-only）。

**选项 A：`depth_stencil: Option<wgpu::DepthStencilState>`**（已采纳）。
- 改动面：`pipeline.rs` 的字段（`:199`）、`standard_shape`（`:253`）、`UnlitPipeline::new`（`:465`）、`apply_surface`（`:717-723`，改为 `options.depth_stencil = surface.depth_stencil_format.map(...)`）、测试（`:948-985`）；
- 字面量构造点：`crates/unlit3d/tests/common/mod.rs:44-67`、`crates/wgpu_unlit_render/tests/gpu_unlit.rs:513/710`、`crates/unlit3d/src/renderer.rs:1852`、`crates/wgpu_unlit_render/src/ui.rs`（`apply_ui_settings` 写三个 depth 字段，`:186-187`）。
- 语义变更：`apply_surface` 从「保留基础」变成「跟随目标」，`pipeline.rs:971` 那个测试要改成断言 `None`。**这是行为变更**，但方向更正确（管线必须与 pass 匹配）。
- `apply_ui_settings` 的语义要按目标分支：**目标有深度**时，声明的深度格式必须与 pass **一致**（`apply_surface` 已把 `format` 对齐到 `surface.depth_stencil_format`），再设 `write=false`+`Always`；**目标无深度**时才置 `None`。两种情况都由 `apply_surface` 之后的 `ui_options_for_surface` 定，这正是 §5.1 那个入口存在的时机。

**选项 B：不动，UI 要求必须有深度附件**（已否决）。
- 改动面 0。
- 代价：`Renderer` 的「color-only target」实际上不可用（今天已如此，只是没写进文档）；UI 在 color-only target 上无法绘制；文档需要明确写死这个限制。

**决定：选 A**。理由：方向明确更正确——管线的深度状态**必须**与 pass 的附件匹配（wgpu 强校验，已实测），「保留基础格式」本质上是把错误推迟到 `set_pipeline` 才炸。
- `pipeline.rs:971` 的测试改名 `..._clears_the_base` 并断言 `None`。
- 顺带把「color-only target 可用」写进 `Renderer::set_render_target` 的文档（`renderer.rs:527`）。

### D6 `FrameSource` 的 build_scene/scene 两方法拆分 — `[已定：保留]`

**问题变更**。原 D5 问「`FrameLayer` 的 `prepare`/`draw` 两阶段是否必要」。FrameLayer 已取消（D2），但**两阶段本身保留了下来**——`build_scene`（拿 `&mut graph`/`&mut encoder`）与 `scene()`（只读）仍是两个方法。

**必要，且不是设计选择而是借用规则的结果**：
- 构建阶段需要 `&mut ResourceGraph`（注册纹理/缓冲/绑定组）与 `&mut CommandEncoder`（stage 上传）；
- 录制阶段需要 `&self.sources`，而每个源的 `Scene` 由它自己持有（D1=A 后无生命周期）——**录制完全不碰资源图**；
- 若合并成一个方法，则源在返回 `Scene` 之前必须能从图里读（UI 确实要读：`integration.scene(&graph)`），于是 `ctx` 必须同时提供 `&mut graph` 与「读 graph」的能力 → 只能靠 `RefCell`（把借用冲突推迟到运行期 panic）。

**决定：保留两个方法**。已实测（`.tmp/src_probe`）这个结构可编译、可运行，无需 `RefCell`、无运行期借用风险。
- 若用户偏好更短的 trait，可改成单方法 + `RefCell<ResourceGraph>`，但那会把编译期保证换成运行期 panic，**不推荐**。

### D7 源的 setup-time API 如何拿到 `&mut ResourceGraph` — `[已定：不再需要（§4.8）]`

**问题（已消失）**。原设计里图私有在 `Renderer`，而源在 setup 期需要 `&mut graph`；用户手里只有 `&mut Renderer`，借走源就借走了整个渲染器，于是拿不到图。当时为此设计了 `with_source_mut` 通用入口 + `MeshSourceExt` 拓展 trait。

**决定**：图既然是**世界里的资源组件**（§4.8），源在 setup 期直接 `world.get_mut::<ResourceGraph>(ctx.graph)` 即可，上述前提不再成立。因此 **`with_source_mut`、`MeshSourceExt`、`source_id_of`、`GpuContext` 全部删除**；「`renderer.rs` 里不出现 mesh 代码」这一目标，由「mesh 代码本来就在 `MeshSource` 组件里」自然达成。

**共享上下文仍只需 `device`/`queue`/`graph`**：逐方法核对的表见 §4.5，其余全是源自己的状态。只有图需要独占借用，`device`/`queue` 是 `Clone` 共享句柄。

**两个实测结论仍然有效**（`.tmp/ext_probe`、`.tmp/hybrid`）：
- `&dyn FrameSource` **不能**转 `&dyn Any`（生命周期非 `'static`），所以类型化访问不能指望调用方自己转——ECS 方案里改为在包装结构上做 trait upcasting（`&mut *source.0 as &mut dyn Any`），见 §4.8。
- `AnySource` 的 blanket impl 让 `Box<dyn AnySource>` 本身即 `Any`，用户无需写任何转型样板。

### D8 源的绘制顺序如何指定 — `[已定：必需的 FrameSource::order + 歧义警告]`

**问题**。「挂载顺序 = 绘制顺序」把顺序绑死在挂载时刻，运行期无法调整，且意图不可见（只能靠「先 mount 后 mount」隐式表达）。

**决定（用户要求，见 §4.6）**：
- `FrameOrder(pub i32)` 是值的全序，提供 `MESH`(0)/`OVERLAY`(100) 常量；**不实现 `Default`**。
- **`FrameSource::order()` 是必需方法，不给默认实现**。理由：默认值会让「忘记写 order」与「确实想用挂载顺序」无法区分，顺序意图又变回隐式——正是要消除的问题。已实测：去掉默认实现后不写 `order` 的源**编译不过**。
- **§4.8 修订**：`mount`/`mount_at`/`set_order`/`SourceId` 已删除——挂载是 spawn 源组件，顺序取源自己的 `order` 字段，运行期改该字段即可（§4.6）。
- 排序 = 按 `(order, mount_index)` 排序，`mount_index` 是源显式携带的挂载序号作 tie-break（**不能**用实体行序，见 §4.8 代价 2）。每帧重算即可，源数量是个位数量级（§4.6）。
- **歧义 = 两个及以上源声明相同 `FrameOrder`** → `log::warn!` 一条，含**该组源数量、那个 order 值、各源 `Entity`**（按 `mount_index`）。**只警告不 panic**（相同 order 有时合法，强制唯一会逼用户编造无意义数字）。
- **用 `log` facade**（用户选择）：`log 0.4.34` 已在依赖图（wgpu/egui 均依赖），只需在 `crates/unlit3d/Cargo.toml` 加显式依赖，零新增编译成本；可被应用 logger 过滤。既有 4 处 `eprintln!` 不改造。

**为什么不排序（只按挂载顺序）**：能工作，但顺序意图不可见、运行期不可调，「UI 必须在 3D 之后」只能靠隐式约定维持。

**为什么不做源间依赖图**：对「一层 UI 叠在 3D 上」过重；改源的 `order` 字段已能表达任意全序。

**已实测**（`.tmp/ord2_probe`，7 条断言）：无默认实现时源必须表态；不同 order 零警告；相同 order 恰好一条警告且含数量/值/ids；连续 100 帧零日志；歧义解决后不再警告、重新引入则再警告一次；三元歧义一条警告列全三个 id；把 order 设成相同值是 no-op 且不警告。

**顺带纠正一条我先前的错误判断**：`PassState` 是 `Scene::record` 的**局部变量**（`scene.rs:282`），所以 scissor/stencil **不会从一个 `Scene` 泄漏到下一个**——「被裁剪的绘制放最后」只约束**一个 Scene 内部**。因此源之间的顺序**不是**被 scissor 逼出来的硬约束，而是「UI 要合成在 3D 之上」的语义要求，正适合用 `order` 表达。

### D9 事件列表的所有权 — `[已定：选 A]`

**问题**。`InputState` 的事件列表要被两个消费者读（分发器 + UI 源）。实测（§6.4.1 第 4 条）：若驱动器在遍历期间**持续持有** `Ref<InputState>`，回调里写 `InputState` 会 panic（`component InputState is already borrowed while it is being write`）。同时，回调里想读 `InputState`（比如读「当前按下的按钮」）又很常见。所以分发前必须先把事件「拿出来」，不能让 `Ref` 跨遍历存续。

**选项 A（默认）**：`dispatch_input` 开始时 `Vec<InputEvent>` 克隆一份（事件小；`Text`/`Ime` 带 `String`，每帧通常个位数个）。`InputState` 的 cell 随即释放，回调可自由读写 `InputState`。分发器遍历的是这份克隆。
**选项 B**：`mem::take` 事件到调用方持有的 scratch，分发完再放回。零克隆，但调用方要保管 scratch，且「谁负责放回」容易出错（中途 panic 会丢事件）。
**选项 C**：事件不放在 `InputState` 里，由调用方作为 `&[InputEvent]` 传给 `dispatch_input` 与 UI 源。最纯粹，但要求帧循环自己存事件，`InputState` 就不再是单一事实来源。

**决定：选 A**（简单、不易错；事件量级极小）。`dispatch_input` 在开头克隆本帧的 `Vec<InputEvent>`，`InputState` 的 cell 随即释放，回调可自由读写它。

### D10 UI 源拿输入的方式 — `[已定：选 A]`

**问题**。UI 需要「本帧事件 + 窗口尺寸 + ppp + 焦点」，这些都在 `InputState` 资源里。源怎么拿到它？

**选项 A（默认）**：`UiSource` 持有 `InputState` 的 `Entity`（§5.2 的 `input` 字段），在 `build_scene` 里 `world.get::<InputState>(self.input)` 读。
- 与整个「行为组件靠实体寻址」的风格一致；`InputState` 是原型（archetype）固定的资源实体，句柄稳定。
- 代价：若调用方忘记 spawn `InputState`，源静默不工作——需要文档 + 一个 debug 断言。

**选项 B**：`FrameContext` 直接带上输入（`pub input: Option<&'a InputState>`）。
- 源不用自己找，但把「输入」硬编码进了通用 `FrameSource` API（未来只做后处理的源不需要输入）。

**决定：选 A**：保持 `RenderContext` 通用；`UiSource` 作为具体源自己知道要什么——它在 `build_scene` 里用类型查询找到 `InputState` 并记住其 `Entity`（§5.2 的 `input` 字段），之后按实体读取。

**实施时机**：UI 的渲染路径（§9 阶段 2 第 5 步）**先不依赖输入**——`build_scene` 读一个空的 `InputState`（或就地构造一个 `RawInput`），先把「只渲染 UI」的快照测试跑通。**输入接进 UI 是阶段 3 第 9 步**，那时才需要这个 `input` 字段。这样切分的理由：让「UI 画不出来」与「输入翻译错」两类失败不互相掩盖。

### D11 feature 划分 — `[已定：选 A]`

**问题**。`egui` 现在是 `crates/unlit3d/Cargo.toml` 里的**非 optional** 依赖，意味着不用 UI 的用户也要编译 egui。

**选项 A（默认，推荐）**：新增 `ui` feature（**默认开启**），`ui = ["dep:egui", "wgpu_unlit_render/egui"]`，`egui` 改为 optional。
- 与 `wgpu_unlit_render` 分层一致（`default = ["unlit"]`、`egui = ["dep:egui", "unlit"]`）。
- `winit` feature 已经是 optional 先例（`crates/unlit3d/Cargo.toml`）。
- 代价：`Cargo.toml` 与 `#[cfg]` 有少量改动；`pub mod ui` 需门控。

**选项 B**：不做 feature，`egui` 常开。
- 改动 0，但「不用 UI 也要编 egui」违背分层，且 egui 会进默认构建的依赖图。

**决定：选 A**。已实施：新增 `ui` feature（**默认开启**），`egui` 改为 optional，`ui = ["dep:egui", "wgpu_unlit_render/egui"]`，`pub mod ui` 随之门控；`input` 模块与 `source` 模块不依赖 egui，常开。`cargo nextest run --no-default-features --features winit` 通过（110 条），即不开 `ui` 也能构建。

### D12 UI 界面如何被驱动 — `[已定：界面是 UiPanel 行为组件，UiSource 逐一驱动]`

**问题**。UI 的「界面」应该放在哪里？初版把它做成 `UiSource` 上的**单个闭包字段** `ui: Box<dyn FnMut(&LocalWorld, &mut egui::Ui)>`，那等于**绕开 ECS**：一个 `UiSource` 只能有一个界面，界面的归属不在世界里，也不符合 `unlit_ecs` 的行为组件范式。

**决定（用户提出，见 §5.2.1）**：**界面是行为组件 `UiPanel`**，与 `unlit_ecs/tests/behavior.rs` 的 `OnFrame`/`OnInput` 同构：

```rust
pub struct UiPanel(Box<dyn FnMut(&LocalWorld, Entity, &mut egui::Ui)>);
```

`UiSource` 退化为**驱动器**：在 `build_scene` 里 `self.ctx.run_ui(input, |ui| for (e, mut p) in world.query::<&mut UiPanel>() { p.run(world, e, ui) })`。要点：
- **调用方即系统**：`UiSource` 决定驱动哪些面板、什么顺序；面板自身不注册、不自调度。
- **状态放兄弟组件**：行为组件运行期间被借用，不能重入借用自己（`behavior.rs:12-14`）。面板的展开状态、输入内容等放在**兄弟组件**。
- **回调可读写世界**：`run_ui` 只借 `&self`（egui `context.rs:794`），闭包是 `FnMut(&mut Ui)`，所以面板内 `get`/`query`/`queue()` 皆可，无额外借用冲突。
- **多面板 = 多实体**，可 spawn/despawn；第三方可定义自己的面板类组件，让 `UiSource` 用过滤器选择（`query_filtered::<Entity, With<C>>` 零借用代价）。
- `UiSource` **不再有 `ui` 字段**，`UiSource::new()` 无参数。

**已实测**（`.tmp/ui_drive_probe`，8 条断言，用 `unlit_ecs` 真实 API）：两个 `UiPanel` 实体由**一个** `UiSource` 驱动、按查询顺序执行；面板经传入的 `&LocalWorld` 读到自己的兄弟组件；面板内 `queue().spawn` 在 `world.apply()` 后落地；面板写兄弟组件成功；按 `query_filtered` 收集 id 的驱动形式与「闭包内直接 `query`」形式**都正确**。

**顺带发现（重要，egui 既有语义）**：egui 的 `run_ui` 在某个 pass 调 `request_discard` 做**多趟布局**时会**多次调用**闭包（`context.rs:770-771`、`:868`）。所以：
- 每次 pass 重建查询迭代器是**正确**的（两种驱动形式都实测通过多趟）；
- 但面板闭包会**跑多次**，因此必须**幂等**，或按 `num_completed_passes` 判断。这适用于所有 egui 后端（`egui-wgpu` 亦然），不是本设计引入的，但**必须在 `UiPanel` 的文档里写明**。

### D13 `RenderContext` 是否携带每帧的 render target — `[已定：选 C]`

**问题**（§4.8 提出）。源进 ECS 后，`device`/`queue`/`graph` 三个 `Entity` 收在一个常驻资源 `RenderContext` 里。但源在 `build_scene` 里往往还需要**本帧的 render target**（`SurfaceKey` 与物理尺寸），用来特化管线（`ui_options_for_surface` 那类入口）与算投影。这个值每帧可能变（resize、换 target），而 `RenderContext` 只 spawn 一次。

**选项 A：`RenderContext` 不带 target，源自己从附件资源查**。
- 附件（color/depth/msaa 视图 id 与 `SurfaceKey`）本就在渲染器实体上，源可自行读取。
- 代价：每个源都要重复「查附件 → 拼 `SurfaceKey`」的代码；而 `SurfaceKey` 的构造依赖渲染器内部约定，重复实现容易漂移。

**选项 B：每帧把 target 写回 `RenderContext`**。
- 源只读它，最省事。
- 代价：写回需要 `&mut world`，而 `render(&LocalWorld)` 只有 `&`；要么把 `render` 改成 `&mut LocalWorld`（会波及所有调用点与测试），要么用别的每帧载体。

**选项 C：target 走一个单独的「每帧资源」**，由帧循环在 `render` 之前写入（帧循环本来就持有 `&mut world`，因为它要 `apply()`）。
- 与 D4 的帧循环形态一致：`dispatch_input` → `apply` → 写 target → `render`。
- 代价：多一个资源实体；且「忘记写」时源读到过期 target（可让字段为 `Option` 并断言，或在 `render` 入口兜底写入）。

**决定：选 C**：它不改变 `render` 的签名，也不需要源各自复制 `SurfaceKey` 的构造逻辑；「每帧写入」与已有的 `InputState.clear_events` 是同一类显式帧循环职责。已实施：target 落为单独的每帧资源 `FrameTarget`，由帧循环在渲染之前写入。

## 11. 风险与注意

- **借用冲突是 panic**：`FrameSource::build_scene` 内不要同时持有 `world` 的可变借用；UI 的界面闭包签名 `FnMut(&LocalWorld, &mut Ui)` 只读世界，写入须走 `queue()` 或借用式 API（`with_mut`）。**特别注意**（`.tmp/borrowprobe` 实测）：驱动器**不能**自己握着 `&mut graph` 再把它递给每个源——嵌套取同一个图会 panic；源各自 `get_mut` 才安全。
- **源的卸载要显式清理**（§4.8 代价 1）：`despawn` 没有钩子，源在图里注册的节点须由一条排队命令先行 `remove_drop`。忘记清理会留下孤儿节点（`cleanup_drop` 可兜底，但依赖释放时机）。
- **不要用查询顺序当挂载顺序**（§4.8 代价 2）：`despawn` 走 `swap_remove`，行序会重排（`[a,b,c]` 删 `a` 得 `[c,b]`）。tie-break 必须用显式的 `mount_index`。
- **开 pass 必须在所有源 `build_scene` 完之后**：源在 build 期要 `&mut encoder` 做 staging 上传，而 pass 借用 encoder。这正是两阶段结构；不要在 `build_scene` 里尝试开 pass。
- **scissor 是 pass 级状态，但 `PassState` 是每个 `Scene::record` 的局部变量**（`scene.rs:282`）：所以 scissor/stencil **不会跨 `Scene`（跨源）泄漏**，「被裁剪的绘制放最后」只约束**一个 `Scene` 内部**。UI 每个 draw 都自带 scissor（现有实现如此），故 UI 的裁剪不会波及 3D。
- **源之间顺序由 `order` 显式声明**（§4.6、D8）：`MeshSource` 用 `FrameOrder::MESH`、`UiSource` 用 `FrameOrder::OVERLAY`。别把「UI 必须在 3D 之后」只寄托在挂载语句的先后上——那是隐式约定，也正是 `order` 要消除的。
- **相同 `order` 会 `log::warn!`**：这是提示不是错误，录制仍按 `mount_index` 进行。若日志里出现这条警告，说明有几个源的相对次序是巧合而非意图——改它们 `order` 字段给不同值。稳态帧不产生日志（只在源集合/顺序真正改变时检查）。
- **测试要装捕获 logger 才能断言警告**：`log` 是 facade，需 `log::set_boxed_logger` + 自定义 `Log`；这要求 `log` 的 `std`（或 `alloc`）feature。库本身只用 `log::warn!`，默认 feature 即可。
- **`print_stderr`/`print_stdout` lint 已生效**：工作区 lint 含这两条，5 个 crate 全 opt-in。所以库内**不要**用 `eprintln!`（会被 clippy 报），顺序警告走 `log::warn!`。现有 3 处 `eprintln!`（示例 2 处 + 测试工具 1 处）会被报出，属既存问题、本期不改造。（**修正**：实际上并不存在这些 `eprintln!`——工作区早已全部使用 `log`；见 §13。）
- **同一 pass 内的相机 UBO**：UI 的 `screen_view` 与 3D 相机不同，必须用各自的 UBO，或者 UI 在绘制前重写 3D 用的相机缓冲——**选各自的 UBO**。
- **`egui::Context` 是 `Clone`（内部 Arc）**，但 `UiSource` 持有它即可，不要跨帧重建（会丢字体图集缓存）。
- **每个源复用它的 `Scene` 分配**：`Scene::clear()` 保留 `draws` 的 capacity（现有 `scene_cache` 正是这么用的，`renderer.rs:1353`）；`MeshSource` 与 `UiSource` 各自每帧复用 `self.scene`，避免每帧分配。
- **卸载的资源回收要显式做**（§4.8 代价 1）：`despawn` 没有钩子，所以源在图里注册的节点须由一条排队卸载命令先行 `graph.remove_drop(root)`（`resources.rs:422`）。没有外部根节点的源（如纯调试叠加）无需清理。
- **`UiPanel` 闭包必须幂等**：egui 在 `request_discard` 的多趟布局中会**多次调用** `run_ui` 的闭包（`context.rs:770-771`、`:868`），所以面板跑多次。别在面板里做「只该发生一次」的副作用（累加计数器、发事件、`spawn` 实体）；要记状态就放兄弟组件并在每趟写同一结果，或按 `num_completed_passes` 判断。所有 egui 后端都如此，不是本设计引入的。
- **面板状态放兄弟组件**：`UiPanel` 在运行期间被借用，不能重入借用自己（`behavior.rs:12-14`）。展开状态、输入框内容等放它自己的兄弟组件。
- **第一帧丢掉**：egui 首帧不知道字体尺寸，例行「热身一帧丢弃」（`tests/gpu_ui.rs` 亦如此）；示例应在初始化后立刻渲染一帧并丢弃，避免用户看到一帧空白。
- **winit 的 `ScaleFactorChanged` 在部分平台先于 `Resized`**：两处都要更新 `InputState`，且 UI 只依赖 `InputState` 的值，不要在源里再读一次 window。
- **wasm**：`Instant` 在 wasm 可用；`std::thread` 不可用（示例已有 `spawn` 分支）。IME 在 web 不支持。
- **`Renderer::graph` 曾是 `pub` 字段**：源进 ECS 后该字段不再存在，图经 `world.get_mut::<ResourceGraph>(ctx.graph)` 取；不再需要任何字段拆分借用（§4.8）。
- **迁移面大**：把 3D 部分搬进 `MeshSource` 是本期最大的一次改动（约 800 行方法搬家，见 §4.4 的表）。收益是渲染器不再有特权路径；若想缩小首个提交，可先做 D1+D2 的源机制与 `build_scene` 骨架、把 `MeshSource` 的搬家放在紧接的第二个提交，但**不要**长期保留「渲染器内置 3D 路径 + 源机制」两套并存——那正是要消除的特权。

---

## 12. 参考位置索引

| 主题 | 位置 |
|---|---|
| 行为组件范式（`UiPanel` 照此设计） | `crates/unlit_ecs/tests/behavior.rs:1-14`、`:307-317` |
| egui 后端现状 | `crates/wgpu_unlit_render/src/ui.rs` |
| UI GPU 测试范式 | `crates/wgpu_unlit_render/tests/gpu_ui.rs` |
| `Scene` 与 pass 状态去重 | `crates/wgpu_unlit_render/src/scene.rs` |
| 帧循环与图缓存 | `crates/unlit3d/src/renderer.rs:1184-1389`、`crates/unlit3d/src/scene.rs:391-438` |
| 管线家族（对比：不走 `Renderer` 持有的另一条扩展路径） | `crates/unlit3d/src/pipeline.rs`、`crates/unlit3d/src/scene.rs:144-245` |
| 既有的两种「用户显式表达顺序」：`ZSortedDrawing`、`PipelineId` | `crates/unlit3d/src/components.rs:275`、`crates/unlit3d/src/pipeline.rs:199` |
| egui `run_ui` 取 `&self` + `FnMut`，`request_discard` 时会多次调用闭包 | `egui-0.36.2/src/context.rs:794`、`:770-771`、`:868` |
| `Scene` 内排序规则（非 z-sorted 在前、再按管线/材质/深度） | `crates/unlit3d/src/scene.rs:306-321` |
| pass 级状态的边界：`PassState` 是 `record` 的局部量 | `crates/wgpu_unlit_render/src/scene.rs:282`、`scene.rs:24-28` |
| `log 0.4.34` 已在依赖图中（wgpu/egui/epaint/naga 均依赖） | `Cargo.lock:1215` |
| 工作区 clippy lint 含 `print_stderr`/`print_stdout`，5 个 crate 全 opt-in | `Cargo.toml:33-37`、各 `crates/*/Cargo.toml` 的 `[lints]` |
| 项目当前的日志用法只有 `eprintln!`（4 处，示例与测试工具）（**修正**：不成立，已全用 `log`；见 §13） | `unlit3d_examples/src/main.rs:193/236` 等 |
| 交换链与附件 | `crates/unlit3d/src/winit.rs` |
| 现有示例 | `unlit3d_examples/src/main.rs` |

---

## 13. 实施结果

本节记录实施完成后的实际状态：提交范围、验证结论、各个待定抉择的最终落点，以及对本文若干判断的修正。§1–§12 保留为设计记录。

### 提交与验证

- **提交范围**：`main` 上 `0cb66c0..6ce33b7`，共 17 个提交；快照子模块 `wgpu_unlit_render_asset_files` 为 `a58efeb..70ae7a3`。两者均已推送。
- **`cargo xtask check`**（全工作区 clippy `-D warnings` + `cargo fmt --check`）：干净。
- **`cargo xtask test`**：392 个测试通过（本次工作前的基线是 185），doctest 同样通过。
- **`cargo nextest run --no-default-features --features winit`**：110 个通过，证明关掉 `ui`（即不编 egui）后 crate 仍可构建。
- **wasm 构建**成功；`typos` 与 `tombi lint --error-on-warnings` 干净。
- **13 个既有快照逐字节不变**（唯一例外见「快照重拍」）。

### 抉择的最终落点

- **D9（事件列表所有权）→ 选项 A**：`dispatch_input` 在开头克隆本帧的 `Vec<InputEvent>`，`InputState` 的 cell 随即释放，回调可自由读写它。
- **D10（UI 源拿输入的方式）→ 选项 A**：`UiSource` 在 `build_scene` 里用类型查询找到 `InputState`，并记住它的 `Entity`。
- **D11（feature 划分）→ 选项 A**：新增 `ui` feature（**默认开启**），`egui` 改为 optional，`ui = ["dep:egui", "wgpu_unlit_render/egui"]`。
- **D13（`RenderContext` 是否携带每帧 render target）→ 选项 C**：target 落为单独的每帧资源 `FrameTarget`，由帧循环在渲染之前写入。

### 对计划本身的修正

1. **§6.6 与键映射讨论中「egui 的 `Key` 只有 6 个字母」的说法是错的**。`egui-0.36.2/src/data/key.rs` 定义了全部 26 个字母 `A`–`Z`，因此映射覆盖全部字母，不存在「大多数字母无法映射、只能丢弃」的情况。
2. **计划声称工作区成员里有 3 处 `eprintln!` 需要改为 `log`——实际一处也没有**。工作区各处（含示例与测试工具）早已使用 `log`。

### 快照重拍

- **`egui_ui.webp` 被有意重拍**（子模块提交 `b2d5d9b`）。原因：§5.1 的纹理选项修复让以 `TextureOptions::NEAREST` 注册的纹理**真的**按最近邻采样，而旧快照捕获的是线性过滤后的渐变——也就是那个 bug 本身。除此之外，所有既有快照逐字节不变。

### 顺带修复的既有缺陷

- **`unlit_ecs::world::send_tests::several_threads_may_write_the_same_world` 存在竞态**：两个线程写的是**同一批实体**，而 `SyncCell::try_write` 是「尝试」而非「等待」，因此在高负载下会 panic `component is already borrowed while it is being write`。
- 修法是让测试在两个线程间**划分实体**（提交 `6ce33b7`），并以连续 30 次全量测试、0 次失败验证。
- 该文件与本次工作前的基线逐字节一致，所以这是**既有的 flake，不是本次改动引入的回归**。

### 仍未实施

- **蒙皮（skinning）与形变目标（morph targets）**：仍是 `docs/DESIGN.md` 中的未决项。
- **输入自动拦截**：按设计不做任何拦截——输入源只通过 `InputCapture` **发布**自己声明捕获了什么，由调用方决定这意味着什么。