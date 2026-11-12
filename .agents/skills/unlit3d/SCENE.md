# 写单 world 场景（unlit3d）

签名见 [REFERENCE.md](REFERENCE.md)；多 world 见 [MULTI_WORLD.md](MULTI_WORLD.md)；画不出来见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。

## 最小骨架

**可直接抄结构**；优先抄 `crates/unlit3d/README.md` 的「Example」四步 doctest——它由 `cargo xtask test` 每趟编译，是唯一保证能编译的骨架，示例里的签名也以它为准。完整可跑实现在 `unlit3d_cli/src/lib.rs` 的 `build_world` / `offscreen_world`。

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::LitOptions;
use unlit_wgpu::resources::ResourceGraph;

// 一 world 一份：GPU 状态（device/queue/graph/capabilities）与 FrameClock 一起进 shared 子树。
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new(), DeviceCapabilities::default());

// 内建 lit family 只登记策略，不立刻编译 pipeline：首个 draw 才建。
let mut mesh_source = MeshSource::new(&world.root_scope(), ctx);
mesh_source.register_lit_family(&world.root_scope());
let key = LitPipelineKey::new(LitOptions::standard());
let renderer = world.root_scope_mut().spawn(bundle![Renderer::new(ctx)]);

// pass 是作用域：它录制挂在自己下面的 source。
let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
let source = world.scope_mut(pass).unwrap().spawn(bundle![Source::new(mesh_source)]);

// 相机与渲染实体都挂在 source 自己的子树里。
world.scope_mut(source).unwrap().spawn(bundle![camera]);
world.scope_mut(source).unwrap()
    .spawn(bundle![Transform::default(), mesh, material, GpuRenderPipeline::new(key)]);

// 每帧：先给 pass 挂附件，再让 renderer 录一次。
let attachments = FrameAttachments::new(&world, ctx, format, size, 1, true);
world.root_scope().query_one::<&mut FramePass>(pass).unwrap()
    .set_attachments(&attachments, color_view);
