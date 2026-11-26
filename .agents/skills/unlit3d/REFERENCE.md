# API 速查

**这是一份索引，不是签名抄本。** 每条只告诉你「有这个符号、它干什么、去哪个文件搜」，参数与返回类型一律回源码读——项目极早期、API 常改，手抄的签名表每一轮都会漏几条，抄错比抄不到更糟。

排序：可抄的骨架看 `crates/unlit3d/README.md` 的「Example」doctest（`cargo xtask test` 每趟编译它，不会过时）→ 确切签名看源码 → 本文只解决「该搜什么」。

决策与约束见 [SKILL.md](SKILL.md)，用法见 [SCENE.md](SCENE.md) / [MULTI_WORLD.md](MULTI_WORLD.md)，示例与命令见 [EXAMPLES.md](EXAMPLES.md)。

## 启动与 world

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 建渲染上下文 | `spawn_context(&mut world, RenderDevice, ResourceGraph)` 每 world 调一次，GPU 状态进 shared 子树；`RenderDevice` 收 device/queue/capabilities/health | `crates/unlit3d/src/source.rs` |
| 上下文句柄 | `RenderContext`，Copy。**它的字段都是 `pub(crate) Entity`，不是 GPU 句柄**，要从外部用只能走它的访问函数（`device` / `queue` / `capabilities` / `clock` / `try_graph` / `graph` / `uniforms` / `maintain_scope`，都吃 `Scope`）；只有 `of(world, anchor)` 是 pub | 同上，搜 `pub fn device` |
| 根作用域 | `World` 的 `root_scope` / `root_scope_mut` / `scope` / `scope_mut` / `shared` / `is_shared`；前两组里按句柄开 scope 的两个直接交出 `Scope` / `ScopeMut`，失败即 panic，要 `Result` 就用 `try_scope` / `try_scope_mut` | `crates/unlit_ecs/src/world/scopes.rs`、`tree.rs` |
| 作用域只读 | `Scope`：`entity` / `query` / `query_one` / `query_many` / `archetype_of` / `children` / `queue` 等；`query_one` 找不到那一行即 panic，预料缺席用 `try_query_one`；已知实体列表批量取用 `query_many`（先整体校验，任一被拒则不交出任何项，错误列出全部被拒实体，同一实体不可重复） | `crates/unlit_ecs/src/view.rs`、`query/many.rs` |
| 作用域可写 | `ScopeMut`：`spawn` / `spawn_named` / `spawn_at` / `despawn` / `link` / `unlink` / `apply` 等 | 同上 |
| 落地排队命令 | `World::apply`：按序应用，期间新排的也应用，失败不中断 | `crates/unlit_ecs/src/world/commands.rs` |
| 单例 | `bundle![...].singleton::<T>()` 声明「这个 world 至多一份」；`Scope::singleton::<T>()` 交出持有者 `Entity`（键被占但持有者不可见时也返回 `None`）。再 spawn 一份即 panic（`spawn_at` 则返回 `EntityAccessError::SingletonTaken`），持有者 despawn 后键随之释放。渲染侧已用它标记 `Presenter` / `WinitWindow` / `Resumed` / `OpenUrls` / `Pacing` / `Interactive` / `LocalTasks` / `GlobalAudioGain` / `InputEvents` | `crates/unlit_ecs/src/bundle.rs`、`crates/unlit_ecs/src/view.rs` |
| 渲染器 | `Renderer::new` / `render` / `context` | `crates/unlit3d/src/renderer.rs` |

