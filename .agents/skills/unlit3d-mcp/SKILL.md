---
name: unlit3d-mcp
description: 通过 MCP(stdio JSON-RPC) 操纵运行中的 unlit3d / unlit3dx world：列出 world 与实体、读写组件、查看资源图、抓帧截图、发送输入、录制混音，也能无窗口地按帧驱动场景。当需要调试或自动化检查 unlit3d、unlit3dx_examples、unlit3d_examples、unlit3d_cli 的运行中场景时使用。
whenToUse: 当需要查看或修改运行中场景的状态（实体、组件、资源图）、抓一帧看渲染结果、注入输入事件、录一段运行中的混音、用 --headless --mcp 无窗口按帧驱动场景做确定性自动化，或想知道 --mcp 开关怎么用、MCP 服务器有哪些工具及其参数时。
---

# 用 MCP 操纵 unlit3d world

MCP 服务器住在 `unlit_ecs`(`crates/unlit_ecs/src/mcp/`)里，让 agent 读写一个**正在运行**的
world。它没有特权通道：每个工具都写在公开 API(`World`、`Renderer`、`MeshSource`、
`ResourceGraph`)之上，且服务器本身不认识引擎——工具由定义其作用对象的 crate 各自用
`inventory::submit!` 注册(`unlit_ecs` 注册 world 与实体工具、`unlit3d` 注册渲染工具、
`unlit3dx` 注册音频录音与宿主控制工具)。传输是 stdio——由客户端**启动进程**，没有常驻端口，
仓库里也没有 `.mcp.json`。示例程序还能注册自己的工具：`unlit3dx_examples --headless --mcp`
就多两个按帧驱动的工具。

## 先决条件与驱动

宿主必须带 `--mcp`。`scripts/mcp_drive.py` 是配套的极简客户端：启动宿主、握手、按参数
执行若干次调用。`--bin` 必填，`--args` 的值以 `-` 开头时必须写成 `--args=...`；
`max` 字段截断每条结果的长度，避免把整个实体表灌进上下文。

```sh
D=.agents/skills/unlit3d-mcp/scripts/mcp_drive.py

# 列出工具
python3 $D --bin ./target/debug/unlit3dx-examples '[{"listTools": true}]'

# 列 world → 等 6 秒 → 总览 → 截图
python3 $D --bin ./target/debug/unlit3dx-examples '[
  {"call": "list_worlds"},
  {"sleep": 6},
  {"call": "world_summary", "max": 2000},
  {"call": "save_screenshot", "args": {"world": {"Handle": 5}, "directory": "/tmp", "file_name": "shot.png"}}
]'

# 换宿主：--args 的值以 - 开头，必须写成 --args=... 的形式
python3 $D --bin ./target/debug/unlit3d-examples --args='--mcp --scene spin_cube' '[{"call": "list_worlds"}]'
```

四个 `--mcp` 宿主：

| 宿主 | 命令 | 说明 |
| --- | --- | --- |
| 多 world 示例 | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --mcp` | 渲染/物理/音频/玩法各自成 world |
| 多 world 无窗口 | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --headless --mcp` | 无 winit，按帧驱动，多两个工具，见下节 |
| 单 world 示例 | `cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube` | glTF 场景与内建 UI |
| CLI | `cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp` | 一个空的离屏 world |

必须知道的行为：

- **桌面端需要 `DISPLAY` 或 `WAYLAND_DISPLAY`**。winit 没有无窗口模式，缺了会 panic：
  `neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set.` 要完全无窗口就用
  `unlit3dx_examples --headless --mcp`，它不建 winit 事件循环。
- **`--mcp` 会改变渲染路径**：`unlit3dx_examples` 里 `let offscreen = mcp;`，即带 `--mcp`
  时场景自己的 pass 离屏渲染，同一个 world 里再加一个 pass 把结果贴到交换链。world
  数量不变，只是多一个 `FramePass` 实体。
- `unlit3dx_examples` 的 `--stepped` 让桌面端走浏览器那条单线程路径（world 由帧循环步进，
  而不是各自开线程）。
- `unlit3d_examples` 的 MCP 只在桌面可用，命令经 winit 用户事件队列投递，不是
  `async_channel`。
- **命令要有人泵**。`unlit3dx` 的桥接把命令投进各 world 的队列，桌面端由 winit 的
  `about_to_wait` 行为泵，无窗口的宿主必须自己循环调 `unlit3dx::mcp::pump_mcp(&mut host)`。
  它除了 drain 命令还会把排队到 stepped world 的 job 跑掉——stepped world 的队列本来只在
  `step_stepped_world` 里推进，只等回复而不推进帧的客户端会一直等不到答复。

## WorldId 与世界名

