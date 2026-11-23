# 多 world（unlit3dx）

`unlit3dx` 不自带渲染器：render bridge 建在 `unlit3d` 的 `MeshSource` 上（参考实现 `unlit3dx_examples/src/render.rs`），单 world 的规则一条不改，见 [SCENE.md](SCENE.md)。签名见 [REFERENCE.md](REFERENCE.md)。

## 通信协议（唯一一种）

- `World` 是 `!Send`：`Arc<Mutex<World>>` 这条路编译器直接封死——锁不是「不推荐」，是「不可行」。多线程的正确形状是**一个关注点一个 world**，world 之间从不互调。
- **进去靠 job，回来靠订阅**：job 是唯一改变另一个世界的方式（`Box<dyn FnOnce(&mut World) + Send>`，不回答、有界队列、满了 `SendError::Full`、已停 `Closed`、永不阻塞）；拿回数据只有三种形状——`watch`（最新状态，单槽无积压）、`subscribe`（每个事件，按序）、`Pending`（一次请求一次应答，超时给 `Expired` 而不是等死）。
- **时机由内置机制保证**：job 在 tick 前 drain，快照 / 流在 tick 之后由驱动方统一发布，tick 体自己不发布。快照单槽是 `ArcSwapOption`，世界换入最新值、reader 读自己持有的副本，两边永不互锁。自造 mpsc channel、`Mutex`、共享 `Arc` 或回答式 job 只会绕过这些时机保证，还得自己面对竞争与死锁。
- **三种 runner 协议完全相同**：`Threaded`（自带线程）、`Stepped`（宿主线程上 `step_stepped_world` 推）、子 world（`spawn_sub_world` + `drain_sub_world`，也在宿主线程）。同一份 tick 体在三种下都能跑。

## 两种骨架

**窗口 / 离屏 / 交互 / MCP 四种运行**——骨架已在库里，模块 `unlit3dx::app`：应用实现 `App` trait（`spawn_scene` 装世界、`setup_render` 装管线，`Args` 关联类型声明自己的参数结构），骨架提供 `windowed` / `headless` / `interactive` / `serve` 四个入口；运行开关（`--mcp-stdio` / `--mcp-http` / `--stepped` / `--max-frames` / `--capture-frames` / `--capture-out` 等）与应用参数由一个 clap 解析器合并读入，解析好的 `App::Args` 结构体交给 `App::configure`，无参数应用用 `NoArgs`。步进节奏用库的 `Stepping`（只含物理 60 Hz 的攒步），音频拉取由 `AudioPull` 单独负责——只有真正消费样本的入口（离屏、交互、录音）才持有它，窗口运行把混音留给音频 world 自己的 tick。应用不再自己写步进器。窗口与管线级别的原语（`setup_presenter` / `request_gpu` / `Pass` / `Renderer`）仍然导出，不想用骨架的应用直接拼它们；GPU 就绪前先把渲染 world 空着建出来，玩法从第一帧就握着它的 handle。

边界：**场景内容由玩法 world 发 job**（相机在 `spawn_scene` 里就排在渲染 world 的收件箱，网格、材质、实例、姿态同理）；**管线由宿主在 GPU 回调里同步建**（资源图、renderer、mesh source、pass、绘制面板的 `EguiSource`）。后者必须同步，因为 pass 在 GPU 应答那一刻才存在，而 job 要等那一帧的 `tick_sub_world` 才会执行——那时帧已经画完了。

输入同理，边界是**谁 tick 一个 world，谁折叠它的输入**。子 world 没有自己的回合，所以驱动它那一帧的地方负责：`tick_sub_world`（一次 drain + 折叠输入 + 跑本地任务 + `dispatch_frame`）就是这条不变式的唯一落点，`present_turn` 与任何自己驱动子 world 的进程都调它。没人折叠的缓冲会把收到的每个事件都留着——`StreamReader::pump` 一 tick 只能调一次，调两次会丢掉第一次收的批次。

```rust
struct Demo;

impl App for Demo {
    fn spawn_scene<R: WorldRunner>(&mut self, host: &mut World, ports: ScenePorts<'_>) -> SceneWorlds {
        // 玩法 / 物理 / 音频三个 world 在这里装配，场景内容经 ports.render 发 job：
        // 相机、网格、材质、实例、姿态，与下面「装配规则」一节相同。
        // audio_setup 与 automatic_drops 按模式选择，示例见
        // unlit3dx_examples/src/demo.rs 的转发。
        scene::spawn_scene::<R>(host, ports.render.clone(), audio_setup, ports.panel.clone(), ports.refresh_millihertz.clone(), ports.input, automatic_drops)
    }

    fn setup_render(&mut self, host: &mut World, ports: RenderPorts<'_>) -> RenderSetup {
        // GPU 应答那一刻才走到这里；面板 source 属于 pass，不画面板传 None。
        setup_render_world(host, ports.world, ports.device.clone(), ports.panel)
    }
}

let event_loop = EventLoop::new()?;
windowed(
    Demo,
    WindowOptions { title: "demo".into(), ..WindowOptions::default() },
    event_loop,
);
```