## pass 与 source

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| pass 组件 | `Pass { frame_pass, load }` + `Pass::new(order)`；Bundle 展开成两者 | `crates/unlit3d/src/pass.rs` |
| 绘制顺序 | `FramePassOrder(pub i32)`，低者先录 | 同上 |
| 附件 | `FramePass::set_attachments`：深度 / MSAA 视图 + 颜色视图 | 同上 |
| 视口 | `FramePass::viewport: Option<ViewportRect>`（字段在 pass.rs）；`ViewportRect::new(w, h)` / `at(x, y)` / `with_depth(min, max)` / `fit_aspect(aspect, target_w, target_h)` | `crates/unlit_wgpu/src/scene.rs` |
| 每 pass 信息 | `PassInfo { pass: PassId, target: RenderTargetId, frame: FrameId, surface: SurfaceKey, width, height, viewport, camera }`，source 靠它知道这次构建属于哪个 pass、哪个目标、哪一帧、目标多大。`SurfaceKey` 只是管线特化键，两个目标可以共用，按目标缓存状态要用 `target`，按 pass 缓存（如线条实例流）用 `pass`。`RenderTargetId` 记的是整组附件（`color`/`depth`/`msaa` 各一个 `Option<ResId>`）：同一个颜色视图但有无深度或采样数不同的两个 pass 是两个目标 | `crates/unlit3d/src/pass.rs`、`crates/unlit3d/src/target.rs` |
| 清屏 | `RenderLoadOps { color, depth, stencil }`（定义在 `crates/unlit3d/src/components.rs`），默认 `color_clear()` / `depth_clear()`（清 0.0，reverse-z 的远平面）/ `stencil_clear()`；三个 `const fn` 在 `crates/unlit_wgpu/src/render_attachments.rs`，经 prelude 重导出 | `crates/unlit3d/src/components.rs`、`crates/unlit_wgpu/src/render_attachments.rs` |
| source 组件 | `Source::new(SourceOrder, impl DrawSource)` + `pub order` 字段（直接写它即可重排）/ `as_mut::<T>` / `as_ref::<T>` / `build_scene`。实现 `SceneSource`（`fn build(&mut self, SceneBuild<'_>, &mut Scene)`）即有 `DrawSource` 的 blanket 实现；`SceneBuild { scope, ctx, info, encoder }` 把四个上下文一次给出 | `crates/unlit3d/src/source.rs` |
| source 顺序 | `SourceOrder::MESH = 0`、`::LINE = 50`、`::BLITZ = 90`、`::EGUI = 100`：线默认夹在 mesh 与 egui 之间，因为它测 mesh 写下的深度；Blitz 页面是覆盖整个目标的不透明文档，所以在 mesh 之上、叠加层之下。顺序属于实体而非源类型，`Source::new` 的第一个参数就是它；同类源的两个实体可以给不同顺序 | 同上 |
| 自定义 source | 首选实现 `trait SceneSource`（只有 `build`），`trait DrawSource::build_scene` 是低层形式，只在需要控制 encoder 超出上传范围时实现。顺序不来自类型，而来自 `Source` 的 `order` 字段。会被**每个 pass 各调一次**：per-pass 状态按 `info.pass` 存，per-target 状态按 `info.target` 存 | 同上 |
| 附件分配 | `FrameAttachments::new(world, ctx, AttachmentDesc { format, size, sample_count, depth: Option<DepthAttachment> })`，只分配**深度**（`depth` 为 `Some(..)` 时；`Transient` 每 pass 后丢弃、`Persistent` 可 `LoadOp::Load`）与 **MSAA**（`sample_count > 1` 时）两张纹理并把视图登记进图；**颜色附件是调用方的**，两者用 `FramePass::set_attachments` 配到一起。`resize` 重建时视图 id 不变，尺寸只有一份（读自视图），`FrameAttachments::size(&world)` / `Pass::target_size(&world)` 都读它 | `crates/unlit3d/src/attachments.rs` |
| 目标特化键 | 画的时候 `Pass::attachments()` 把三个视图装配成 `RenderAttachments`，它的 `surface_key()` 才是 `SurfaceKey` 的来源。**`FrameAttachments` 上没有这个方法** | `crates/unlit_wgpu/src/render_attachments.rs` |

## mesh 与材质（MeshSource）

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 建 | `MeshSource::new(&Scope, ctx)` + `device` / `queue` / `capabilities` / `context` | `crates/unlit3d/src/mesh_source.rs`（`mesh_source/` 目录下的子模块由它声明） |
| 注册管线族 | `register_family::<K, F, I>`；同一 key 类型重复注册会 panic | 同上，`families.rs` |
| 自定义管线 key | `RenderPipelineKey` 只要求 `variant()`，变体须 `Clone + Hash + Eq`（每可见实体每帧查一次）。选项小且可哈希（如 `LitOptions`）就 `Hashed<选项>`；描述符较大时变体只装蓝图的**输入**（`wgpu::ShaderModule` 等句柄按身份 `Hash`/`Eq`，`VertexLayout`、`Option<IndexFormat>` 亦然），在 `PipelineVariant::descriptor` 里用这些输入构造 `RenderPipelineDesc`——未命中时才构造一次，变体可 `derive` 全套 | `crates/unlit_wgpu/src/specialize.rs`、`crates/unlit_wgpu/src/util.rs` |
| 建 mesh | `allocate_mesh` + `MeshDescriptor { vertex_buffers, indexing, aabb, bind_group, skin, morph_deltas }`，返回 `Result<GpuMesh, MeshError>`；`indexing` 是 `MeshIndexing::NonIndexed { vertex_count }` 或 `MeshIndexing::Indexed { index_buffer, format, count }`，count 与 buffer 的匹配由 `validate` 先行校验（`MeshDescriptor` 定义在 `crates/unlit3d/src/mesh.rs`，与 `MorphDeltas` 同处） | 同上；`MeshDescriptor` 搜 `crates/unlit3d/src/mesh.rs` |
| 建纹理 | `register_texture_and_default_view` 返回 (纹理、视图) 两个句柄 | 同上 |
| 建采样器 | `register_sampler` | 同上 |
| 建材质 | `allocate_material`，闭包拿到 `&ResourceGraph` 建 BindGroup | 同上 |
| 释放 | drop 最后一个 `GpuMesh` 句柄即弃用 mesh；`maintain` 收集 root 并归还池区间与 metadata 槽 | 同上 |

## 内建 lit family

定义在 `crates/unlit3d/src/lit.rs`（策略与实例组件）与 `crates/unlit3d/src/lit/allocate.rs`（注册与分配）；注册是 `MeshSource` 的 inherent 方法，**不需要额外 trait import**。