每个工具都用 `WorldId` 指名 world：字符串 `"Host"` 指宿主线程自己持有的 world，
`{"Handle": bits}` 指某个宿主实体携带的 world（`bits` 是该实体的 `u64`）。**先调
`list_worlds` 再指名别的 world。**

宿主暴露它携带的每一个可调度的 world：带 `&World` 组件的子 world（`spawn_sub_world`），
以及由线程或步进驱动的 world（`spawn_threaded_world*` / `spawn_stepped_world*`，它们把命令
通道留在代理实体上的 `WorldSender` 里）。子 world 的命令在宿主线程上同步执行；其余 world
的命令作为 job 投进它的执行者——它自己的线程，或 `step_stepped_world`。

`list_worlds` 返回的是 `{"world": <WorldId>, "name": "..."}` 对象数组，名字是 spawn 时
给的 `&'static str`（留在实体上的 `WorldName`）。**每个被报告的 world 都一定有名字**：
宿主携带但不带 `WorldName` 的 world 不会出现在结果里。例如
`unlit3dx_examples --mcp` 的 `list_worlds` 返回
`[{"world":"Host","name":"host"},{"world":{"Handle":5},"name":"render"},…]`，
依次是渲染，以及物理、音频、玩法四个 world。

## 工具清单

32 个工具，六组，全部由 crate 自己注册。所有工具的 `world` 都可省略（默认 `"Host"`）。
`unlit3dx_examples --headless --mcp` 另外注册了 2 个自有工具（见「按帧驱动」一节）。

### world 与实体

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `list_worlds` | 宿主携带的所有 world 及其名字 | 无 |
| `world_summary` | 实体数、archetype 数、见过的组件名 | 无 |
| `list_components` | 服务器能编解码的组件名 | 无 |
| `list_archetypes` | 每个 archetype 的组件名与长度 | 无 |
| `list_entities` | 实体及其组件名 | `limit`（默认 256） |
| `get_entity` | 单个实体的组件、archetype、row | `entity` |
| `get_component` | 读一个组件为 JSON | `entity`、`component` |
| `set_component` | 用 JSON 覆盖一个组件 | `entity`、`component`、`value` |
| `spawn_entity` | 按「组件名 → JSON」整体 spawn | `components` |
| `despawn_entity` | 删除实体 | `entity` |

组件名可以是裸类型名(`Transform`)，也可以是模块限定名
(`unlit3d::components::Transform`)，后者用来区分不同模块里的同名组件。实体 spawn 后
**不能增删组件**(ECS 的约定)，所以 `spawn_entity` 一次给全，`set_component` 只就地覆盖
字段。

### 资源图

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `graph_summary` | 按种类统计节点数、脏的与可重建的 | `context` |
| `list_resources` | 每个资源槽的 handle、种类、脏否、可重建否 | `context` |
| `resource_dependencies` | 一个资源依赖的 handle | `id` |
| `graph_maintain` | 丢掉无引用资源、重建脏资源 | `context` |
| `read_buffer` | 读 buffer 为 base64（不推荐） | `id`、`offset`、`size` |
| `save_buffer` | 读 buffer 写到文件（推荐） | `id`、`directory`、`file_name` |
| `read_texture_as_image` | 读纹理为内联 base64 编码的图片（不推荐） | `id`、`format` |
| `save_texture_as_image` | 读纹理写到文件（推荐） | `id`、`directory`、`file_name` |

资源用 `list_resources` 报的 `u64` id 指名——它是 `ResId` 的 bits：低 32 位是槽号，高 32
位是槽的 generation；已被移除的 id 不会再解析到复用了该槽的新资源。`format` 取
`webp`(默认，无损)、`png`、`jpeg`；文件名扩展名决定格式，`save_buffer` 默认 `.bin`，
其他不认识的扩展名会被拒绝。

### 输入

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `input_state` | 读输入状态(含本帧到达的事件) | `entity` |
| `send_input` | 注入事件并投递 | `events`、`entity` |

`send_input` 走窗口同一条 `dispatch_input`，行为组件看到的事件与真人操作一致。

### 绘制

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `create_mesh` | 用顶点数据建 mesh 并 spawn 绘制实体 | `positions`(必填)、`uvs`、`colors`、`indices`、`transform`、`color`、`cutoff`、`source` |
| `remove_mesh` | 释放 mesh 并删除实体 | `entity` |
| `render_frame` | 渲染一帧 | `renderer` |
| `read_screenshot` | 渲染一帧并内联返回图像（不推荐） | `renderer`、`target`、`format` |
| `save_screenshot` | 渲染一帧并保存图像为文件（推荐） | `directory`(必填)、`file_name`、`renderer`、`target` |
| `load_gltf` | 从磁盘加载 glTF 并 spawn 默认场景 | `path`、`source` |