world.root_scope().query_one::<&mut Renderer>(renderer).unwrap().render(&world);
```

想直接出图：颜色附件用 `create_color_target` 建，渲染完用 `unlit_wgpu::readback::readback_texture(device, queue, &texture)` 取像素（按纹理格式返回该格式本身的字节，行对齐 padding 已剥掉）。

## 状态怎么放

两条底线，单 world 与多 world 都适用：**状态尽量进 ECS，跨线程 / 跨 world 通信只用内置机制**（后者见 [MULTI_WORLD.md](MULTI_WORLD.md)）。

- **bundle 是对象，组件是数据**。一个「对象」= 数据组件 + 它的 tick（行为组件），一个 bundle 一次 spawn：加对象就是 spawn 实体，删对象就是 despawn。不要另立一份 `Vec<MyObject>` 或 `GameState` 大结构体再手动与 world 同步——实体就是对象，组件就是它的字段，query 就是遍历。
- 组件是数据；行为放行为组件（`OnFrame` / `OnInput` / `OnAudioFill`）或由调用方驱动。unlit_ecs 没有 systems 也没有 resources：要全局可达的状态就 spawn 一个实体（放 `world.shared()`），caller 手里留一个 `Entity` 句柄。
- 按数据归属拆组件，不做一个塞下一切的巨大组件，也不用一个大 enum 状态机统筹流程：一个实体的不同侧面各是一个组件；跨事件的标志用兄弟组件（回调里取不到自己这个组件类型），「这一帧跳过」就是一例。
- scope 按「谁看得到」拆：pass / source 是 scope，UI source 与 mesh source 互相看不见，两边都要读写的状态放 `world.shared()`；实体的组件集合在 spawn 时固定，要换集合就 despawn 重 spawn，不要造一个 `Option<Everything>` 组件模拟可变结构。

## 加几何体

| 想做     | 做法                                                                                                                                                                 |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 建 mesh  | `source.allocate_lit_mesh(&scope, &key.options, LitMeshDesc { .. })` 返回 `GpuMesh`；顶点通道由你给的 slice 决定（positions 必填，uvs/colors/normals/indices/joints/weights/morph 可选；`expanded` 外扩壳需要 normals） |
| 建材质   | 纹理 `register_texture_and_default_view`、采样器 `register_sampler`，再 `allocate_lit_material(&scope, &key.options, view, sampler)`                                          |
| 画一份   | `spawn(bundle![Transform, mesh, material, GpuRenderPipeline::new(key)])`，挂在 source 子树里                                                                           |
| 画多份   | 同一个 `key` / `mesh` / `material` clone 到多个实体；每份差异用 `InstanceColor` / `InstanceCutoff` / `Transform`，**不要**为每个颜色重建 mesh                          |
| 暂时隐藏 | `InstanceVisibility { visible: false }`，不要 despawn                                                                                                                     |
| 释放     | `source.remove_mesh(mesh)`                                                                                                                                               |

每实例组件都是**具名字段结构体**：`InstanceColor { color }` / `InstanceCutoff { cutoff }` / `InstanceHullWidth { width }` / `InstanceVisibility { visible }`；前三个有 `new(..)`，四个都有 `Default`（`InstanceVisibility` 没有 `new`，用 `InstanceVisibility { visible: false }`）。写成 `InstanceColor(Vec4::ONE)` 编不过。

不假设顶点布局：通用版 `allocate_mesh` 接受你自定的 `MeshDesc`。`LitOptions` 是**纯策略**（剔除、深度测试与写入、混合、写掩码、着色开关）；顶点通道有无、是否带材质、是否 morph，是 family 在解析 draw 时自己折叠进 `LitVariant` 的事实。颜色格式、采样数、有没有深度附件由这一帧的 `SurfaceKey` 决定，**不**写在 `LitOptions` 里。

## 管线与渲染目标

- `LitOptions::standard()` 无参：背面剔除、过滤型 base-color 绑定、reverse-z（`DepthStencilPolicy::default()` 即写深度 + `Greater`）、不混合。它**不**指定颜色格式、采样数或深度格式——这三样属于目标：`Renderer` 从 pass 的附件算出 `SurfaceKey`，经 `PassInfo::surface` 交给每个 source（`crates/unlit3d/src/renderer.rs` 的 `pass_info`）。内建 lit family 把 `draw.surface` 折进 `LitVariant` 作为特化键，换单采样目标或别的颜色格式都不用改 `LitOptions`。**自己实现 family 时要在管线描述符上按这个 surface 调 `set_surface`**（`SurfaceTarget`，`crates/unlit_wgpu/src/specialize.rs`），内建 blit family 就是这么做的（`crates/unlit3d/src/blit.rs`）；漏了就是 pipeline 创建失败。
- 深度是 **reverse-z**：`depth_compare: Greater`、清成 0.0（`depth_clear()`）、投影用 `glam::camera::rh::proj::directx::perspective_infinite_reverse`。配 forward-z 投影会整片被裁掉。
- 要自定义管线就 `register_family::<K, F, I>`：内建 lit 没有特权路径，它只是其中一个 family。同一个 world 里同一 key 类型只能注册一次（重复会 panic：`a pipeline family is already registered for this key type`）。
- 描边：`LitOptions::inverted_hull()` 是配套的壳策略（沿法线外扩 + 剔正面 + 不写深度 + 无光）。同一 mesh 画两遍，壳实体带 `InstanceHullWidth { width }`，mesh 要给 `LitMeshDesc::normals`；两个 key 落进同一个 family，不需要第二个 mesh 或 pass。

## 光照

`standard()` 的 `unlit` 是 `false`，即**受光**；但示例的 `default_options()` 与 `LitGltf::pipeline_key` 都显式设成 `unlit: true`。所以「照抄示例骨架却永远一块纯色」不是渲染坏了——是那个骨架根本没开光。想看明暗，做三件事：

1. key 来自 `options.unlit = false` 的 `LitOptions`（在 `standard()` 上改一处即可）；
2. 灯是 **`LightSource` 组件**（`LightSource::new(DirectionalLight { .. })` 等），spawn 在**画这个 mesh 的那个 source** 的子树里——和相机、mesh 完全同一条规则，挂错地方就是没有光；
3. 灯沿它自己 `Transform` 的 **`-Z`** 照。方向光没有位置、只有朝向，聚光灯的锥也由朝向定：瞄准就是**旋转实体**，不是填一个 direction 字段。没有 `Transform` 的灯坐在原点朝 `-Z`。

- 四种灯 `DirectionalLight` / `PointLight` / `SpotLight` / `QuadLight`（矩形光用 `QuadLight::rect(..)`，任意多边形填 `vertices`）。`intensity` **无量纲**：片元就是 base color × Σ(color × intensity)，没有曝光也没有能量守恒，数字凭观感调。
- 环境光 `AmbientLight { color }`，没有 intensity，一帧把所有环境光**相加**。
- 上限与分簇：方向光只有个位数名额（超了按 importance 丢最弱的），每簇能引用的灯数、quad 光的最大角数都有常量；调 `MeshSource` 的 `clustering` 字段（`ClusterConfig`）改分簇粒度。常量与字段见 [REFERENCE.md](REFERENCE.md)。
- 光源上传每帧跑一次遍历，门槛在调用点之前：**这帧有活动相机、且有可见 mesh** 才跑，与画不画受光 mesh 无关。
- 可抄的完整例子：`unlit3d_examples/src/scenes/lit.rs`（共 8 盏：2 方向 + 3 点 + 1 聚光 + 1 矩形 + 1 三角面光）与 `lit_clusters.rs`（改分簇参数）。示例把「灯 + Transform」包成 `PlacedLight`（`scenes/mod.rs`），`PlacedLight::aiming` 就是「转到 `-Z` 指向目标」的写法——**那是示例 bundle，不是库 API**，自己写就一个 `bundle![Transform, LightSource::new(..)]`。

## 透明与排序

透明物件单开一个 key，在 `standard()` 基础上改三处：`options.blend = Some(wgpu::BlendState { .. })`（straight alpha：`SrcAlpha` / `OneMinusSrcAlpha` / `Add`）、`options.depth_stencil.depth_write_enabled = false`（深度**测试**仍开，否则前面的片子会挡住后面的）、`options.primitive.cull_mode = Some(wgpu::Face::Back)`（薄板只画一层，避免叠两次混合），并给实例加 `ZSortedDrawing`——带它的在不带它的之后画，按视轴深度从后往前。可抄的写法见 `unlit3d_examples/src/scenes/transparent.rs`。

## 相机与 viewport

- 相机是 `Camera { view_from_world, clip_from_view, active }`，自己构造：`glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y)` + `perspective_infinite_reverse(fov_y_radians, aspect, 0.1)`。
- **每帧按 `frame.size` 重建** aspect，不要在构建时捕获尺寸，窗口拉宽时才不拉伸。
- 多相机靠 `active` 切换，不要 respawn。
- letterbox：把 `ViewportRect` 设在**场景自己的 pass** 上（`ViewportRect::new(w, h).at(x, y)` 或 `ViewportRect::fit_aspect(aspect, w, h)`），相机按内容尺寸的宽高比建，窗口变大只加黑边。

## UI

**可直接抄结构**：

```rust
let ui = world.root_scope_mut().spawn(bundle![Source::new(UiSource::new())]);
world.scope_mut(ui).unwrap().spawn(bundle![UiPanel::new(|_scope, _entity, ui| {
    ui.label("hello");
})]);
```

- `UiPanel` 的回调第一个参数是 `Scope<'_>`，不是 `&World`。
- 面板必须挂在 `UiSource` 的子树里；UI source 与 mesh source 各自的作用域互相看不见，两边都要读写状态就放 `world.shared()`。
- UI source 要在**场景构建之前**挂到 pass 下，场景自己的面板才落在它画的 scope 里。
- egui 窗口显式给 id（`egui::Window::new("x").id(egui::Id::new("x"))`），否则 id 由标题派生，别处按 id 找 rect 会找不到。
- 想做可复现截图，把 egui style 的 `animation_time` 设 0（`context_mut().all_styles_mut(|style| style.animation_time = 0.0)`；它是 style 的字段，不是 `Context` 的），否则首帧的淡入取决于取帧时刻。

