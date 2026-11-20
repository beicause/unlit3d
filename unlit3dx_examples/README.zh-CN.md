[English](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.md) | 简体中文

# unlit3dx_examples

一个多 world 示例：渲染、物理与音频各自运行在自己的 world 里，以 job 与快照相连，
建立在 [`unlit3dx`](../crates/unlit3dx/README.zh-CN.md) 之上。包名为
`unlit3dx_examples`；二进制名为 `unlit3dx-examples`。它能在窗口中运行，也能在
浏览器里、在一条 Android activity 之后运行，同一个场景还会为快照测试无头运行。

## 你会看到什么

一片棋盘格地面和一串下落的球——球形、立方体、圆柱与圆锥，各自穿着带图案的
材质。空格键从高处随机位置丢下一个球；球下落、与地面和彼此碰撞。球
也会自动下落，大约 0.7 秒一个。相机绕着场景缓慢环绕。

背景音乐循环自音频 world 能出声起就在它自己的音轨上演奏——那一刻是用户第一
次点下面板的 Play：这次点击在所有平台解锁音频输出，在浏览器里也一并解锁页
面的音频。另一条音轨放一段
单次播放的 MIDI 序列——五声音阶上行再下行，由音乐盒音色奏出——在它的行上
按下 Play 之前保持空闲。控制面板——一个小小的 egui 窗口——按每条音轨的状态
给出它该有的按钮，丢下下一个球，并把所有活着的球收起来：未开始的序列
只显示 Play，已开始的音轨显示 Pause 或 Resume 与 Rewind，而播完的单次序列
会变回只显示 Play。音乐不依赖球：清空场景时音乐照常演奏。

## 如何运行

```sh
cargo run -p unlit3dx_examples
```

合成器演奏的音色库在构建期由承载它的 [`asset_bytes`](asset_bytes/README.zh-CN.md)
crate 嵌入二进制：同一份字节能到达窗口、浏览器与 Android activity，运行期不需要文件。
`--soundfont <path>` 可改为加载另一份音色库；运行不认识的参数会被拒绝。
「这些字节为什么单独放一个 crate」以及缺了被 gitignore 的
`assets/FluidR3Mono_GM.sf3` 时会怎样，见该 crate 的 README。

`--mcp-stdio` 开关把各个 world 通过[模型上下文协议](../crates/unlit_ecs/README.zh-CN.md)
对外服务，而不是随窗口一起退出：工具随后可以检视每个 world，为渲染 world 保存截图，
并把音频 world 的混音录成文件——默认 FLAC（两者中更小的那个），文件名另有要求时
则录成 WAV。

在 web 上它是 `cargo xtask run-wasm` 构建、并由 Pages 工作流发布的两个页面之一。
浏览器没有可放置 world 的线程，于是改由库的步进 runner 从帧循环驱动它们全部。
Android APK 加载的 `.so` 则由 `unlit3dx_examples_android` 构建——该 crate 只放导出，
因为在这里写 `cdylib` 会让桌面和浏览器也一并链接动态库——
`cargo xtask build-android` 负责构建它，悬浮的 tab 条则在 `unlit3d` 与
`unlit3dx` 两条 activity 之间切换。

## 离屏模式

```sh
cargo run -p unlit3dx_examples -- --headless --max-frames 300 --capture-frames 299 --capture-out headless
```

`--headless` 运行同一个场景，但不用窗口、线程或音频设备。它就是浏览器那条
路径，只是帧由测试而非重绘交来：`spawn_scene::<Stepped>` 把每个 world 都放进
宿主自己的 world，骨架的 `Stepping` 一帧一帧地推进它们：物理按它自己的频率
（`PHYSICS_HZ`，60 Hz）到期才步进，合成器按采样率填块，玩法则每帧 tick 一次，
时长就是这个运行给定的 `HEADLESS_HZ`。`--max-frames` 让运行在走满这么多帧后结束，
`--capture-frames` 点名要留哪几帧：每帧在 `--capture-out` 下写成
`frame_<index>.png`，运行真的播放过东西时再写一段 `audio.flac`。两个开关在窗口运行上
同样生效，只是窗口上的捕获还会把呈现切到离屏路径——交换链的图读不回来——而窗口只
写帧：它的混音由音频设备按设备时钟填充，与可能由客户端一次一步推进的帧没有共同
时间基，那些样本对应不到捕获点名的帧上。每一帧长度都相同、
生成骰子也是确定的，因此每次运行都可复现——[快照测试](tests/headless.rs)正建立
在此之上：渲染帧与 webp 基准比较，混音与 FLAC 基准比较，基准都保存在
[`unlit3d_asset_files`](../unlit3d_asset_files) 子模块里；
`SNAPSHOT_UPDATE=1` 重新保存基准。