`renderer`/`source`/`context` 分别是携带 `Renderer`/`MeshSource`/渲染上下文的实体 bits，
`target` 是纹理资源 id——都省略时服务器取「该 world 唯一的一个」。**优先用 `save_*` 而不是
`read_*`**：内联图片或 base64 会永久占掉一块模型上下文，写到磁盘的文件可以按需读、
也可以不读。

### 音频录音

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `start_audio_recording` | 开始录一个音频 world 填出的混音 | `duration_seconds`（默认 60） |
| `stop_audio_recording` | 停止录音并把捕获的混音写成文件（格式看扩展名） | `directory`(必填)、`file_name` |
| `read_audio_recording` | 停止录音并**在回复里**返回音频内容块，不碰文件系统 | `format`（默认 `flac`） |
| `cancel_audio_recording` | 停止录音并丢弃已捕获的内容 | 无 |
| `audio_recording_status` | 报告当前录到的帧数与秒数 | 无 |

录音器排在填充顺序的最末，录到的是所有音源与 world 的 `GlobalAudioGain` 之后的混音；格式
（采样率与声道数）在第一次填充时学到，所以 `--stepped` 或设备尚未打开时也能录。同一
个 world 同时只允许一段录音，第二段会被拒绝。两种格式装的是同一批 16-bit 样本，选法与
图片工具一致：落盘时看 `file_name` 的扩展名（`.flac` 无损且更小，是无扩展名时的默认，
另有 `.wav`），内联返回时看 `format` 参数。`read_audio_recording` 是给自动化断言用的：
要在内存里判断「这段混音是不是静音」时，别先落盘再读回来。

### 宿主控制

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `request_exit` | 让宿主正常退出 | 无 |

`request_exit` 把宿主 root scope 上每个 `ExitRequest` 置为 `true`。它只对**会检查这个
组件**的宿主有意义：`unlit3dx_examples` 的 `--headless --mcp` 循环每轮读它、winit 宿主在
`about_to_wait` 里读它；没有这个组件的宿主会回 `the host has no exit request`。用它收尾
能让自动化跑到进程自己退出，而不是靠 kill 看退出码。

## 按帧驱动（`--headless --mcp`）

`unlit3dx_examples --headless --mcp` 是不建 winit、只渲染到离屏纹理的宿主：它**不自己
推进时间**，只有 `step_frames` 让场景往前走。这样一次完整的「输入 → 渲染 → 录音」流程
不需要 sleep，帧号、模拟步长和截图内容都确定。它也关掉了自动掉球与背景自动播放，初始
状态严格静止。

| 工具 | 作用 | 关键参数 |
| --- | --- | --- |
| `step_frames` | 按固定 60 Hz 帧推进场景 | `frames`（≥1） |
| `scene_status` | 报告帧号、球数、MIDI 序列状态、各按钮中心、world 与纹理 id | 无 |

`scene_status` 的字段：`ready`、`frame`、`balls`、`sequence_paused`、`sequence_time`、
`sequence_finished`、`buttons`（`{"按钮标签": [中心 x, 中心 y]}`）、`render_world`、
`audio_world`、`target`（离屏纹理的 `ResId` bits）。`ready` 为 `false` 表示玩法 world
还没起来，等一两帧再问。

一个典型回合：

```sh
python3 $D --bin ./target/debug/unlit3dx-examples --args='--headless --mcp' '[
  {"call": "scene_status"},
  {"call": "step_frames", "args": {"frames": 60}},
  {"call": "send_input", "args": {"world": {"Handle": 6}, "events": [
    {"Mouse": {"Moved": {"position": [79.2, 117.0]}}},
    {"Mouse": {"Button": {"position": [79.2, 117.0], "button": "Primary", "pressed": true}}},
    {"Mouse": {"Button": {"position": [79.2, 117.0], "button": "Primary", "pressed": false}}}
  ]}},
  {"call": "step_frames", "args": {"frames": 60}},
  {"call": "read_texture_as_image", "args": {"world": {"Handle": 3}, "id": 8, "format": "png"}}
]'
```

要点：

- **`world` 必须指名**：渲染是子 world（`{"Handle": 3}`）、录音是音频 world
  （`{"Handle": 5}`）、输入走玩法 world（`{"Handle": 6}`）——每次启动的 bits 会变，
  照 `scene_status` 报的用。
- **点击要打给玩法 world 的 `send_input`**，坐标用 `scene_status` 里 `buttons` 给的中心
  （物理像素，`WINDOW_SIZE` 是 1024×720）。egui 一帧最多结算一次点击，所以连点两个按钮
  必须在两次点击之间 `step_frames`。
- **读图用 `read_texture_as_image` 而不是 `save_screenshot`**：后者会先 `render_frame`，
  在离屏宿主里会重入 present 路径并把进程打崩。`id` 用 `scene_status` 的 `target`。
