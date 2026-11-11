# API 速查

**这是一份索引，不是签名抄本。** 每条只告诉你「有这个符号、它干什么、去哪个文件搜」，参数与返回类型一律回源码读——项目极早期、API 常改，手抄的签名表每一轮都会漏几条，抄错比抄不到更糟。

排序：可抄的骨架看 `crates/unlit3d/README.md` 的「Example」doctest（`cargo xtask test` 每趟编译它，不会过时）→ 确切签名看源码 → 本文只解决「该搜什么」。

决策与约束见 [SKILL.md](SKILL.md)，用法见 [SCENE.md](SCENE.md) / [MULTI_WORLD.md](MULTI_WORLD.md)，示例与命令见 [EXAMPLES.md](EXAMPLES.md)。

## 启动与 world

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 建渲染上下文 | `spawn_context` 每 world 调一次，GPU 状态进 shared 子树 | `crates/unlit3d/src/source.rs` |
| 上下文句柄 | `RenderContext`，Copy。**六个字段都是 `pub(crate) Entity`，不是 GPU 句柄**，要从外部用只能走它的访问函数（`device` / `queue` / `capabilities` / `clock` / `try_graph` / `graph` / `uniforms` / `maintain_scope`，都吃 `Scope`）；只有 `of(world, anchor)` 是 pub | 同上，搜 `pub fn device` |
| 根作用域 | `World` 的 `root_scope` / `root_scope_mut` / `scope` / `scope_mut` / `shared` / `is_shared` | `crates/unlit_ecs/src/world.rs` |
| 作用域只读 | `Scope`：`entity` / `query` / `query_one` / `archetype_of` / `children` / `queue` 等 | `crates/unlit_ecs/src/view.rs` |
| 作用域可写 | `ScopeMut`：`spawn` / `spawn_named` / `spawn_at` / `despawn` / `link` / `unlink` / `apply` 等 | 同上 |
| 落地排队命令 | `World::apply`：按序应用，期间新排的也应用，失败不中断 | `crates/unlit_ecs/src/world.rs` |
| 渲染器 | `Renderer::new` / `render` / `context` | `crates/unlit3d/src/renderer.rs` |

## pass 与 source

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| pass 组件 | `Pass { frame_pass, load }` + `Pass::new(order)`；Bundle 展开成两者 | `crates/unlit3d/src/pass.rs` |
| 绘制顺序 | `FramePassOrder(pub i32)`，低者先录 | 同上 |
| 附件 | `FramePass::set_attachments`：深度 / MSAA 视图 + 颜色视图 | 同上 |
| 视口 | `FramePass::viewport: Option<ViewportRect>`（字段在 pass.rs）；`ViewportRect::new(w, h)` / `at(x, y)` / `with_depth(min, max)` / `fit_aspect(aspect, target_w, target_h)` | `crates/unlit_wgpu/src/scene.rs` |
| 每 pass 信息 | `PassInfo { surface: SurfaceKey, width, height, viewport }`，source 靠它知道画进什么目标、目标多大 | 同上 |
| 清屏 | `RenderLoadOps { color, depth, stencil }`，默认 `color_clear()` / `depth_clear()`（清 0.0，reverse-z 的远平面）/ `stencil_clear()` | `crates/unlit3d/src/components.rs` |
| source 组件 | `Source::new(impl FrameSource)` + `with_order` / `as_mut::<T>` / `as_ref::<T>` | `crates/unlit3d/src/source.rs` |
| source 顺序 | `FrameSourceOrder::MESH = 0`、`::LINE = 50`、`::UI = 100`：线默认夹在 mesh 与 UI 之间，因为它测 mesh 写下的深度 | 同上 |
| 自定义 source | `trait FrameSource`：`build_scene` + `order`。会被**每个 pass 各调一次**，per-pass 状态按 `info.surface` 存 | 同上 |
| 附件分配 | `FrameAttachments::new` 只分配**深度**（`with_depth` 为真时）与 **MSAA**（`sample_count > 1` 时）两张纹理并把视图登记进图；**颜色附件是调用方的**，两者用 `FramePass::set_attachments` 配到一起。`resize` 重建时视图 id 不变 | `crates/unlit3d/src/attachments.rs` |
| 目标特化键 | 画的时候 `Pass::attachments()` 把三个视图装配成 `RenderAttachments`，它的 `surface_key()` 才是 `SurfaceKey` 的来源。**`FrameAttachments` 上没有这个方法** | `crates/unlit_wgpu/src/render_attachments.rs` |