| 项 | 是什么 |
| --- | --- |
| 注册 | `MeshSource::register_lit_family` |
| 建 mesh | `allocate_lit_mesh`，第二参是 `&key.options`（不是 `&key`）；它只是 `MeshSource::allocate_pooled_mesh`（共享池子的通用入口）在 lit 上的薄适配，自有整块缓冲的 mesh 用 `allocate_mesh` |
| 建材质 | `allocate_lit_material`，同上 |
| 描述 | `LitMeshDesc`：`positions` 必填，其余通道都是 `Option`，照抄时用 `..Default::default()` 补齐。`normals` 是 `expanded` / `inverted_hull()` 外扩壳的方向，缺了画不出壳 |
| key | `LitPipelineKey::new(LitOptions)`，`pub options: Hashed<LitOptions>`；受光与否由 `LitOptions::unlit` 定（`false` 受光） |
| 管线组件 | `GpuRenderPipeline::new(key)`，不带 key 类型参数 |
| 画一份 | `LitMeshBundle`（`crates/unlit3d/src/lit/bundle.rs`）——一条受光绘制的组件，`transform`、`primitive`、`skin`、`morph`；`primitive` 携带 mesh/pipeline/color 等可复用绘制数据 |
| 画模型 | `LitModelBundle`（`crates/unlit3d/src/lit/model.rs`）——根 `Transform` 加节点层级、动画、蒙皮与 morph；glTF 用 `LitGltf::to_model` 适配 |
| 每实例覆盖 | `InstanceColor { color }` / `InstanceCutoff { cutoff }` / `InstanceHullWidth { width }` / `InstanceVisibility { visible }`。都是**具名字段**结构体，**不是元组结构体**：前三个有 `new(..)`，四个都有 `Default`（`InstanceVisibility` 没有 `new`） |
| 实例记录 | `LitInstance`（family 的 `InstanceData`） |
| 排序 | `ZSortedDrawing`：带它的在不带它的之后画，按视轴深度从后往前；不改 blend |

## 管线策略 LitOptions

`crates/unlit_wgpu/src/pipeline/options.rs`，搜 `pub struct LitOptions`（`pipeline.rs` 只做聚合重导出）。**无 `Default`**，用构造函数起底再改字段。

| 构造 | 内容 |
| --- | --- |
| `LitOptions::standard()`（**无参**） | 背面剔除、过滤型 base-color、`DepthStencilPolicy::default()`（写深度 + `Greater`，即 reverse-z）、`blend: None`、`unlit: false` |
| `LitOptions::inverted_hull()` | `unlit` + `expanded` + 剔 `Front` + 不写深度；与 `standard()` 配对做描边壳，见 [SCENE.md](SCENE.md) |
| `Shader::new(label, source)` | 一整块 WESL 模块，**替换**（不是插入）内置入口着色器：它挂在被替换的 `lit.wesl` / `line.wesl` 所处的包路径上，自己声明 `vs_main` / `fs_main`，按需 `import package::lit_bindings` / `lit_vertex` / `lit_lighting` / `lit_fragment`（或 `line_bindings` / `line_vertex` / `line_fragment`）组合。同一个 `Shader` 既能放进 `LitOptions::shader`，也能放进 `LineOptions::shader`；入口点名与顶点流、布局仍是变体决定的。取值访问函数 `label()` / `source()` 都返回 `&str` |

字段分两类：**着色开关**（`shader` / `unlit` / `srgb_to_linear_output` / `base_color` / `alpha_cutoff` / `expanded` / `texture_filtering`）与**几何与混合**（`primitive` / `depth_stencil` / `blend` / `write_mask`）。

**没有** `color_target` / `multisample`，`depth_stencil` 也不是 `Option<wgpu::DepthStencilState>` 而是 `DepthStencilPolicy { depth_write_enabled, depth_compare }`（`Default` 为 `true` / `Greater`）。颜色格式、采样数、深度格式属于 `SurfaceKey`（`crates/unlit_wgpu/src/specialize.rs`）：`Renderer` 从 pass 附件算出后经 `PassInfo::surface` 交给 source，内建 lit family 把 `draw.surface` 折进 `LitVariant`；**只有自己实现 family 时才需在管线描述符上调 `SurfaceTarget::set_surface`**，漏了就是 pipeline 创建失败。

深度格式由 `default_depth_stencil_format(device)`（`crates/unlit_wgpu/src/render_attachments.rs`）选：支持 `DEPTH32FLOAT_STENCIL8` 就用它，否则 `Depth24PlusStencil8`。

## 灯光与分簇