不想用骨架时，窗口与管线照旧一层层拼：`WindowSetup` / `ExitOn` / `request_gpu` 回调里 `setup_render_world` + `setup_presenter`，每帧用 `present_turn` 把推进与呈现绑成一趟事务，再由 `pump_presented` / `resize_presented` 收尾。

**无窗口 / 定步长**（测试、离屏、wasm）——`headless` / `interactive` 两个入口覆盖它；要自己驱动每一帧时（测试驱动器就这么做），步进与取帧是：

```rust
// 示意骨架：render_world/pass/device 来自 setup_render_world 返回的装配（ctx 是它的
// RenderContext），size 是你要的离屏尺寸；名字对应该文其它片段。捕获帧的进程给
// setup_render_world 的 panel 参数传 None，pass 下就只有 mesh source。
let step = FIXED_FRAME_SECONDS;                               // 每帧时长（秒）
// 目标建在它在的那个 world 里；pass 由 setup_render_world 建好。
let target = PersistentTarget::bind(&render_world, ctx, pass.0, &device, size, TargetConfig::default())
    .expect("the offscreen target is bound");
let mut frame = Frame { delta_time: step as f32, index: 0, size };
// Stepping 只记物理一笔账：按 PHYSICS_HZ 攒步。
// 音频拉取是另一件事，交给自己要消费样本的进程：
// AudioPull::new(sample_rate) 按采样周期攒块，audio_format 由音频 world
// 打开设备后自己 spawn，先取它再拉。
let mut stepping = Stepping::new(worlds.clone(), PHYSICS_HZ);
for index in 0..frames {
    frame.index = index;
    stepping.step(&host, step);                               // 玩法 → 物理 → 音频
    let mut nested = host.root_scope().query_one::<&mut World>(render.entity);
    let world = nested.get();                                 // QueryOne 没有 Deref，先 get() 借出 &mut World
    tick_sub_world(world, &mut frame);                        // drain job → 折叠输入 → 跑本地任务 → 分发帧
    // render 要 &World，不能再借 &mut；行只在这次调用里借出，写完即释放。
    world.root_scope().query_one::<&mut Renderer>(renderer.0)
        .get().render(&world);
    // target.texture 是资源节点的 handle，取回真纹理才能回读。
    let texture = world.root_scope().query_one::<&mut Renderer>(renderer.0)
        .get().context()
        .graph(&world.root_scope()).get()
        .get(&target.texture).cloned()
        .expect("the target texture is a resource node");
    // 读回是 future，同步驱动；放进 async 里的话直接 .await。
    let pixels = pollster::block_on(readback_texture(device, queue, &texture));
}
```

## 装配规则

- 世界装配只放配置（device/queue/tick 率/重力/缓冲大小），**游戏内容一律走 job**；setup 里出现「游戏里的某个东西」就说明写错了层。
- 一个 scene 对每个它要说话的世界只留一个 port（`JobSender` + 订阅值），收在一个结构体里递下去。
- 跨 world 的实体用 `unlit_ecs::RemoteEntity`（句柄 + 产出它的 `WorldId`）：每个 world 的实体编号都从 0 开始，裸 `Entity` 一定会混。跨 world 传值时用 `World::remote` 收、用 `World::resolve` 解，解不开说明句柄来自别的 world。

## 展示路径与离线取帧

- `setup_presenter(.., offscreen, surface_options)` 传 `false` 时场景直画交换链（`PresentationPath::Surface`）；要截图、快照或 MCP 就传 `true`（`PresentationPath::Intermediate`），场景画进常驻 target、再由第二个 pass 贴到交换链——纹理才活得过 present，交换链换格式或尺寸也不碰场景 pipeline。
- 中间纹理的尺寸与采样是 `TargetConfig`：`size` 用 `IntermediateSize`（`Scale(1.0)` 跟随窗口、`Scale(0.5)` 半宽半高、`Fixed(w, h)` 固定像素），`samples` / `with_depth` 决定附件。presenter 的 `resize` 会按新的窗口尺寸重新 resolve 并原地换掉纹理与 view，blit 的 bind group 靠依赖自动重建。
- 离线取帧：`PersistentTarget::bind(&world, ctx, pass, &device, size, config)` 拿到 `{ texture, view }`，再配 `readback_texture`。`device` 必须是 `setup_presenter` 用的那个 `gpu.device`，否则附件进不了同一个图。
- 离屏目标必须在**它有读者的那个 world** 里建：全屏截图的离屏纹理建在渲染子 world 里，而 MCP 工具作用于宿主 world，于是 `read_screenshot` 在宿主里查不到那个 id。要截渲染 world 就指名 `{"Handle": bits}`（`list_worlds` 取），或把 target 建在工具所在的 world。