## mesh 与材质（MeshSource）

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| 建 | `MeshSource::new(&Scope, ctx)` + `device` / `queue` / `capabilities` / `context` | `crates/unlit3d/src/mesh_source/`（入口 `mod.rs`） |
| 注册管线族 | `register_family::<K, F, I>`；同一 key 类型重复注册会 panic | 同上，`families.rs` |
| 建 mesh | `allocate_mesh` + `MeshDesc { vertex_buffers, index_buffer, count, indexed, aabb, bind_group, morph_deltas }`（`MeshDesc` 定义在 `crates/unlit3d/src/mesh.rs`，与 `MorphDeltas` 同处） | 同上；`MeshDesc` 搜 `crates/unlit3d/src/mesh.rs` |
| 建纹理 | `register_texture_and_default_view` 返回 (纹理, 视图) 两个句柄 | 同上 |
| 建采样器 | `register_sampler` | 同上 |
| 建材质 | `allocate_material`，闭包拿到 `&ResourceGraph` 建 BindGroup | 同上 |
| 释放 | `remove_mesh` / `maintain` | 同上 |

## 内建 lit family

全在 `crates/unlit3d/src/lit.rs`；注册是 `MeshSource` 的 inherent 方法，**不需要额外 trait import**。

| 项 | 是什么 |
| --- | --- |
| 注册 | `MeshSource::register_lit_family` |
| 建 mesh | `allocate_lit_mesh`，第二参是 `&key.options`（不是 `&key`） |
| 建材质 | `allocate_lit_material`，同上 |
| 描述 | `LitMeshDesc`：`positions` 必填，其余通道都是 `Option`，照抄时用 `..Default::default()` 补齐。`normals` 是 `expanded` / `inverted_hull()` 外扩壳的方向，缺了画不出壳 |
| key | `LitPipelineKey::new(LitOptions)`，`pub options: Hashed<LitOptions>`；受光与否由 `LitOptions::unlit` 定（`false` 受光） |
| 管线组件 | `GpuRenderPipeline::new(key)`，不带 key 类型参数 |
| 每实例覆盖 | `InstanceColor { color }` / `InstanceCutoff { cutoff }` / `InstanceHullWidth { width }` / `InstanceVisibility { visible }`。都是**具名字段**结构体，**不是元组结构体**：前三个有 `new(..)`，四个都有 `Default`（`InstanceVisibility` 没有 `new`） |
| 实例记录 | `LitInstance`（family 的 `InstanceData`） |
| 排序 | `ZSortedDrawing`：带它的在不带它的之后画，按视轴深度从后往前；不改 blend |

## 管线策略 LitOptions

`crates/unlit_wgpu/src/pipeline.rs`，搜 `pub struct LitOptions`。**无 `Default`**，用构造函数起底再改字段。