`--headless --interactive --mcp-stdio` 则等待 MCP 命令，只在 `step_frames` 工具被调用时推进。场景渲染到
离屏纹理，客户端用 `read_texture_as_image` 取图；`scene_status` 报告运行本身——帧号、
各 world 的句柄、离屏目标——而示例自己的 `scene_extra_status` 补充场景：球数、MIDI
序列状态与每个面板按钮的中心。`request_exit` 让进程退出。该模式下自动掉球与背景自动播放
均关闭，因此每个可见与可听的变化都来自客户端。[`mcp_input` 集成测试](tests/mcp_input.rs)
启动这个二进制，只通过 MCP 的 `send_input` 点击面板、捕获三张图片快照、把序列播放期间
录下的混音与第四条基准比较、断言暂停之后录下的混音为静音且不落盘，并请求进程正常退出。不带 `--interactive` 时由运行自己推进：`--headless --mcp-stdio
--max-frames 600` 走完这些帧，并在帧与帧之间服务 MCP，客户端可以观察与注入，但节奏归运行。
这种运行按真实时钟配速；不给 `--max-frames` 时它会一直跑下去，直到客户端请求退出
（`request_exit`）或进程被杀掉（Ctrl-C、`kill`）。

同一段剧情由 `tests/mcp_input/shared.rs` 的一份描述跑两遍：
[原生驱动](tests/mcp_input/native.rs) 通过 MCP 与那个子进程对话，
[wasm 驱动](tests/mcp_input/wasm.rs) 则用 `Interactive` 在原地组装出同一个宿主，自己推送
同样的输入事件与帧数，于是浏览器套件无需 JSON-RPC 传输、套接字或第二个线程就能跑到
交互式运行，两者比较的也是同样那四条基准。

两个快照测试都是单一测试目标（[tests/gpu_suite.rs](tests/gpu_suite.rs)）的模块。

## 各 world 如何相连

五个 world，遵循 `unlit3dx` 的协议：job 发出去，快照传回来。其余 world
扮演服务器，玩法 world 里则是驱动它们的那些对象。`spawn_scene` 通过一个 runner
构建它们——窗口里是 `Threaded`，浏览器与离屏运行是 `Stepped`——因此宿主在两种
情况下都携带同样的实体与组件，差别只在于谁调用 tick、以及如何配速。

- **宿主（host）** 拥有窗口并驱动帧。它请求 GPU；GPU 应答之后它装配渲染 world
  与展示路径，随后一切交给库里的行为：`draw_presented` 每帧排空渲染 world 的
  job 收件箱、分发帧并呈现，`pump_presented` 在窗口存活期间让帧持续到来，
  `resize_presented` 跟随窗口变化——宿主对物理与音频一无所知，也不知道场景里
  有什么。
- **渲染 world** 是主线程上的子 world，表现得像一台 Godot 式的资源服务器：
  玩法用 job 请求网格、材质与实例，每个生成实例的实体作为句柄传回。场景往里
  放的一切都以同样的方式到达：环绕的相机、刚体所带的姿态也都是 job，并在任何绘制
  之前被排空。宿主在那里建的则是管线而不是场景——资源图、renderer、mesh source，
  以及绘制控制面板的那个 source——它们在 GPU 应答的那一刻就建好，早于第一次
  drain 可能执行任何 job。资源服务器本身——网格、材质与实例的分配，回收镜头看不见
  的实例的那一轮清扫，以及相机辅助——就是本 crate 自己的 `src/render.rs`，
  建立在 `unlit3d` 的通用 `MeshSource` 注册表之上；`unlit3dx` 不自带 render 桥。