- **录音在内存里取**：`start_audio_recording`（给音频 world）→ `step_frames` →
  `read_audio_recording`，回复里是 `type` 为 `audio` 的内容块，解出来就是样本；结构化
  内容里另有帧数、秒数、采样率与声道数。暂停一个 MIDI 序列后，被按下的音符还要走完
  patch 的 release，
  所以要再推进一段（几十到几百帧）才真正静音。
- **收尾用 `request_exit`**，然后断言进程退出码与 stderr 没有错误。

## 常见错误

| 现象 | 原因 |
| --- | --- |
| `failed to deserialize parameters: missing field 'directory'` | `save_*` 系列必须给 `directory` |
| `the world has no renderer` / `the world has no render context` | 指名的 world 不带 `Renderer`。`unlit3dx_examples --mcp` 下要指名渲染子 world，不是 `Host` |
| `entity N carries no world` | 该实体既不带 `World` 也不带 `WorldSender`，不是可调度的 world |
| 某个 world 不在 `list_worlds` 里 | 它没带 `WorldName`，桥接层无法给它命名，于是不报告 |
| `the world on entity N has stopped` | 那个 world 已经结束，job 不会再跑 |
| 无响应 | 宿主没起来(缺 DISPLAY 会 panic)，或宿主没带 `--mcp`，或宿主没泵命令(无窗口宿主必须每轮调 `pump_mcp`) |
| 发给 stepped world 的命令一直不回 | 宿主没在泵，或没跑 `pump_mcp` 的 job 阶段；stepped world 的队列只在推进帧时被 drain |
| `the host has no exit request` | 该宿主没挂 `ExitRequest` 组件，`request_exit` 对它无效 |
| `the host is not an interactive headless scene` | `step_frames`/`scene_status` 只存在于 `unlit3dx_examples --headless --mcp` |
| `the world has no input writer` | 指名的 world 没挂 `InputWriter`；给玩法 world 发输入，不是 `Host` |
| 点了按钮但场景没变 | 坐标不是物理像素、world 指错，或同一帧里连点多次(egui 每帧只结算一次) |
| `the world has more than one readable texture; pass target` | 不传 `id`/`target` 时服务器会猜，离屏宿主里猜不出来；用 `scene_status` 的 `target` |

## 在源码里的位置

- `crates/unlit_ecs/src/mcp/server.rs` —— `McpServer`、`ToolRouter`、`serve_stdio_with`
  与 `serve_with`，以及 `tool()`/`ToolSource` 这套注册机制。
- `crates/unlit_ecs/src/mcp/host.rs` —— 传输契约：`McpCommand`、`Output`、`WorldId`、
  `WorldEntry`、`Job`，以及落盘助手。
- `crates/unlit_ecs/src/mcp/tools.rs` —— 10 个 world 与实体工具，末尾的 `register` 是
  它们的注册表。
- `crates/unlit3d/src/mcp/` —— 16 个渲染工具(`host.rs`)与它们的 `register`(`mod.rs`)。
- `crates/unlit3dx/src/mcp/mod.rs` —— 5 个音频录音工具、`request_exit` 与它们的
  `register`；音频录制的实现在 `crates/unlit3dx/src/record.rs`。
- `crates/unlit3dx/src/mcp/bridge.rs` —— `serve_mcp(world)` 起名为 `unlit3dx-mcp-transport`
  的线程，宿主侧挂 `McpInbox` + `drain_mcp()` 的 `OnAboutToWait`；`pump_mcp(world)` 是无
  winit 宿主用的同一套 drain + apply + 跑 stepped world 队列；`reachable_worlds` 决定
  `list_worlds` 看到什么。
- `crates/unlit3dx/src/world.rs` —— `WorldSender`（留在代理实体上、让 bridge 够得着线程/
  步进 world 的命令通道）、`WorldName`（`list_worlds` 报告的名字）与
  `drain_stepped_world`（把队列里的 job 跑掉）。
- `unlit3dx_examples/src/mcp.rs` —— `--headless --mcp` 宿主：不建 winit、离屏渲染、
  `step_frames`/`scene_status` 两个自有工具。它的端到端验证在
  `unlit3dx_examples/tests/mcp_input/native.rs`（起真实进程 + 手写 JSON-RPC 客户端）。
- `unlit3d_examples/src/lib.rs` —— 走 winit 用户事件；`unlit3d_cli/src/lib.rs` —— 走 mpsc
  channel。
- `crates/unlit_ecs/README.md` —— 设计说明(无特权路径、传输、各 crate 注册工具)。

组件之所以能被 JSON 指名，是因为它们在 `unlit3d` 的 `reflect` feature 下用
`inventory::submit!` 提交了 link-time 表项；没有注册的组件既读不到也写不了。