| 构造 | 内容 |
| --- | --- |
| `LitOptions::standard()`（**无参**） | 背面剔除、过滤型 base-color、`DepthStencilPolicy::default()`（写深度 + `Greater`，即 reverse-z）、`blend: None`、`unlit: false` |
| `LitOptions::inverted_hull()` | `unlit` + `expanded` + 剔 `Front` + 不写深度；与 `standard()` 配对做描边壳，见 [SCENE.md](SCENE.md) |
| `LitShader::new(label, module, vertex, fragment)` | 三段 WESL 片段**插进**内置着色器的 hook 位置（不做替换）：module 在模块顶层、vertex 在顶点阶段末、fragment 在受光与裁剪之后、目标编码之前（那里颜色是 `var color`，可就地改）。空串=该处保持原样，`lit` 示例只给 vertex 段。取值的访问函数 `label()` / `module()` / `vertex()` / `fragment()` 都返回 `&str` |

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
| 示例封装 | `PlacedLight { light, translation, rotation }` + `PlacedLight::aiming(light, at, target)`：把 `-Z → 目标方向` 的旋转算好——**示例 bundle，不是库 API** | `unlit3d_examples/src/scenes/mod.rs` |
| 上限 | `MAX_DIRECTIONAL_LIGHTS = 4`：按 `intensity` 排，最弱的丢掉（且不占簇槽位，因为方向光根本不分簇）；`LIGHTS_PER_CELL = 15`：每簇能引用的有界灯数，满了丢最弱的；`QUAD_LIGHT_CORNERS = 4`（`QuadLight::vertices: [Vec3; QUAD_LIGHT_CORNERS]`）。被接收面的地平线平面裁掉一块后最多再多出 1 个角，所以**存裁剪结果**的数组写 `QUAD_LIGHT_CORNERS + 1`，没有第 5 个角的常量；裁剪后不足 3 个角直接算 0（Rust 与 WGSL 各有一份同名常量，实际生效的是 `crates/unlit_wgpu/shaders/polygon.wesl` 那份） | `crates/unlit_wgpu/src/light.rs` |
| 分簇配置 | `MeshSource` 的 `clustering` 字段（`pub`），类型 `ClusterConfig { cell_pixels, z_slices, max_clusters, first_slice_depth, far }` + `ClusterConfig::DEFAULT`；从 prelude 直接可用 | `crates/unlit_wgpu/src/light.rs` / `crates/unlit3d/src/mesh_source/mod.rs` |
| 多边形光 | 少于 3 个角无面积、不画；重复一个角可以拼三角，相差极小的角会被合并 | `crates/unlit3d/src/light.rs` |
| 关闭某盏灯 | `describe.intensity <= 0.0` 的灯在收集时直接跳过（不用 despawn，但 despawn 更省一次遍历） | 同上 |
| 上传 | `upload_lights`（`mesh_source/lighting.rs`）每帧在 `build_scene` 里跑，只要这帧有可见 mesh 就跑，**与画不画受光 mesh 无关**：分簇在 CPU 上重算一遍并整块重传，因为相机与灯都可能动 | `crates/unlit3d/src/mesh_source/mod.rs` |

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
| 帧 | `Frame { delta_time, index, size }` |
| 时钟 | `FrameClock { time, delta_time, index }` + `advance` |
| 派发 | `dispatch_frame`：先 reset `FrameSkip`、推进所有 `FrameClock`，再把 `OnFrame` 按 order 跑一遍（只按 order 排序，实体不参与比较），最后 `world.apply()`——**apply 失败只 `log::error!`，不返回错误** |
| 行为 | `OnFrame::new(f)` 不带顺序、`OnFrame::with_order(order, f)` 带顺序。回调是 `FnMut(&World, Entity, &mut Frame) + 'static`——**是 `&mut Frame`，不是 `&Frame`** |
| 顺序 | `FrameBehaviourOrder(pub i32)`，低者先跑，**同序无序**；dispatch 期间不得增删行为 |
| 跳过 | `FrameSkip`（放在兄弟实体上，由声明了顺序的行为去读） |

## 输入

全在 `crates/unlit3d/src/input/`。

