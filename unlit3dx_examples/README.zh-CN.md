[English](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.md) | 简体中文

# unlit3dx_examples

一个多 world 示例：渲染、物理与音频各自运行在自己的 world 里，以 job 与快照相连，
建立在 [`unlit3dx`](../crates/unlit3dx/README.zh-CN.md) 之上。包名为
`unlit3dx_examples`；二进制名为 `unlit3dx-examples`。它能在窗口中运行，也能在
浏览器里、在一条 Android activity 之后运行，同一个场景还会为快照测试无头运行。

## 你会看到什么

一片棋盘格地面和一池下落的形体——球、立方体、圆柱与圆锥，各自穿着带图案的
材质。空格键从高处随机位置丢下一个形体；形体下落、与地面和彼此碰撞。形体
也会自动下落，大约 0.7 秒一个。相机绕着场景缓慢环绕。

背景音乐循环自音频 world 能出声起就在它自己的音轨上演奏。另一条音轨放一段
单次播放的 MIDI 序列——五声音阶上行再下行，由音乐盒音色奏出——在它的行上
按下 Play 之前保持空闲。控制面板——一个小小的 egui 窗口——按每条音轨的状态
给出它该有的按钮，丢下下一个形体，并把所有活着的形体收回池中：未开始的序列
只显示 Play，已开始的音轨显示 Pause 或 Resume 与 Rewind。音乐不依赖形体：
清空场景时音乐照常演奏。

## 如何运行

```sh
cargo run -p unlit3dx_examples
```

合成器演奏的音色库在构建期嵌入二进制：`build.rs` 读取
`assets/FluidR3Mono_GM.sf3`（仓库不把它纳入版本控制），`include_bytes!` 把它
带进二进制，同一份字节在任何平台都能用，wasm 也不例外。文件缺失时构建发出
警告，示例静音运行。把任何 SF3 音色库放到该路径并重新构建即可换音色，例如
[FluidR3Mono_GM.sf3](https://github.com/beicause/soundfont/blob/main/FluidR3Mono_GM.sf3)。

`--mcp` 开关把各个 world 通过[模型上下文协议](../crates/unlit_ecs/README.zh-CN.md)
对外服务，而不是随窗口一起退出：工具随后可以检视每个 world，为渲染 world 保存截图，
并把音频 world 的混音录成 WAV 文件。

在 web 上它是 `cargo xtask run-wasm` 构建、并由 Pages 工作流发布的两个页面之一。
浏览器没有可放置 world 的线程，于是改由库的步进 runner 从帧循环驱动它们全部。
Android APK 加载的也是同一个库：`cargo xtask build-android` 把它构建成 `.so`，
悬浮的 tab 条则在 `unlit3d` 与 `unlit3dx` 两条 activity 之间切换。

## 离屏模式

```sh
cargo run -p unlit3dx_examples -- --headless --frames 300 --out headless
```

`--headless` 开关运行同一个场景，但不用窗口、线程或音频设备。它就是浏览器那条
路径，只是帧由测试而非重绘交来：`spawn_scene::<Stepped>` 把每个 world 都放进
宿主自己的 world，`SceneStepper` 按各自的固定频率推进它们——玩法 120 Hz、
物理 64 Hz、渲染 60 Hz——共 `--frames` 帧，随后这一运行的最后一帧与整段混音
写入 `--out` 下的 `screenshot.png` 与 `audio.flac`。步进是固定频率的，生成骰子也是
确定的，因此每次运行都可复现——快照测试（[English](tests/headless.rs)、
[简体中文](tests/headless.rs)）正建立在此之上：渲染帧与 webp 基准比较，
混音与 FLAC 基准比较，基准都保存在
[`unlit3d_asset_files`](../unlit3d_asset_files) 子模块里；
`SNAPSHOT_UPDATE=1` 重新保存基准。

## 各 world 如何相连

五个 world，遵循 `unlit3dx` 的协议：job 发出去，快照传回来。其余 world
扮演服务器，玩法是驱动它们的客户端。`spawn_scene` 通过一个 runner 构建
它们——窗口里是 `Threaded`，浏览器与离屏运行是 `Stepped`——因此宿主在两种
情况下都携带同样的实体与组件，差别只在于谁调用 tick、以及如何配速。

- **宿主（host）** 拥有窗口并驱动帧。它请求 GPU；GPU 应答之后，一切交给库里的
  行为：`draw_presented` 每帧排空渲染 world 的 job 收件箱、分发帧并呈现，
  `pump_presented` 在窗口存活期间让帧持续到来，`resize_presented` 跟随窗口
  变化——宿主对物理与音频一无所知。空格键作为一个 job 转发给玩法。
- **渲染 world** 是主线程上的子 world，表现得像一台 Godot 式的资源服务器：
  玩法用 job 请求网格、材质与实例，每个生成实例的实体作为句柄传回。场景往里
  放的一切都以同样的方式到达：环绕的相机、控制面板、刚体所带的姿态也都是
  job，并在任何绘制之前被排空。这个 world 自身的初始化只有资源图、renderer
  与 mesh source。
- **物理 world** 以 64 Hz 步进 rapier，发布刚体变换与碰撞事件。地面是它的
  第一个 job。
- **音频 world** 持有输出流，混音它所携带的任意音源，欠载时静音。它自己的采样率
  要等流打开才知晓，于是届时发布一个 `AudioFormat`，由示例以一个 job 应答，
  按这个速率生成一台合成器与每条音轨各一台 MIDI 播放器。音色库加载期间它会发布
  就绪状态：在播放器存在之前发出的传送 job 找不到可寻址的音轨。
- **玩法 world** 是唯一知道其余三者的地方。它把搭建场景写成一个 async
  future，在 `LocalPool` 上 await 渲染服务器的资产与实例，向物理请求刚体、
  把它们映射到实例句柄、再把姿态作为一个 job 发回去。音频 world 报告自己能出声后，玩法启动
  背景音乐循环；序列由面板启动，此后面板直接驱动两条音轨，不再经过玩法。它以固定 120 Hz 步进，
  独立于帧率。