模块文档在 `crates/unlit3d/src/light.rs`（照光源语义的那三条，值得先读）；GPU 侧的打包、分簇与上限在 `crates/unlit_wgpu/src/light.rs`。

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 组件 | `LightSource::new(impl Light)`；所有灯共用这一个组件类型，一帧一次 query 全收 | `crates/unlit3d/src/light.rs` |
| 四种灯 | `DirectionalLight { color, intensity }`（无位置、无 range）、`PointLight { .., range }`、`SpotLight { .., range, inner_angle, outer_angle }`（两个角都是**全角**、弧度；两者相等即硬边）、`QuadLight { .., range, vertices: [Vec3; 4] }` + `QuadLight::rect(color, intensity, range, width, height)` | 同上 |
| 环境光 | `AmbientLight { color: Vec4 }` / `AmbientLight::new(color)`（`const fn`）。**没有 intensity**：颜色就是那一项，一帧把 source 作用域内所有环境光**相加** | 同上 |
| 自定义灯 | `trait Light`：`pack(world_from_local, LightSink)` 写 GPU 槽位、`describe(world_from_local) -> LightDescribe` 给 CPU 侧排序与剔除用；两者都要实现 | 同上 |
| 描向 | 沿实体自己 `Transform` 的 `-Z` 照；方向光与聚光灯靠**旋转实体**瞄准，quad 光的多边形在实体 `XY` 平面；没有 `Transform` 就坐在原点朝 `-Z` | 同上 |
| 强度 | 无量纲：片元把 base color 乘上各灯 `color × intensity` 之和，无曝光、无能量守恒。点光/聚光按 `range` 做平滑衰减，quad 光走多边形形状因子（自带平方反比，没有单独的 range 衰减） | `crates/unlit3d/src/light.rs` |
| 示例封装 | `PlacedLight { light, translation, rotation }` + `PlacedLight::aiming(light, at, target)`：把 `-Z → 目标方向` 的旋转算好——**示例 bundle，不是库 API** | `unlit3d_examples/src/scenes.rs` |
| 上限 | `MAX_DIRECTIONAL_LIGHTS = 4`：按 `intensity` 排，最弱的丢掉（且不占簇槽位，因为方向光根本不分簇）；`LIGHTS_PER_CELL = 15`：每簇能引用的有界灯数，满了丢最弱的；`QUAD_LIGHT_CORNERS = 4`（`QuadLight::vertices: [Vec3; QUAD_LIGHT_CORNERS]`）。被接收面的地平线平面裁掉一块后最多再多出 1 个角，所以**存裁剪结果**的数组写 `QUAD_LIGHT_CORNERS + 1`，没有第 5 个角的常量；裁剪后不足 3 个角直接算 0（Rust 与 WGSL 各有一份同名常量，实际生效的是 `crates/unlit_wgpu/shaders/polygon.wesl` 那份） | `crates/unlit_wgpu/src/light.rs` |
| 分簇配置 | `MeshSource` 的 `lights.clustering` 字段（`pub`），类型 `ClusterConfig { cell_pixels, z_slices, max_clusters, first_slice_depth, far }` + `ClusterConfig::DEFAULT`；从 prelude 直接可用 | `crates/unlit_wgpu/src/light.rs` / `crates/unlit3d/src/mesh_source/lighting.rs` |
| 多边形光 | 少于 3 个角无面积、不画；重复一个角可以拼三角，相差极小的角会被合并 | `crates/unlit3d/src/light.rs` |
| 关闭某盏灯 | `describe.intensity <= 0.0` 的灯在收集时直接跳过（不用 despawn，但 despawn 更省一次遍历） | 同上 |
| 上传 | `upload`（`mesh_source/lighting.rs`）每帧在 `build_scene` 里跑，前提与实例上传相同——这帧有活动相机且可见集非空——但**与画不画受光 mesh 无关**：分簇在 CPU 上重算一遍并整块重传，因为相机与灯都可能动 | `crates/unlit3d/src/mesh_source.rs` |

## 相机与变换

| 项 | 是什么 |
| --- | --- |
| 相机 | `Camera { view_from_world, clip_from_view, active }` + `clip_from_world()` / `position()`，在 `crates/unlit3d/src/components.rs` |
| 变换 | `Transform { translation, rotation, scale }`，`Default` 是单位变换 |
| 可见性 | `InstanceVisibility { visible }`、`MeshParts`、`SkinBinding` / `SkinPose`、`MorphBinding` / `MorphWeights` |
| 投影 | `glam::camera::rh::proj::directx::perspective_infinite_reverse`；视图 `glam::camera::rh::view::look_at_mat4` |
| 剔除 | `culling::is_culled`、`bounds::{Aabb, Obb, FrustumPlanes}` 自己算视锥剔除；`Renderer` 不做遮挡剔除 |

## 帧

全在 `crates/unlit3d/src/frame.rs`。

| 项 | 是什么 |
| --- | --- |
| 帧 | `Frame { delta_time: FrameDelta, index }`；`FrameDelta::from_secs` 将负值和非有限值视为零，数值边界用 `as_secs()` 或 `as_secs_f32()` |
| 时钟 | `FrameClock { time, delta_time, index }` + `advance` |
| 派发 | `dispatch_frame`：先 reset `FrameSkip`、推进所有 `FrameClock`，再把 `OnFrame` 按 order 跑一遍（只按 order 排序，实体不参与比较），最后 `world.apply()`——**apply 失败只 `log::error!`，不返回错误** |
| 行为 | `OnFrame::new(f)` 不带顺序、`OnFrame::with_order(order, f)` 带顺序。回调是 `FnMut(&World, Entity, &mut Frame) + 'static`——**是 `&mut Frame`，不是 `&Frame`** |
| 顺序 | `FrameBehaviourOrder(pub i32)`，低者先跑，**同序无序**；dispatch 期间不得增删行为 |
| 跳过 | `FrameSkip`：**必须用单例键 spawn**（`bundle![FrameSkip::default()].singleton::<FrameSkip>()`），`skip`/`is_skipped`/reset 都只认那把键，不带键的实例读不到也写不到；由声明了更晚顺序的行为去读 |

## 输入

全在 `crates/unlit3d/src/input/`。

| 项 | 是什么 |
| --- | --- |
| 写端 | `InputWriter::new` + `push(event)` |
| 订阅组件 | `InputFeed(InputWriter)`（带写端）与 `InputPair`（只带缓冲与 `InputState`，由 `InputWriter::subscribe()` 返回；每个世界至多一个，单例键 `InputEvents`）；spawn 进世界才收事件 |
| 派发 | `dispatch_input(&World) -> bool`：取一个 batch、折进 `InputState`、跑行为；不 apply 结构变更 |
| 读状态 | 直读 `InputState` 组件（与 buffer 同实体、由本 world 的 tick 独占写入）；批走 `InputEvents::with_batch` / `copy_events_into`，tick 用 `refresh`（把批折进同实体的 state）。要按批重放就得用批初状态 `InputEvents::copy_baseline_into`——`InputState` 已含本批，拿它当起点会把每处改动算两遍 |
| 行为 | `OnInput`（catch-all）/ `OnKey` / `OnPointer` / `OnMouse` / `OnTouch` / `OnText` / `OnIme` |
| 分发 | `subscribe_input(&mut World)`（给 world 自己的缓冲登记订阅）、`distribute_input(&World)`（把根 world 的 writer 复制进每个嵌套 world 并登记订阅） |