| 项 | 是什么 |
| --- | --- |
| 写端 | `InputWriter::new` + `push(event)` |
| 订阅组件 | `InputFeed(InputWriter)`，spawn 进世界才收事件 |
| 派发 | `dispatch_input(&World) -> bool`：取一个 batch、折进 `InputState`、跑行为；不 apply 结构变更 |
| 读状态 | `InputEvents::with_state` / `with_batch` / `refresh` / `copy_events_into` |
| 行为 | `OnInput`（catch-all）/ `OnKey` / `OnPointer` / `OnMouse` / `OnTouch` / `OnText` / `OnIme` |
| 分发 | `subscribe_input(&mut World)`（向各 world 的缓冲登记订阅）、`distribute_input(&World)`（把已到的事件发给行为组件） |

## UI

| 项 | 是什么 |
| --- | --- |
| source | `UiSource::new()`、`publish_target(Snapshot<UiTarget>)`、`context()` / `context_mut()` |
| 面板 | `UiPanel::new(f)`，`f` 是 `FnMut(Scope<'_>, Entity, &mut egui::Ui)`——**第一个参数是 `Scope`，不是 `&World`** |
| 跨 world | `ExternalFrame` / `ExternalFrames::new / publish / with_frames`、`UiTarget { screen, max_texture_side }`、`lay_out`、`raw_input`。`publish` 会把被顶掉帧的纹理增量 `append` 给继任者——egui 只在写出纹理的那一帧带增量，丢掉它就没材质 |

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
| 自带线程 | `spawn_threaded_world` / `..._with_init`；tick 返回 bool 表示继续 | 同上 |
| 定步长 | `spawn_stepped_world_with_init`、`step_stepped_world`、`with_stepped_world` | 同上 |
| 节拍 | `ThreadControl::self_driven` / `wait_timeout`、`FixedClock::new` / `set_step` / `wait`、`fixed_steps` 与 `MAX_CATCH_UP_STEPS = 4` | `crates/unlit3dx/src/thread.rs` |
| 子 world | `spawn_sub_world`、`drain_sub_world` | `crates/unlit3dx/src/host.rs` |
| 句柄 | `WorldHandle { entity, sender, state }` + `new` / `is_finished` / `stop` | `crates/unlit3dx/src/handle.rs` |
| 名字与状态 | `WorldName(&'static str)`；`Threaded` / `Stepped` 标记；`WorldState` = `Running` / `Stopped` / `Panicked(Arc<str>)`——**带 panic 消息，`match` 时要绑定** | `crates/unlit3dx/src/world.rs`、`thread.rs` |

| 通信 | 是什么 | 语义 |
| --- | --- | --- |
| job | `type Job = Box<dyn FnOnce(&mut World) + Send>`；`JobSender::send` | 有界队列，`Full` / `Closed`，永不阻塞；世界每次 tick 前 drain |
| 状态 | `Snapshot<T>`：`publish` / `reader` / `has_readers`；`SnapshotReader::latest` | 单槽，订阅前发布的值也留着 |
| 事件 | `Stream<T>`：`subscribe` / `publish` / `publish_all` | 每个 reader 自带队列、按序、reader drop 即退订 |
| 应答 | `Pending::new` / `request` / `advance` | `advance` 要吃 `(delta_seconds, &mut Vec<(K, Settled<T>)>)`，**别漏第一个参数**；一次请求一次应答，`Settled::Answered` / `Expired` |
| 发布 | `publish_collected(world) -> usize` | 世界里收集到的快照/流在 tick 之后统一发布 |

## 展示路径（unlit3dx）

