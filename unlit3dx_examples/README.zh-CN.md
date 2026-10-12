[English](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/README.md) | 简体中文

# unlit3dx_examples

一个多 world 示例：渲染、物理与音频各自运行在自己的 world 里，以 job 与快照相连，
建立在 [`unlit3dx`](../crates/unlit3dx/README.zh-CN.md) 之上。包名为
`unlit3dx_examples`；二进制名为 `unlit3dx-examples`。

## 你会看到什么

一片棋盘格地面和一池彩色小球。空格键从高处随机位置丢下一个球；球下落、与地面和彼此
碰撞，每一次足够重的撞击都通过音色库合成器奏出一个五声音阶音符。球也会自动
下落，大约一秒半一个。相机绕着场景缓慢环绕。

## 如何运行

```sh
cargo run -p unlit3dx_examples
```

合成器需要一个音色库。第一个位置参数是它的路径，默认
`assets/FluidR3Mono_GM.sf3`，仓库不把它纳入版本控制；任何 SF3 音色库都可以，
例如
[FluidR3Mono_GM.sf3](https://github.com/beicause/soundfont/blob/main/FluidR3Mono_GM.sf3)。

`--mcp` 开关把各个 world 通过[模型上下文协议](../crates/unlit3d_mcp/README.md)
对外服务，而不是随窗口一起退出：工具随后可以检视每个 world，并为渲染 world
保存截图。

## 离屏模式

```sh
cargo run -p unlit3dx_examples -- --headless --frames 300 --out headless
```

`--headless` 开关运行同一个场景，但不用窗口、线程或音频设备：各个 world
在单线程上按各自的固定频率内联步进——玩法 120 Hz、物理 64 Hz、渲染 60 Hz——
共 `--frames` 帧，随后这一运行的最后一帧与整段混音写入 `--out` 下的
`screenshot.png` 与 `audio.wav`。步进是固定频率的，生成骰子也是确定的，
因此每次运行都可复现——快照测试（[English](tests/headless.rs)、
[简体中文](tests/headless.rs)）正建立在此之上：渲染帧与 webp 基准比较，
混音与 wav 基准比较，基准都保存在
[`unlit3d_asset_files`](../unlit3d_asset_files) 子模块里；
`SNAPSHOT_UPDATE=1` 重新保存基准。

## 各 world 如何相连

五个 world，遵循 `unlit3dx` 的协议：job 发出去，快照传回来。其余 world
扮演服务器，玩法是驱动它们的客户端。

- **宿主（host）** 拥有窗口并驱动帧。它请求 GPU、构建渲染 world、每帧排空
  渲染 world 的 job 收件箱并呈现结果——对物理与音频一无所知。空格键作为
  一个 job 转发给玩法。
- **渲染 world** 是主线程上的子 world，表现得像一台 Godot 式的资源服务器：
  玩法用 job 请求网格、材质与实例，生成实例的实体位作为句柄传回。它还持有
  环绕的相机，以及一个赶在任何绘制之前应用最新变换快照的行为。
- **物理 world** 以 64 Hz 步进 rapier，发布刚体变换与碰撞事件。地面是它的
  第一个 job。
- **音频 world** 持有输出流与合成器；它接收音符 job 并混音，欠载时静音。
- **玩法 world** 是唯一知道其余三者的地方。它通过向渲染服务器请求资产与
  实例来搭建场景，向物理请求刚体、把它们映射到实例句柄、发布变换快照、
  为每个接触对记一个音符并让它淡出。它以固定 120 Hz 步进，独立于帧率。