## egui

| 项 | 是什么 |
| --- | --- |
| source | `EguiSource::new()`、`publish_target(Snapshot<EguiTarget>)`、`context()` / `context_mut()` |
| 面板 | `EguiPanel::new(f)`，`f` 是 `FnMut(Scope<'_>, Entity, &mut egui::Ui)`——**第一个参数是 `Scope`，不是 `&World`** |
| 跨 world | `ExternalFrame` / `ExternalFrames::new / publish / with_frames`、`EguiTarget { screen, max_texture_side }`、`lay_out`、`raw_input`。`publish` 会把被顶掉帧的纹理增量 `append` 给继任者——egui 只在写出纹理的那一帧带增量，丢掉它就没材质 |

## blitz

| 项 | 是什么 |
| --- | --- |
| source | `BlitzSource::new()`——无参；绘制自己作用域里携带的第一个 `BlitzDocument`，没有就不画 |
| 页面 | `BlitzDocument::new(markup)`、`set_markup(markup)`、`set_base_url(base)`、`insert_resource(path, bytes)`——**挂在 source 实体之下**，否则 source 看不见 |
| 回调 | `OnBlitzEvent::new(f)`，`f` 是 `FnMut(Scope<'_>, Entity, &mut BlitzEvent<'_>)`；`BlitzEvent` 给 `data()`（其 `.name()` 是 "click"/"input"/"keydown" 等 DOM 事件名）、`target()`、`chain()`（事件指向的节点及其元素祖先，回调对整条链只跑一次）、`document_mut()`、`prevent_default()` / `request_redraw()`。一次物理动作可能派发多个事件（pointer 与 mouse/touch 兼容事件各一遍），按 `data().name()` 过滤 |
| 输入 | 帧输入自动驱动文档：指针（含笔）、滚轮、键盘、文本（`InputEvent::Text` 合成 Character 按键）、IME；坐标换算成 CSS 像素。键盘事件带 `Key` 推出的 `Code` 与 `KeyLocation` 推出的左右位置，二者只在回调里可见；会打出字符的键只陈述一次（按下带上那份文本，松开用同一个字符名），不产生文本的按下按它本身陈述，粘贴那次文本没有物理键。悬停、焦点、滚动、文本编辑与 checkbox/radio 勾选状态都由 Blitz 维持。被多个 pass 够到的源只取一个 tick 的输入，一次点击只派发一次。一批事件从它排队时的状态（`InputEvents::copy_baseline_into`）重放，逐个事件推进，因此批内先移动后松开时报的是移动时按着键、滚轮报的是它转动时的位置；读到更早一批或页面中途被换掉的源，也从世界持有的状态重放，不沿用自己上一批的状态。每个指针只陈述自己的按键：按下的笔在造成按下的那个事件里就报出按下，按着的手指把按键带进下一帧，笔还按着时松开鼠标只抬起鼠标；触点按到达顺序排，第一个是主触点。指针在它最后所在的位置结束：说不出位置的移动或按下会被丢掉，结束事件（松开、取消、离开）即使没带位置也报在那个位置。丢失焦点会取消还按着的每个指针（触点从最早的开始）并放开修饰键，因为本该结束它们的事件去了抢到焦点的窗口 |
| 字节 | 无网络。`data:` URL 当场解码；其余由 `insert_resource` 按路径提供，路径在加载时按当前基准解析；没人提供的路径收到空字节（样式表读成空表、图片读成加载失败），页面照常画 |
| 布局与绘制 | 用 Blitz 布局、Vello 的 GPU 后端在帧的构建阶段光栅化进一张与 pass 同尺寸的 `Rgba8Unorm` 纹理（命令录进本帧编码器），再由共享的全屏合成贴上目标；页面图片各上传成自有纹理并按图片缓存；重绘时机 = 标记/视口/尺寸变化、输入或回调要求、文档有动画 |
| 字体 | 默认字体是 egui 的 `epaint_default_fonts::HACK_REGULAR`（`blitz_epaint_fonts` 特性），`blitz_sc_fonts` 另加 Noto Sans SC 一级+二级子集承载汉字（四个通用族里排在默认字体之后，并挂上汉字回退表），系统字体默认关闭（`blitz_system_fonts` 才加载），各平台字形一致；内置字体按进程读一次，每个页面按自己的集合布局，`@font-face` 注册的族只有加载它的那个页面匹配得到 |
| 边界 | 页面只向宿主请求重绘；光标形状、IME 开关、剪贴板都没接通（输入法的预编辑与提交本身会送到页面，只是窗口不打开平台输入法），事件含义由 Rust 决定。CSS 滤镜只画 Flood／GaussianBlur／DropShadow／Offset 这四种单原语图，色彩滤镜（brightness／grayscale／sepia 等）与多原语图按无滤镜绘制 |

## winit 桥

