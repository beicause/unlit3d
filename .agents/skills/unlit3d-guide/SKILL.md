---
name: unlit3d-guide
description: 用 unlit3d（单 world 渲染库）与 unlit3dx（多 world 运行时）写场景：搭 world、挂 source、加几何体与材质、接 UI 与输入，以及把渲染、物理、音频拆到各自的 world。当需要新增场景、新增示例、扩展 unlit3d/unlit3dx，或照现有示例的骨架起一份新程序时使用。
whenToUse: 当要动手写 unlit3d 或 unlit3dx 的代码（新建 world、画东西、加面板、接输入、拆多 world），或要判断某段渲染代码该放哪一层、某个现象为什么画不出来时。
---

# 用 unlit3d 与 unlit3dx 写场景

两层选一层。`unlit3dx` 不自带渲染器：它的 render bridge 建在 `unlit3d` 的 `MeshSource` 上（参考实现是 `unlit3dx_examples/src/render.rs`），所以下面单 world 的规则在多 world 里一条都不改。

| 层 | 什么时候用 | 入口 |
| --- | --- | --- |
| `unlit3d` | 单 world 就够：离线出图、快照测试、嵌进别人的宿主程序 | `spawn_context` + `MeshSource` + `Renderer` |
| `unlit3dx` | 渲染/物理/音频/玩法各自成 world，靠 job 与 snapshot 相连 | `spawn_sub_world` / `spawn_threaded_world` + `request_gpu` + `setup_presenter` |

API 速查见同目录的 [REFERENCE.md](REFERENCE.md)；可跑的例子见本文末尾的「示例索引」。

## 心智模型

1. **world 是一棵树**，不是一堆组件。`root` → `shared` → 各 scope 的子树；每个 scope 只看得到自己的子树加 `shared` 子树，看不见兄弟。
2. **source 画它自己子树里的东西**。`Renderer::render` 遍历每个 pass，对 pass 下面的每个 `Source` 用 `world.scope(source_entity)` 取作用域，只有挂在该 scope 下的实体才被画。相机同理——相机是为「持有它的那个 source」取景的。
3. **pass 是一个 scope，source 又是一个 scope**。pass 录制它下面的 source，按 `FramePassOrder` 排序；pass 内按 `FrameSourceOrder` 排序。挂在 pass 下、与 source 平级的渲染实体不会被画。
4. **一帧两个阶段**：先让每个 pass 的每个 source 调 `build_scene` 填 `Scene`（上传也走这里给的同一个 encoder），再逐个 pass 录制提交。所以上传和读它的 draw 在同一次 submission 里。
5. **`Renderer::render(&world)` 是唯一的驱动点**。它只录帧、不推进世界；`Frame`（`delta_time` / `index` / `size`）由调用方构造，用 `dispatch_frame` 发给 `OnFrame` 行为组件。
6. **资源句柄是组件**。`GpuMesh` / `GpuMaterial` 是带 `Rc` 的句柄，clone 就是拷贝句柄；想再画一份就 clone 出来再 spawn 一个实例实体，不要重新创建 GPU 资源。

## 单 world 骨架（unlit3d）

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::resources::ResourceGraph;

// 一 world 一份：GPU 状态（device/queue/graph/capabilities）与 FrameClock 一起进 shared 子树。
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new(), DeviceCapabilities::default());

// 内建 unlit family 只登记策略，不立刻编译 pipeline：首个 draw 才建。
let mut mesh_source = MeshSource::new(&world.root_scope(), ctx);
mesh_source.register_unlit_family(&world.root_scope());
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world.root_scope())));
let renderer = world.root_scope_mut().spawn(bundle![Renderer::new(ctx)]);

// pass 是作用域：它录制挂在自己下面的 source。
let pass = world.root_scope_mut().spawn(Pass::new(FramePassOrder(0)));
let source = world.scope_mut(pass).unwrap().spawn(bundle![Source::new(mesh_source)]);

// 相机与渲染实体都挂在 source 自己的子树里。
world.scope_mut(source).unwrap().spawn(bundle![camera]);
world.scope_mut(source).unwrap()
    .spawn(bundle![Transform::default(), mesh, material, UnlitPipeline::new(key)]);