| 项 | 是什么 | 搜 |
| --- | --- | --- |
| GPU | `Gpu` 就是 `unlit_wgpu::context::GpuContext`：`GpuContext::headless(tier, power_preference, label)` 自建 instance，`GpuContext::present_to(instance, window, tier, power_preference, label)` 用窗口 surface 探测适配器后丢弃它；`label` 是设备在校验信息里的名字，两者都 async 返回 `Result<GpuContext, GpuRequestError>`。`request_gpu(app, ready)` 返回 `OnFrame`，把请求驱动成帧任务并在就绪回调里交给调用者 | `crates/unlit_wgpu/src/context.rs`、`crates/unlit3dx/src/gpu.rs` |
| 常量 | `SCENE_PASS_ORDER = FramePassOrder(0)`、`BLIT_PASS_ORDER = FramePassOrder(1)`、`PRESENT_ORDER = 10` | `crates/unlit3d/src/winit/presenter.rs` |
| 两条路径 | `PresentationPath`：`Surface { samples, with_depth }` 直画交换链；`Intermediate(TargetConfig)` 画进常驻纹理再加一个 blit pass 贴回交换链。默认 `Surface { samples: OFFSCREEN_SAMPLES, with_depth: true }` | 同上 |
| 装配（库） | `build_presenter(world, render, renderer, scene_pass, surface, instance, adapter, fallback_size, path)` → `Presenter`；`setup_presenter(..)` 同参再 spawn 成组件并返回 `Entity`。`surface` 由调用方 `instance.create_surface(window)` 造 | 同上 |
| 装配（dx） | `setup_presenter(world, render, renderer, scene_pass, gpu, fallback_size, offscreen: bool)`，内部造 surface 并映射成 `Intermediate(TargetConfig::default())` / `Surface`，返回 `Result<(), PresenterError>`——**是 enum，不是 `String`** | `crates/unlit3dx/src/present.rs` |
| 组件方法 | `Presenter::present(host)` / `resize(host, size)` / `release(host)`；三者都只借 `&mut self`，所以宿主可以直接把 `Presenter` 挂成组件、暂停时 `release` 而不必拆掉实体，恢复后继续 `present` 会自动重开交换链；字段**全私有**，只能经由 `build_presenter` / `setup_presenter` 造 | `crates/unlit3d/src/winit/presenter.rs` |
| 每帧行为（dx） | `draw_presented(render)`——它自己 drain sub-world + 分发帧再调 `Presenter::present`；`pump_presented()` 返回 `(OnAboutToWait, OnWindowEvent)` 两个要分别 spawn（内部就是 `pump_presented_with(Some(LocalTasks::pump))`）；`resize_presented` | 同上 / `crates/unlit3dx/src/present.rs` |
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
| 音频 | `spawn_audio_world_with::<R>`、`dispatch_audio_fill`、`OnAudioFill::new(order, f)`（**第一个参数是 `AudioFillOrder`**）、`AudioFillOrder`、`GlobalAudioGain`、`AudioConfig` | `crates/unlit3dx/src/audio.rs` |
| 合成 | `load_soundfont`、`load_midi_file_with_loop_type(bytes, loop_type)`（**没有无 loop_type 的 `load_midi_file`**；手搓事件序列用 `midi_file`）、`SynthVoice::from_font`、`MidiSequencer::new` / `advance` / `rewind`、`midi_job` | `crates/unlit3dx/src/synth.rs` |

## glTF

`LitGltf`（`crates/unlit3d/src/gltf/`，入口 `mod.rs`）：`load` / `from_bytes`、`pipeline_key`、`insert_resources`、`spawn_default_scene`、`apply_animation` / `animation_duration` / `animation_name`、`skin_pose` / `morph_weights`、`insert_image` / `insert_material` / `insert_mesh`。

## 常用常量

| 项 | 值 |
| --- | --- |
| 示例格式 | 定义在 `unlit3d_examples/src/scenes/mod.rs`，是示例侧常量、不是库 API：`TEST_FORMAT = Rgba8UnormSrgb`、`TEST_SIZE = (256, 192)`、`BASELINE_SIZE = (960, 720)`、`SEQUENCE_STEP = 0.5` |
| 相机基线 | 眼 `(0.0, 1.2, 3.2)`、目标 `(0.0, 0.2, 0.0)`、fov 60°、near 0.1 |