| 项 | 是什么 |
| --- | --- |
| 宿主 | `WinitHost<U>::new` + `world` / `world_mut` / `run(event_loop)`，在 `crates/unlit3d/src/winit/event.rs` |
| 内置行为 | `WindowSetup`、`ExitOn(ExitTrigger)`（`ExitTrigger` 是 enum，`CloseRequested` 与 `Escape` **二选一，不是位标志**）、`ExitRequest`、`CreateWindowRequest`、`request_redraw_while_foreground`，在 `crates/unlit3d/src/winit/builtin.rs` |
| 窗口 | `WindowSurface::new` / `acquire(nested_world, &mut renderer)`、`Frame::color_view()` / `present(&queue)`，在 `crates/unlit3d/src/winit/surface.rs` |

## 多 world（unlit3dx）

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| runner | `trait WorldRunner`：`const SELF_DRIVEN` + `spawn<I, S, T>`（init / tick 两个闭包） | `crates/unlit3dx/src/world.rs` |
| 自带线程 | `spawn_threaded_world` / `..._with_init`；tick 返回 `TickOutcome`：`Running` 继续，`Finished` 结束 | 同上 |
| 定步长 | `spawn_stepped_world_with_init`（返回 `Result`，setup panic 是 `ThreadError::Setup` 且不留半成品）、`step_stepped_world`、`with_stepped_world` | 同上 |
| 节拍 | `ThreadControl::self_driven` / `wait_timeout`、`FixedClock::new` / `set_step` / `wait`、`fixed_steps` 与 `MAX_CATCH_UP_STEPS = 4` | `crates/unlit3dx/src/thread.rs` |
| 子 world | `spawn_sub_world`、`drain_sub_world`、`tick_sub_world`（返回 `StepOutcome`；drain + 折叠输入 + 本地任务 + `dispatch_frame`，驱动子 world 一帧就用它） | `crates/unlit3dx/src/host.rs` |
| 失败成因 | `WorldError` = `Panic { stage: WorldStage, message }` / `Command { source: EntityAccessError }` / `Step { source: StepError }`；`WorldStage` = `Setup` / `Job` / `Command` / `Body` / `Publish`。`StepOutcome::Failed(WorldError)` 与 `WorldState::Failed(WorldError)` 携同一个成因 | `crates/unlit3dx/src/turn.rs` |
| 批量失败 | `drain_all_stepped_worlds(host) -> Vec<WorldFailure>`（`{ name, error }`）、`join_failures(&[WorldFailure]) -> String` | `crates/unlit3dx/src/world.rs` |
| 端点 | `WorldEndpoint`：`entity()` / `commands()` / `state()` / `ownership()` / `driving()` / `is_finished()` / `stop()` / `join_timeout()` / `detach()`；字段全私有，identity、队列、状态与生命周期不可拆。`WorldOwnership` = `HostOwned` / `ThreadOwned`、`WorldDriving` = `Continuous` / `Stepped`；所有 spawn 都返回它，包括 `spawn_sub_world`（`HostOwned` + `Stepped`） | `crates/unlit3dx/src/endpoint.rs` |
| 名字与状态 | `WorldName(&'static str)`；`Threaded` / `Stepped` 标记；`WorldState` = `Running` / `Stopped` / `Failed(WorldError)`——**带成因，`match` 时要绑定** | `crates/unlit3dx/src/world.rs`、`thread.rs` |

| 通信 | 是什么 | 语义 |
| --- | --- | --- |
| job | `type Job = Box<dyn FnOnce(&mut World) + Send>`；`JobSender::send` | 有界队列，永不阻塞，世界每次 tick 前 drain；被拒绝时 `SendError` **带着 job 回来**：`kind()` 给 `SendErrorKind::Full` / `Closed`，`into_job()` 取回那份工作。没跑过的 job 重发安全；想放弃就 drop 错误，job 随之而去（连同它持有的状态）。已入队但被停止的世界丢掉的 job 可能跑过一半，没有谁会自行重发它 |
| 状态 | `Snapshot<T, E>`：`publish` / `reader` / `has_readers`；`SnapshotReader::latest` / `poll` | 单槽。`poll` 按 reader 区分 `Uninitialized` / `Unchanged` / `Updated` / `Closed` / `Failed`；终止后 `latest()` 仍返回最后的值 |
| 事件 | `Stream<T, E>`：`subscribe(NonZeroUsize)` / `publish` / `publish_all`；`StreamReader::pump` / `with_batch` | 每个订阅有独立有界队列，clone 共享同一队列。`pump` 先交付已接收的事件，再报 `Closed` / `Failed` / `Overflow`；overflow 只结束队列满的 reader |
| 订阅 | `JobSender::watch` / `subscribe` → `Subscription<Reader>`；`Subscription::poll()` | 返回的是句柄，不是 reader：入队 ≠ 已安装，world 到下一轮才跑安装 job，之前 `poll()` 答 `None`。装上后 `Some(Ok(reader))`；world 先停则 `Some(Err(InstallError::Ended))`，world 跑了却够不到自己的 outlet 则 `Some(Err(InstallError::Outlet))`。队列满时整个调用仍返回 `Err(SendError)` |
| 应答 | `Pending::new(endpoint, Option<WallTimeout>)` / `request(key, make)` / `poll(&mut settled)` | `Pending` 只向绑定的 `WorldEndpoint` 发送。`Settled` 区分 `Answered` / `TimedOut` / `WorldStopped` / `WorldFailed` / `Cancelled`。超时只结束等待；`cancel(&key)` 发信号，job 在运行前或 `request_cancellable` 的检查点确认取消。`Asked::Coalesced` 不运行新闭包、不重置首个请求的超时 |
| 发布 | `publish_collected(world) -> usize` | 世界里收集到的快照/流在 tick 之后统一发布 |