- **物理 world** 以固定频率（`PHYSICS_HZ`，60 Hz）步进 rapier，每一步排空碰撞事件。
  球们用自己的收集闭包
  订阅它——`GamePhysics` 载荷属于本 crate，不属于库——闭包在姿态未变时什么都不
  返回，因此刚体都睡着的场景根本不发布快照。地面是它的第一个 job。
- **音频 world** 持有输出流，混音它所携带的任意音源，欠载时静音。它持有一台
  合成器，其音源由一个 job 装上，该 job 以音源的实体作答；音源在第一次渲染的 fill
  里、按那个 fill 报出的速率才建出合成器，因此在流还没有速率时就能装上。音乐弹的
  每个音都是发给那个实体的 job。这个 world 对外报告的是它的 `AudioFormat`（示例
  读它来应答安装）与流带来的 `AudioOutputOpen` 标记。
- **玩法 world** 是唯一知道其余三者的地方，它是一组对象而不是一坨状态。每个对象都以
  一个 bundle 生成：一个组件携带它自己拥有的东西，外加一个推进它的 `OnFrame`
  行为。其中大多数不声明顺序，跑在默认顺序带里，派发不保证带内的先后；真正读取
  别人写入的两个才声明顺序——球排在它所读的资源之后，面板的发布排在它展示的每个
  对象之后：

  - `SceneState` 用一个 job 搭出场景的资源——网格与带图案的材质，早于读取它们的
    对象被生成。每次落球都向渲染 world 要一个自己的实例。
  - `spawn_floor` 在第一次落球可能发生之前排队地面这个固定刚体，并搭建绘制它的
    棋盘格实例。
  - `Balls` 每次落球都 spawn 一个 `Ball` 实体，向渲染 world 要实例、向物理
    要刚体。两份答复都点名这个球，因此球本身就是身份：两半都到齐、并且渲染 world 确认
    已经显示，球才算显示；找不到自己球的答复会自行清理。镜头再也看不到的实例由渲染
    world despawn 并把球回报，球随之一起消失。
  - `SceneAudio` 向音频 world 索要合成器的音源，拿到答复后在本 world 里
    生成场景的两条音轨：每条是一个 `MidiSequencer` 加它的时钟、它所弹入的音源，而
    音频 world 的 sender 则来自 tick 时拿到的这个 `Scene`。背景音乐循环等第一次
    点 Play 时开播，示例序列则暂停着等面板。

  每个对象都通过一个 `Scene` 值触达其余 world：render、physics、audio 三个
  sender、物理快照，以及这三个 world 各自对自己状态的报告，交给每一次
  tick。从不对音频 world 说话的对象也就不会持有它的 sender。

  它每帧 tick 一次，时长取这一帧的实测值，因此玩法跑得像显示器驱动得那么快；
  显示器的刷新率只约束它的线程多久醒一次，从不决定时间前进多少。

控制面板是第五个对象，和其余对象一样住在**玩法 world** 里。面板只能在它自己的
egui context 所运行的地方构建，所以由这个 world 负责布局并发布帧，渲染 world 负责绘制
槽位里存着的那一帧：面板实体携带 `EguiPanel`，而生成在它旁边的 `EguiPanelHost` 承载
那个 context 并替它发布。它的传送按钮直接写这个 world 拥有的序列——播放、暂停、
倒回就是往被点那一行的 `MidiSequencer` 上写字段。渲染 world 只持有一个挂上那些槽位的
`EguiSource`，负责上传并记录它在槽位里找到的帧——[`setup_render_world`](src/scene.rs)
在它建 pass 的那一处把它生成在 pass 之下，因为 source 属于记录它的那个 pass。捕获帧的
运行改传 `None`，pass 下就只有场景自己的 source：面板照常布局、照常发布，只是没有
source 把它带进帧里。

两侧各持有一个 egui context，纹理 id 也都从零开始——缘由与后果见
[`unlit3d`](../crates/unlit3d/README.zh-CN.md) 的 egui 一节。

没人操作的面板不会在通道上产生任何开销，它最后发布的那一帧会一直被绘制，直到有更新的
一帧取代它——何时才会布局一帧的完整规则，见 [`unlit3dx`](../crates/unlit3dx/README.zh-CN.md)
的 egui 桥一节。