// 每帧：先给 pass 挂附件，再让 renderer 录一次。
let attachments = FrameAttachments::new(&world, ctx, format, size, 1, true);
world.root_scope().query_one::<&mut FramePass>(pass).unwrap()
    .set_attachments(&attachments, color_view);
world.root_scope().query_one::<&mut Renderer>(renderer).unwrap().render(&world);
```

想直接出图：颜色附件用 `create_color_target` 建，渲染完用 `unlit_wgpu::readback::readback_texture(device, queue, &texture)` 取像素（按纹理格式返回该格式本身的字节，行对齐 padding 已剥掉）。

完整可跑的单 world 实现在 `unlit3d_cli/src/lib.rs` 的 `build_world` / `offscreen_world`。

## 多 world 骨架（unlit3dx）

窗口版——GPU 就绪前先把渲染 world 空着建出来，玩法从第一帧就握着它的 handle。下面的 `setup_render_world` / `mount_scene_content` / `SceneStepper` 是本仓库 `unlit3dx_examples` 里的私有装配函数（`unlit3dx_examples/src/scene/mod.rs`），库只提供 `spawn_context` / `Pass` / `Renderer` / `setup_presenter` 那一层，照抄时要把它们换掉：

```rust
let event_loop = EventLoop::new()?;
let mut host = WinitHost::new();
let world = host.world_mut();

world.root_scope_mut().spawn(WindowSetup { attributes: Window::default_attributes().with_title("app") });
world.root_scope_mut().spawn(ExitOn(ExitTrigger::CloseRequested));

let render = spawn_sub_world(world, "render", 64);
let app = world.root_scope_mut().spawn(bundle![ExitRequest(false)]);
world.root_scope_mut().spawn(bundle![request_gpu(app, move |world, gpu| {
    // 回调拿 &mut World，在两次事件回调之间执行：正好一次建完渲染 world 与展示路径。
    let (renderer, pass) = setup_render_world(world, render.entity, &gpu.device, &gpu.queue, gpu.capabilities);
    mount_scene_content(&render, pass, panels);   // 场景内容一律走 job
    setup_presenter(world, render.entity, renderer, pass, gpu, FALLBACK_SIZE, false)
        .expect("the presenter is built");
})]);