## 展示路径（unlit3dx）

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| GPU | `Gpu` 就是 `unlit_wgpu::context::GpuContext`：`GpuContext::headless(tier, power_preference, label)` 自建 instance，`GpuContext::present_to(instance, window, tier, power_preference, label)` 用窗口 surface 探测适配器后丢弃它；`label` 是设备在校验信息里的名字，两者都 async 返回 `Result<GpuContext, GpuRequestError>`。`request_gpu(app, ready, failed)` 返回 `OnFrame`，把请求驱动成帧任务；成功时调用 `ready(world, gpu)`，失败时调用 `failed(world, GpuStartError)` 并请求退出 | `crates/unlit_wgpu/src/context.rs`、`crates/unlit3dx/src/gpu.rs` |
| 常量 | `SCENE_PASS_ORDER = FramePassOrder(0)`、`BLIT_PASS_ORDER = FramePassOrder(1)`、`PRESENT_ORDER = 10` | `crates/unlit3d/src/winit/presenter.rs` |
| 两条路径 | `PresentationPath`：`Surface { samples, with_depth }` 直画交换链；`Intermediate(TargetConfig)` 画进常驻纹理再加一个 blit pass 贴回交换链。默认 `Surface { samples: OFFSCREEN_SAMPLES, with_depth: true }` | 同上 |
| 呈现模式 | `SurfaceOptions { present_mode, desired_maximum_frame_latency }`，默认 `Fifo` + 2 在途帧。表面缺所请求的模式时降级到 `Fifo`（连 `Fifo` 都没有才退回它列出的第一个）并打警告；`AutoVsync` / `AutoNoVsync` 原样交给 wgpu 解析 | `crates/unlit3d/src/winit/surface.rs` |
| 装配（库） | `build_presenter(world, render, renderer, scene_pass, surface, instance, adapter, fallback_size, path, surface_options)` → `Presenter`；`setup_presenter(..)` 同参再 spawn 成组件并返回 `Entity`。`surface` 由调用方 `instance.create_surface(window)` 造 | 同上 |
| 装配（dx） | `setup_presenter(world, render, renderer, scene_pass, gpu, fallback_size, offscreen: bool, surface_options)`，内部造 surface 并映射成 `Intermediate(TargetConfig::default())` / `Surface`，返回 `Result<(), PresenterError>`——**是 enum，不是 `String`** | `crates/unlit3dx/src/present.rs` |
| 组件方法 | `Presenter::present(host)` / `present_with(host, prepare)` / `resize(host, size)` / `release(host)`；`present_with` 先取得交换链图像，再运行准备回调，成功呈现才返回 `Ok(true)`，没有图像时返回 `Ok(false)`；字段**全私有**，只能经由 `build_presenter` / `setup_presenter` 造 | `crates/unlit3d/src/winit/presenter.rs` |
| 每帧行为（dx） | `present_turn(world, render, &mut frame, prepare)` 把模拟推进与呈现绑成一趟事务：它自己 `tick_sub_world`（drain + 折叠输入 + 本地任务 + 分发帧）再调 `Presenter::present`；只想呈现时 `prepare` 传 `\|\| Ok(())`；结果是 `TurnOutcome::{Presented, Skipped, Suspended, Unprepared}`（`Suspended` 是窗口挂起，`Unprepared` 是呈现路径还没建起来）；`pump_presented()` 返回 `(OnAboutToWait, OnWindowEvent)` 两个要分别 spawn；`resize_presented` | 同上 / `crates/unlit3dx/src/present.rs` |
| 运行形态（dx） | `RunShape::parse(&options)` 把全部开关读成一个值：`ownership`（`Hosted`/`Threaded`）、`driving`（`Loop`/`Client`）、`offscreen`、`transports`（`stdio` 与 `http` 可同时开）、`frames`、`capture`；`shape.pacing()` 是唯一的 `Pacing` 投影；本构建满足不了的开关在解析处报 `RunError::NoMcpServer`/`NoHttpTransport` | `crates/unlit3dx/src/app/shape.rs` |
| 每帧行为（单 world） | `present_frames()` / `pump_presented()` / `resize_presented()`：`present_frames` 跳过 `FrameSkip` 标记的帧、且 winit 宿主报告挂起时不呈现（无 `Resumed` 组件的 world 视为已恢复），所以设标志的行为要给更小的 order；它不懂示例的 `Scene`，重述 viewport、`Scene::end_frame` 要自己挂行为 | 同上 |
| 宿主前置任务 | `pump_presented_with(Some(Box::new(pump)))`：`pump: HostPump = Box<dyn FnMut(&mut World)>`，每帧 dispatch 前在宿主 world 上跑一次，用来排空子 world 的 job、poll 宿主任务 | 同上 |
| 纯离屏 | `PersistentTarget::bind(world, context, pass, &device, size, TargetConfig { format, samples, with_depth, size })` + `readback_texture`；`resize` 原地 `replace` 纹理与 view，节点 id 不变 | `crates/unlit3d/src/offscreen.rs` |
| 尺寸 | `IntermediateSize`：`Scale(f32)`（窗口倍数，`1.0` 跟随窗口，`0.5` 半宽半高，至少 1px）或 `Fixed(u32, u32)` | 同上 |