## 输入

输入两步，节奏由调用方拥有：`InputWriter::push(event)` 投递到每个订阅 world 的缓冲；`dispatch_input(&world)` 开一次 tick（取 batch → 折进本 world 的 `InputState` → 跑行为组件），不 apply 结构变更。

下例的 `Presses` 是你自己定义的状态组件（`struct Presses(u32);`），不是库类型——跨事件状态就放这种兄弟组件里。**可直接抄结构**：

```rust
let input = InputWriter::new();
world.root_scope_mut().spawn(InputFeed(input.clone()));
world.root_scope_mut().spawn(bundle![Presses(0), OnKey::new(|world, entity, key| { /* ... */ })]);
input.push(InputEvent::Key(KeyEvent { key: Key::W, pressed: true, repeat: false, modifiers: Modifiers::default() }));
assert!(dispatch_input(&world));
world.apply().expect("the queue is drained");
```

- 回调里不能取自己这个组件类型（运行时已借用 → panic），跨事件状态放**兄弟组件**。
- 结构变更走 `Scope::queue`，`World::apply` 落地；回调只拿 `&World`。
- 指针优先用 `OnPointer`（触摸也走它），只在需要悬停语义时用 `OnMouse`。
- 面板要先看 `UiSource` 的 `context().egui_wants_pointer_input()` 再决定是否吃事件，否则拖窗口会同时拖场景。

## 帧行为与顺序

- `OnFrame::new` 不带顺序，所有不带顺序的构成一个 band，**band 内无序**；依赖别人的读或写，就用 `OnFrame::with_order(FrameBehaviourOrder(n))` 两端都声明。
- 库只给**它自己装配的帧**定顺序：presenter 侧 `PRESENT_ORDER = 10`（默认 band 之后，好让喂帧的行为都跑完）、unlit3dx 内部 `REQUEST_GPU_ORDER = -1`（默认 band 之前，GPU 先就绪），pass 侧另有两个 `FramePassOrder` 常量 `SCENE_PASS_ORDER` / `BLIT_PASS_ORDER`。其余顺序都是应用的约定，示例的排法是：默认 band 建资产（不带顺序），玩法 tick 在它**之后**（`BALLS_ORDER = 100`，因为 tick 读的就是建好的资产），发布面板最后（`PUBLISH_ORDER = 200`）。
- 「这一帧跳过」用兄弟组件做标志，让声明了顺序的行为去读，不要在回调里删行为（dispatch 期间增删行为是未定义顺序）。