world.root_scope_mut().spawn(bundle![draw_presented()]);
let (pump, redraw) = pump_presented();
world.root_scope_mut().spawn(bundle![pump]);
world.root_scope_mut().spawn(bundle![redraw]);
world.root_scope_mut().spawn(bundle![resize_presented()]);
host.run(event_loop)?;
```

无窗口 / 定步长（测试、离屏、wasm）——进程自己驱动每一帧：

```rust
let step = 1.0 / HEADLESS_HZ;                                 // 每帧时长（秒）
let mut frame = Frame { delta_time: step as f32, index: 0, size };
for index in 0..frames {
    frame.index = index;
    stepper.step(&host, &worlds, step);                       // 玩法 → 物理 → 音频
    let mut nested = host.root_scope().query_one::<&mut World>(render.entity).unwrap();
    drain_sub_world(&mut nested);                             // render world 的 job 在宿主线程跑
    dispatch_frame(&mut nested, &mut frame);
    nested.root_scope().query_one::<&mut Renderer>(renderer).unwrap().render(&nested);
    let pixels = readback_texture(device, queue, &offscreen.texture);
}
```

## 加几何体

| 想做 | 做法 |
| --- | --- |
| 建 mesh | `source.allocate_unlit_mesh(&scope, &key, UnlitMeshDesc { .. })` 返回 `GpuMesh`；顶点通道由你给的 slice 决定（positions 必填，uvs/colors/indices/joints/weights/morph 可选） |
| 建材质 | 纹理 `register_texture_and_default_view`、采样器 `register_sampler`，再 `allocate_unlit_material(&scope, &key, view, sampler)` |
| 画一份 | `spawn(bundle![Transform, mesh, material, UnlitPipeline::new(key)])`，挂在 source 子树里 |
| 画多份 | 同一个 `key` / `mesh` / `material` clone 到多个实体；每份差异用 `InstanceColor` / `InstanceCutoff` / `Transform`，**不要**为每个颜色重建 mesh |
| 暂时隐藏 | `InstanceVisibility { visible: false }`，不要 despawn |
| 释放 | `source.remove_mesh(mesh)` |

不假设顶点布局：通用版 `allocate_mesh` 接受你自定的 `MeshDesc`。`UnlitOptions` 是**纯策略**（剔除、深度、混合、采样、目标格式）；顶点通道有无、是否带材质、是否 morph，是 family 在解析 draw 时自己折叠进 `UnlitVariant` 的事实。

## 管线与渲染目标

- `UnlitOptions::standard(device)` 面向「4x MSAA + 设备默认深度格式 + `Rgba8UnormSrgb` + reverse-z」。深度格式**不要写死**：它与 `create_depth_target` 一样取 `unlit_wgpu::render_attachments::default_depth_stencil_format(device)`——设备支持 `DEPTH32FLOAT_STENCIL8` 时是 `Depth32FloatStencil8`，否则才是 `Depth24PlusStencil8`。换成单采样目标或别的颜色格式时**不必**自己改这三个字段：内建 unlit family 在创建管线时按这一帧附件的 surface（`attachments.surface_key()`）重写它们（`SurfaceTarget::set_surface`，`crates/unlit_wgpu/src/pipeline.rs:888`），`UnlitOptions` 里写的是占位。只有自己实现的 family 才要负责这件事——在管线描述符上同样调 `set_surface`，内建的 blit family 就是这么做的（`crates/unlit3d/src/blit.rs:337`）。
- 深度是 **reverse-z**：`depth_compare: Greater`、清成 0.0（`depth_clear()`）、投影用 `glam::camera::rh::proj::directx::perspective_infinite_reverse`。配 forward-z 投影会整片被裁掉。
- 要自定义管线就 `register_family::<K, F, I>`：内建 unlit 没有特权路径，它只是其中一个 family。同一个 world 里同一 key 类型只能注册一次（重复会 panic: `a pipeline family is already registered for this key type`）。

## 透明与排序

透明物件单开一个 key：在 `standard` 基础上加 `color_target.blend`（straight alpha：`SrcAlpha` / `OneMinusSrcAlpha` / `Add`），`depth_stencil.depth_write_enabled = Some(false)`（深度**测试**仍开，否则前面的片子会挡住后面的），`cull_mode = Back`（薄板两面剔除，避免叠两层混合），并给实例加 `ZSortedDrawing`——带它的在不带它的之后画，按视轴深度从后往前。

## 相机与 viewport

- 相机是 `Camera { view_from_world, clip_from_view, active }`，自己构造：`glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y)` + `perspective_infinite_reverse(fov_y_radians, aspect, 0.1)`。
- **每帧按 `frame.size` 重建** aspect，不要在构建时捕获尺寸，窗口拉宽时才不拉伸。
- 多相机靠 `active` 切换，不要 respawn。
- letterbox：把 `ViewportRect` 设在**场景自己的 pass** 上（`ViewportRect::new(w, h).at(x, y)` 或 `ViewportRect::fit_aspect(aspect, w, h)`），相机按内容尺寸的宽高比建，窗口变大只加黑边。

## UI

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
- 想做可复现截图，把 context 的 `animation_time` 设 0，否则首帧的淡入取决于取帧时刻。

## 输入

输入两步，节奏由调用方拥有：`InputWriter::push(event)` 投递到每个订阅 world 的缓冲；`dispatch_input(&world)` 开一次 tick（取 batch → 折进本 world 的 `InputState` → 跑行为组件），不 apply 结构变更。

下例的 `Presses` 是你自己定义的状态组件（`struct Presses(u32);`），不是库类型——跨事件状态就放这种兄弟组件里：

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
- 库不定义任何行为顺序常量（帧行为的顺序谁先谁后是应用的约定；只有 pass 侧有 `SCENE_PASS_ORDER` / `BLIT_PASS_ORDER`）。示例的排法是：默认 band 建资产（不带顺序），玩法 tick 在它**之后**（`BALLS_ORDER = 100`，因为 tick 读的就是建好的资产），发布面板最后（`PUBLISH_ORDER = 200`）。
- 一个「对象」= 组件 + 它的 tick，做成一个 bundle 一次 spawn；加对象就是 spawn 实体，删对象就是 despawn。
- 「这一帧跳过」用兄弟组件做标志，让声明了顺序的行为去读，不要在回调里删行为（dispatch 期间增删行为是未定义顺序）。

## 多 world（unlit3dx）

- 三种 runner 协议完全相同：`Threaded`（自带线程）、`Stepped`（宿主线程上 `step_stepped_world` 推）、子 world（`spawn_sub_world` + `drain_sub_world`，也在宿主线程）。同一份 tick 体在三种下都能跑。
- 一条 job 是 `Box<dyn FnOnce(&mut World) + Send>`，**不回答**；有界队列，满了返回 `SendError::Full`、世界已停返回 `Closed`，永不阻塞。
- 拿回数据三种形状：`watch`（最新状态，单槽无积压）、`subscribe`（每个事件，按序）、`Pending`（一次请求一次应答，超时给 `Expired` 而不是等死）。发布发生在世界跑完一次 tick 之后，tick 体自己不发布。
- 世界装配只放配置（device/queue/tick 率/重力/缓冲大小），**游戏内容一律走 job**；setup 里出现「游戏里的某个东西」就说明写错了层。
- 一个 scene 对每个它要说话的世界只留一个 port（`JobSender` + 订阅值），收在一个结构体里递下去。
- 跨 world 的实体用 newtype 包一层（示例的 `RenderEntity(pub Entity)`）：每个 world 的实体编号都从 0 开始，裸 `Entity` 一定会混。
- 展示路径：`setup_presenter(.., offscreen)` 传 `false` 时场景直画交换链；要截图、快照或 MCP 就传 `true`，场景画进持久 target、再由第二个 pass 贴到交换链——纹理才活得过 present，交换链换格式或尺寸也不碰场景 pipeline。
- 离线取帧：`bind_offscreen(&world, ctx, pass, &device, size)` 拿到 `OffscreenTarget { texture, view }`，再配 `readback_texture`。

## 最佳实践（从 examples 提炼）

**作用域与世界**

1. 渲染实体与相机必须 spawn 在**画它的那个 source** 之下；这是「什么都不画」的第一嫌疑。
2. `spawn_context` 每个 world 只调一次；它把 GPU 状态放进 shared 子树，别自己再造一套。
3. 多个 scope 都要读写的状态放 `world.shared()`：UI source 与 mesh source 之间没有共享作用域。
4. 行为回调只拿 `&World`：结构变更 `queue` + `apply`；回调里别再取自己这个组件类型。
5. 每帧工作是一次 `dispatch_frame`；帧间隔与尺寸由调用方塞进 `Frame`，行为里不要自己读时钟，才可复现。
6. 世界装配只放配置，游戏内容走 job。

**几何体与材质**

7. 一个 mesh 多份实例：颜色走 `InstanceColor`、镂空走 `InstanceCutoff`，不要为每个颜色建一个 mesh。
8. 顶点通道由你上传的 slice 决定，不由 `UnlitOptions` 决定；少传一个通道就少打包一份数据。
9. 材质是 bind group 的配方：纹理与采样器先注册进 `ResourceGraph`，只把 `ResHandle` 交给 `allocate_unlit_material`。
10. 高频纹理开各向异性过滤（`anisotropy_clamp: 4` 起），点采样在掠射角会出摩尔纹。

**管线与目标**

11. `UnlitOptions::standard` 假设 4x MSAA + Rgba8UnormSrgb + 设备默认深度格式（`default_depth_stencil_format(device)`，别写死 `Depth24PlusStencil8`）；内建 unlit 的这三个目标字段由附件覆盖，自定义 family 得自己 `set_surface`。
12. reverse-z：`Greater` + 清 0.0 + `perspective_infinite_reverse`，三者缺一画面就空。
13. 透明用**第二个 key**（blend + 不写深度 + 仍测深度 + 背面剔除）+ `ZSortedDrawing`；spawn 顺序故意与绘制顺序不同，才验证得出排序。
14. 要新管线用 `register_family` 自己注册，别去找内建的内部路径（内建 unlit 只是其中一个 family）。

**帧、输入、UI**

15. 顺序显式声明：`FramePassOrder`（pass 间）/ `FrameSourceOrder`（source 间，库给了 `MESH` / `UI`）/ `FrameBehaviourOrder`（行为间，库不定义顺序值）；同序无序。
16. 相机每帧按 `frame.size` 重建 aspect；多相机用 `active` 切，不 respawn。
17. letterbox 把 `ViewportRect` 设在自己的 pass 上，相机按内容宽高比建。
18. UI source 先挂、场景后建；面板与它读写的状态必须在同一棵可达子树里。
19. 可复现截图把 egui 的 `animation_time` 设 0。
20. 指针用 `OnPointer` 覆盖触摸；面板先问 `egui_wants_pointer_input()` 再吃事件。

**多 world**

21. 一个 scene 每个世界一个 port；跨 world 实体用 newtype。
22. job 不回答：要值用 `Pending`，要状态用 `watch`，要事件用 `subscribe`；`send` 的 `Full` / `Closed` 别 unwrap 掉。
23. 对象 = 组件 + tick 一个 bundle；默认 band 无序，依赖就用带 order 的构造器显式声明。
24. 要截图、快照或 MCP 就开 offscreen 展示路径，否则帧活不过 present。

## 常见错误

| 现象 | 原因 |
| --- | --- |
| 什么都不画 | 渲染实体或相机不在 source 的子树里；`Renderer` 只画 source 自己的子树 |
| pass 完全不画 | 没 `set_attachments`：`FramePass` 在拿到颜色附件之前不画任何东西 |
| 整片被裁掉 / 深度反了 | forward-z 投影配 `Greater`，或 reverse-z 投影配 `Less` |
| pipeline 创建失败 | 自定义 family 没在自己的管线描述符上 `set_surface`，`multisample.count` / `color_target.format` / `depth_stencil.format` 与附件不一致（内建 unlit 已自动覆盖） |
| panic: a pipeline family is already registered for this key type | 同一个 world 里 `register_unlit_family` 调了两次 |
| `allocate_unlit_mesh` panic | slice 为空、通道长度不一致、joints/weights 只给一边、morph 数量不对 |
| 借用 panic | 行为回调里取自己这个组件类型；或同一实体同时 `query_one` 两次 |
| 结构变更没生效 | 回调里 spawn/despawn 没走 `queue`，或没调 `apply` |
| 透明面发黑 / 错位 | 没开 blend、`depth_write_enabled` 没关，或没加 `ZSortedDrawing` |
| 画面拉伸 | viewport 的宽高比与相机的不一致 |
| 面板看不见 | `UiPanel` 不在 `UiSource` 子树里，或 UI source 挂在场景构建之后 |
| 面板有 draw 但一个材质都没有 | 发布方比绘制方快，携带纹理增量的那一帧被下一帧顶掉了。`ExternalFrames::publish` 会把被顶掉帧的 `textures` 交给继任者；自己绕过它换帧就会丢图集 |
| job 丢了 | `send` 的 `Full` / `Closed` 被忽略 |
| snapshot 一直是 None | 目标 world 还没跑完一次 tick（发布在 tick 之后），或 reader 已 drop（drop 即退订） |
| 集成测试没跑 | 本仓库用 `cargo nextest run`；带 required-features 的套件要 `--all-features` 或点名 feature |

## 跑起来、调起来

| 目的 | 命令 |
| --- | --- |
| 单 world 的窗口示例 | `cargo run -p unlit3d_examples --bin unlit3d-examples -- --scene spin_cube`（`--list-scenes` 看全部，`--mcp` 开调试宿主） |
| 多 world 的示例 | `cargo run -p unlit3dx_examples --bin unlit3dx-examples`（另有 `--mcp` / `--stepped` / `--headless`；`--headless --mcp` 是无窗口、按帧驱动的 MCP 宿主） |
| glTF 出图 | `cargo run -p unlit3d_cli --bin unlit3d-cli` |
| 整个工作区测试 | `cargo xtask test`（内部是 `cargo nextest run` 加 `cargo test --doc`） |
| 单 crate | `cargo nextest run -p unlit3d --all-features` |
| 重存快照 | `SNAPSHOT_UPDATE=1 cargo nextest run -p unlit3d_examples`，再审快照的 diff |
| 多 world 的端到端测试 | `cargo nextest run -p unlit3dx_examples`：离屏渲染帧、混音，以及起真实进程走 MCP 的四步交互（三张图 + 一段音频） |
| 校验与格式 | 先 `cargo clippy` 再 `cargo fmt` |

快照不匹配的帧写在 `target/snapshot-mismatches`（webgl2 档 `target/snapshot-mismatches-webgl2`，wasm 档 `target/snapshot-mismatches-wasm`；单独跑时由 `UNLIT3D_SNAPSHOT_MISMATCH_DIR` 指定）。

调试**运行中**的示例用 `unlit3d-mcp` 技能（连 `--mcp` 宿主，读写实体、看资源图、抓帧截图），不要用截屏软件。

## 示例索引

| 场景 | 演示什么 |
| --- | --- |
| `spin_cube` | 纹理立方体 + 两块 egui 面板 + 键盘与拖拽；CI 的快照就是它 |
| `ui_only` | 没有 mesh source、没有相机也能出一帧 UI |
| `mesh_and_ui` | mesh source 与 UI source 同帧的记录顺序、scissor、load ops |
| `transparent` | 透明 + `ZSortedDrawing` + blend |
| `animated` | 八帧内 mesh 的分配、回收、平移旋转 |
| `skinned` / `morphed` / `instanced_skinned_morph` | 蒙皮、morph，以及共享一个 mesh 的实例合并 |
| `gltf` | `UnlitGltf` 加载、动画采样、`spawn_default_scene` |
| `mesh_topologies` | 五种 primitive topology，索引与非索引各一遍 |
| `custom_pipeline` | 自己注册 family（程序化天空 + 粒子） |
| `unlit3dx_examples`（另一个二进制，不是场景 id） | 多 world：render / physics / audio / gameplay 分工，跨 world UI，MIDI |

## 在源码里的位置

- 单 world 骨架与四步示例：`crates/unlit3d/README.md` 的「Example」一节（doctest，可直接抄）。
- frame 模型与 source/pass 语义：`crates/unlit3d/src/source.rs`、`crates/unlit3d/src/pass.rs`、`crates/unlit3d/src/renderer.rs`。
- 内建 unlit family 与实例组件：`crates/unlit3d/src/unlit.rs`。
- mesh 分配与资源注册：`crates/unlit3d/src/mesh_source.rs`。
- 管线策略与 `UnlitOptions`：`crates/unlit_wgpu/src/pipeline.rs`。
- 帧行为与帧时钟：`crates/unlit3d/src/frame.rs`。
- 输入：`crates/unlit3d/src/input/mod.rs`。
- UI：`crates/unlit3d/src/ui/mod.rs`。
- winit 桥与内置行为：`crates/unlit3d/src/winit/` 下的 `event.rs` / `builtin.rs` / `surface.rs`。
- 多 world：`crates/unlit3dx/README.md`，代码在 `crates/unlit3dx/src/world.rs`、`host.rs`、`job.rs`、`channel.rs`、`request.rs`。
- 展示路径与 GPU 请求：`crates/unlit3dx/src/present.rs`、`crates/unlit3dx/src/gpu.rs`。
- 参考装配（真正的骨架长什么样）：`unlit3dx_examples/src/lib.rs`、`scene/mod.rs`、`render.rs`、`headless.rs`。
- 无窗口的交互式 MCP 宿主（按帧驱动、离屏渲染、自有工具）：`unlit3dx_examples/src/mcp.rs`。
- 端到端快照测试的写法（起子进程 + 手写 JSON-RPC 客户端）：`unlit3dx_examples/tests/mcp_input/native.rs`。
- 单 world 的完整可跑实现：`unlit3d_cli/src/lib.rs` 的 `build_world` / `offscreen_world`。