## 物理 / 音频 / 合成

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 物理 | `Physics::new(gravity)`、`init_physics_world`、`spawn_physics_world_with::<R>` | `crates/unlit3dx/src/physics.rs` |
| 刚体 | `spawn_body`、`spawn_fixed_cuboid`、`despawn_body`、`step_once` | 同上 |
| 常量 | `PHYSICS_HZ = 60.0`、`FRAME_SECONDS` | 同上 / `crates/unlit3dx/src/lib.rs` |
| 音频 | `spawn_audio_world_with::<R>`、`dispatch_audio_fill`、`OnAudioFill::new(order, f)`（**第一个参数是 `AudioFillOrder`**）、`AudioFillOrder`、`OnAudioPostFill::new(f)`（观察 `AudioMix`，不可改样本）、`AudioMix`、`GlobalAudioGain`（**每 world 一份，分发器取查询交回的第一份**）、`AudioConfig` | `crates/unlit3dx/src/audio.rs` |
| 合成 | `load_soundfont`、`load_midi_file_with_loop_type(bytes, loop_type)`（**没有无 loop_type 的 `load_midi_file`**；手搓事件序列用 `midi_file`）、`SynthVoice::from_font`（**一台音色绑一份字体；`from_font` 可对同一 `Arc<SoundFont>` 生成多台，各持独立 16 通道**；共用一台音色的文件须分占互不相交的通道）、`MidiSequencer::new` / `advance` / `rewind`、`midi_job` | `crates/unlit3dx/src/synth.rs` |

## glTF

用 `LitGltf::insert_asset(source, world)` 准备默认场景，再将 `asset.model` 放进 `LitModelBundle`，在同一个 mesh source 下 spawn。只要指定片段时，用 `insert_asset_with(source, world, clips)` 传入文档的 `AnimationId`，返回的模型按传入顺序编号。模型自己持有绘制所需的句柄，不需要保留文档或 `asset.resources`。自定义上传用 `GltfResources` 按类型化 ID 组织句柄，再调用 `to_model`；图元缺 mesh/材质会返回 `Error::MeshNotInserted` / `Error::MaterialNotInserted`，模型数据校验失败会返回 `Error::Model`。

移动整个实例时改根实体的 `Transform`；选片段写 `LitModelAnimation::clip`（也可用 `LitModel::clip_index(name)` 按名字找，比位置稳），时间由 `LitModelAnimation::clock` 决定：`LitModelClock::Playing { speed, looping }` 由帧派发按 delta 推进，`LitModelClock::Manual` 把播放交给调用方。

帧派发完全不管手动时钟的实例：它既不推进时间也不求值，姿态只由调用方的 `LitModelBundle::evaluate(&scope, root)` 产生。所以手动时钟下写入 `AnimationTime::from_secs(seconds)` 后必须自己调一次 `evaluate`，本帧才会画到这个时间；而帧序不再影响结果——排在 `LIT_MODEL_UPDATE_ORDER = -10`（`Playing` 唯一的自动求值点）之前还是之后写都一样。spawn 时的 `OnSpawn` 已采样一次，所以只想渲染一帧（如 CLI 的一次性离屏渲染）不必再 evaluate。手动时钟下改节点基准变换，要等到下一次采样才进画面，不是下一帧。

模型节点、绘制实体、蒙皮姿态与权重实体都是模型自持的派生状态，`LitModelInstance`（`nodes()` / `draws()` / `poses()` / `weights()` / `matrices()`）只读。单独 despawn 一棵子树不受支持：失效的句柄会让那次求值静默跳过它们，留下半活的模型；要移除就 despawn 根。

低层 API：`LitGltf`（`crates/unlit3d/src/gltf.rs`，`gltf/` 目录下的子模块由它声明）：`load` / `from_bytes`、`pipeline_key`、`insert_resources`、`to_model`（装配整份模型为 `LitModel`，放置交给 `LitModelBundle` 的根 `Transform`，不要在节点上另写变换）、`animation_count` / `animation_name` / `animation_duration`、`skin_pose` / `morph_weights`、`insert_image` / `insert_material` / `insert_mesh`（`insert_resources` / `insert_materials` / `insert_material` 都返回 `Result`：图像没插就插材质得 `Error::ImageNotInserted`）。`pipeline_key` 按材质决定无光与否：材质带 `KHR_materials_unlit` 就 unlit，否则 lit。镜像绕序在绘制时按该帧实体变换的行列式符号特化，不在 spawn 时定死；蒙皮网格在着色器里忽略实体变换、顶点由骨骼矩阵直接在世界空间产出，因此实体变换不影响蒙皮网格的绕序。

## 常用常量

| 项 | 值 |
| --- | --- |
| 示例格式 | 定义在 `unlit3d_examples/src/scenes.rs`，是示例侧常量、不是库 API：`TEST_FORMAT = Rgba8UnormSrgb`、`TEST_SIZE = (256, 192)`、`BASELINE_SIZE = (960, 720)`、`SEQUENCE_STEP = 0.5` |
| 相机基线 | 眼 `(0.0, 1.2, 3.2)`、目标 `(0.0, 0.2, 0.0)`、fov 60°、near 0.1 |
