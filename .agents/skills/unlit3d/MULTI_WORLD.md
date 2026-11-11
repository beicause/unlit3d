# 多 world（unlit3dx）

`unlit3dx` 不自带渲染器：render bridge 建在 `unlit3d` 的 `MeshSource` 上（参考实现 `unlit3dx_examples/src/render.rs`），单 world 的规则一条不改，见 [SCENE.md](SCENE.md)。签名见 [REFERENCE.md](REFERENCE.md)。

## 通信协议（唯一一种）

- `World` 是 `!Send`：`Arc<Mutex<World>>` 这条路编译器直接封死——锁不是「不推荐」，是「不可行」。多线程的正确形状是**一个关注点一个 world**，world 之间从不互调。
- **进去靠 job，回来靠订阅**：job 是唯一改变另一个世界的方式（`Box<dyn FnOnce(&mut World) + Send>`，不回答、有界队列、满了 `SendError::Full`、已停 `Closed`、永不阻塞）；拿回数据只有三种形状——`watch`（最新状态，单槽无积压）、`subscribe`（每个事件，按序）、`Pending`（一次请求一次应答，超时给 `Expired` 而不是等死）。
- **时机由内置机制保证**：job 在 tick 前 drain，快照 / 流在 tick 之后由驱动方统一发布，tick 体自己不发布。快照单槽是 `ArcSwapOption`，世界换入最新值、reader 读自己持有的副本，两边永不互锁。自造 mpsc channel、`Mutex`、共享 `Arc` 或回答式 job 只会绕过这些时机保证，还得自己面对竞争与死锁。
- **三种 runner 协议完全相同**：`Threaded`（自带线程）、`Stepped`（宿主线程上 `step_stepped_world` 推）、子 world（`spawn_sub_world` + `drain_sub_world`，也在宿主线程）。同一份 tick 体在三种下都能跑。

## 两种骨架

**窗口版**——**示意，不是库 API**：`setup_render_world` / `mount_scene_content` / `SceneStepper` 是示例自己的装配助手（`unlit3dx_examples/src/scene/mod.rs`，是 `pub` 的，但属于示例、不属于库），库只提供 `spawn_context` / `Pass` / `Renderer` / `setup_presenter` 那一层，照抄时要把它们换成自己的装配；完整骨架见 `unlit3dx_examples/src/lib.rs`。GPU 就绪前先把渲染 world 空着建出来，玩法从第一帧就握着它的 handle。

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
    // renderer / pass 是示例的 RemoteEntity：同号的 Entity 在别的 world 里指别的东西，setup_presenter 要里面的 Entity，所以取 .entity。
    // 尺寸给「还没有窗口时的兜底大小」，示例用的是 WINDOW_SIZE = (1024, 720)。
    setup_presenter(world, render.entity, renderer.entity, pass.entity, gpu, (1024, 720), false)
        .expect("the presenter is built");
})]);

world.root_scope_mut().spawn(bundle![draw_presented(render.entity)]);
let (pump, redraw) = pump_presented();
world.root_scope_mut().spawn(bundle![pump]);
world.root_scope_mut().spawn(bundle![redraw]);
world.root_scope_mut().spawn(bundle![resize_presented()]);
host.run(event_loop)?;
```

**无窗口 / 定步长**（测试、离屏、wasm）——**示意骨架**，进程自己驱动每一帧：

```rust
// 示意骨架：render_world/pass/device 来自 setup_render_world 返回的装配（ctx 是它的
// RenderContext），size 是你要的离屏尺寸；名字对应该文其它片段。
let step = 1.0 / HEADLESS_HZ;                                 // 每帧时长（秒）
// 目标建在它在的那个 world 里；pass 由 setup_render_world 建好。
let target = PersistentTarget::bind(&render_world, ctx, pass.0, &device, size, TargetConfig::default())
    .expect("the offscreen target is bound");
let mut frame = Frame { delta_time: step as f32, index: 0, size };
for index in 0..frames {
    frame.index = index;
    stepper.step(&host, &worlds, step);                       // 玩法 → 物理 → 音频
    let mut nested = host.root_scope().query_one::<&mut World>(render.entity)
        .expect("the render sub-world exists");
    let world = nested.get();                                 // QueryOne 没有 Deref，先 get() 借出 &mut World
    drain_sub_world(world);                                   // render world 的 job 在宿主线程跑
    dispatch_frame(world, &mut frame);
    let _ = world.root_scope().query_one::<&mut Renderer>(renderer.0)
        .map(|mut row| { row.get().render(&world); });        // render 要 &World，不能再借 &mut
    // target.texture 是资源节点的 handle，取回真纹理才能回读。
    let texture = world.root_scope().query_one::<&mut Renderer>(renderer.0)
        .map(|mut row| row.get().context())
        .expect("the renderer exists")
        .graph(&world.root_scope()).get()
        .get(&target.texture).cloned()
        .expect("the target texture is a resource node");
    let pixels = readback_texture(device, queue, &texture);
}
```

## 装配规则

- 世界装配只放配置（device/queue/tick 率/重力/缓冲大小），**游戏内容一律走 job**；setup 里出现「游戏里的某个东西」就说明写错了层。
- 一个 scene 对每个它要说话的世界只留一个 port（`JobSender` + 订阅值），收在一个结构体里递下去。
- 跨 world 的实体用 `unlit_ecs::RemoteEntity`（句柄 + 产出它的 `WorldId`）：每个 world 的实体编号都从 0 开始，裸 `Entity` 一定会混。跨 world 传值时用 `World::remote` 收、用 `World::resolve` 解，解不开说明句柄来自别的 world。

## 展示路径与离线取帧

- `setup_presenter(.., offscreen)` 传 `false` 时场景直画交换链（`PresentationPath::Surface`）；要截图、快照或 MCP 就传 `true`（`PresentationPath::Intermediate`），场景画进常驻 target、再由第二个 pass 贴到交换链——纹理才活得过 present，交换链换格式或尺寸也不碰场景 pipeline。
- 中间纹理的尺寸与采样是 `TargetConfig`：`size` 用 `IntermediateSize`（`Scale(1.0)` 跟随窗口、`Scale(0.5)` 半宽半高、`Fixed(w, h)` 固定像素），`samples` / `with_depth` 决定附件。presenter 的 `resize` 会按新的窗口尺寸重新 resolve 并原地换掉纹理与 view，blit 的 bind group 靠依赖自动重建。
- 离线取帧：`PersistentTarget::bind(&world, ctx, pass, &device, size, config)` 拿到 `{ texture, view }`，再配 `readback_texture`。`device` 必须是 `setup_presenter` 用的那个 `gpu.device`，否则附件进不了同一个图。
- 离屏目标必须在**它有读者的那个 world** 里建：全屏截图的离屏纹理建在渲染子 world 里，而 MCP 工具作用于宿主 world，于是 `screenshot` 在宿主里找不到纹理。要截渲染 world 就指名 `{"Handle": bits}`（`list_worlds` 取），或把 target 建在工具所在的 world。
