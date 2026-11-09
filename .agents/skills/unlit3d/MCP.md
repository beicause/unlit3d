# 用 MCP 操纵运行中的 world

工具签名与数量以 `tools/list` 的返回为准；world / 资源 id 一律从返回值取，别抄示例里的数值。排查项见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)，源码位置见 [EXAMPLES.md](EXAMPLES.md)。

## 服务器与驱动

MCP 服务器住在 `unlit_ecs`（`crates/unlit_ecs/src/mcp/`）里，让 agent 读写一个**正在运行**的 world。它没有特权通道：每个工具都写在公开 API（`World`、`Renderer`、`MeshSource`、`ResourceGraph`）之上，且服务器本身不认识引擎——工具由定义其作用对象的 crate 各自用 `inventory::submit!` 注册（`unlit_ecs` 注册 world 与实体工具、`unlit3d` 注册渲染工具、`unlit3dx` 注册音频录音与宿主控制工具）。传输是 stdio——由客户端**启动进程**，没有常驻端口，仓库里也没有 `.mcp.json`。示例程序还能注册自己的工具：`unlit3dx_examples --headless --mcp` 就多两个按帧驱动的工具。

客户端用 **mcpdo**（MCP Inspector 的连接 CLI，随 `@modelcontextprotocol/inspector` 一起装）：它起一个常驻 daemon，`connect` 一次之后，后续每条命令都打到**同一个宿主进程**——world 的状态跨命令持续，这是它比「每次调用都重新启动宿主」的一次性客户端（`mcp-inspector --cli`）更合适的根本原因。`@modelcontextprotocol/inspector` 已全局安装就直接调 `mcpdo`（下文命令都这么写）；没装就在每条命令前加免安装前缀（首次要下载，慢）：

```sh
pnpm dlx --package @modelcontextprotocol/inspector mcpdo <子命令…>
```

```sh
# 连接：stdio 宿主以 -- 分隔写在后面；连接名就是 target 本身
mcpdo connect -- ./target/debug/unlit3dx-examples --headless --mcp

# 之后所有命令都用 --connection（或 @<连接名>）指名这条连接
mcpdo --connection './target/debug/unlit3dx-examples' tools/list --format json
mcpdo --connection './target/debug/unlit3dx-examples' tools/call scene_status --format json
mcpdo --connection './target/debug/unlit3dx-examples' tools/call step_frames frames:=10 --format json
mcpdo disconnect './target/debug/unlit3dx-examples'
```

要点：

- **一次会话 = 一次 connect + N 条 tools/call**，最后 `disconnect`（或者直接 kill 宿主）。工具参数写 `key:=value`（`:=` 后是按 JSON 解析的字面量，对象/数组/字符串都行）。
- **`--format json` 给 agent 读**；不加是给人看的带色文本。stdout 只有结果，宿主的日志走 stderr，不碍事。
- 连接在 daemon 里**跨 shell 持久**，所以「连点两个按钮中间要 `step_frames`」这类多步流程天然成立；一次性客户端每次调用都拿到全新宿主，做不了这种事。
- 桌面 / winit 宿主需要 `DISPLAY` 或 `WAYLAND_DISPLAY`，`connect` 前设好（如 `WAYLAND_DISPLAY=wayland-1`）。
- daemon 把状态放在 `~/.mcp-inspector/`；沙箱只读 home 时用 `HOME=<可写目录>` 指过去。
- 换宿主就再开一条连接：`mcpdo connect -- ./target/debug/unlit3d-examples --mcp --scene spin_cube`。

## 四个 `--mcp` 宿主

| 宿主             | 命令                                                                              | 说明                                       |
| ---------------- | --------------------------------------------------------------------------------- | ------------------------------------------ |
| 多 world 示例    | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --mcp`                 | 渲染/物理/音频/玩法各自成 world            |
| 多 world 无窗口  | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --headless --mcp`      | 无 winit，按帧驱动，多两个工具，见下节     |
| 单 world 示例    | `cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube` | glTF 场景与内建 UI                         |
| CLI              | `cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp`                             | 一个空的离屏 world：固定 960×720、4 采样、带深度，其余选项一律忽略 |

## 必须知道的行为

- **桌面端需要 `DISPLAY` 或 `WAYLAND_DISPLAY`**。winit 没有无窗口模式，缺了会 panic：`neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set.` 要完全无窗口就用 `unlit3dx_examples --headless --mcp`，它不建 winit 事件循环。
- **`--mcp` 会改变渲染路径**：`unlit3dx_examples` 里 `let offscreen = mcp;`，即带 `--mcp` 时场景自己的 pass 离屏渲染，同一个 world 里再加一个 pass 把结果贴到交换链。world 数量不变，只是多一个 `FramePass` 实体。
- `unlit3dx_examples` 的 `--stepped` 让桌面端走浏览器那条单线程路径（world 由帧循环步进，而不是各自开线程）。
- `unlit3d_examples` 的 MCP 只在桌面可用，命令经 winit 用户事件队列投递，不是 `async_channel`。
- **命令要有人泵**。`unlit3dx` 的桥接把命令投进各 world 的队列，桌面端由 winit 的 `about_to_wait` 行为泵，无窗口的宿主必须自己循环调 `unlit3dx::mcp::pump_mcp(&mut host)`。它除了 drain 命令还会把排队到 stepped world 的 job 跑掉——stepped world 的队列本来只在 `step_stepped_world` 里推进，只等回复而不推进帧的客户端会一直等不到答复。

## WorldId 与世界名

每个工具都用 `WorldId` 指名 world：字符串 `"Host"` 指宿主线程自己持有的 world，`{"Handle": bits}` 指某个宿主实体携带的 world（`bits` 是该实体的 `u64`）。**先调 `list_worlds` 再指名别的 world。**

宿主暴露它携带的每一个可调度的 world：带 `&World` 组件的子 world（`spawn_sub_world`），以及由线程或步进驱动的 world（`spawn_threaded_world*` / `spawn_stepped_world*`，它们把命令通道留在代理实体上的 `WorldSender` 里）。子 world 的命令在宿主线程上同步执行；其余 world 的命令作为 job 投进它的执行者——它自己的线程，或 `step_stepped_world`。

`list_worlds` 返回的是 `{"world": <WorldId>, "name": "..."}` 对象数组，名字是 spawn 时给的 `&'static str`（留在实体上的 `WorldName`）。**每个被报告的 world 都一定有名字**：宿主携带但不带 `WorldName` 的 world 不会出现在结果里。例如 `unlit3dx_examples --mcp` 会依次报出 host、render、physics、audio、gameplay 这些名字——具体句柄值每次启动都变，以返回值为准。

## 工具清单

工具按注册 crate 分六组，**名称、参数与数量以 `tools/list` 返回为准**；`unlit3dx_examples --headless --mcp` 另外注册 2 个自有工具（见下节）。所有工具的 `world` 都可省略（默认 `"Host"`）。

### world 与实体

| 工具              | 作用                                 | 关键参数                              |
| ----------------- | ------------------------------------ | ------------------------------------- |
| `list_worlds`      | 宿主携带的所有 world 及其名字        | 无                                    |
| `world_summary`    | 实体数、archetype 数、见过的组件名   | 无                                    |
| `list_components`  | 服务器能编解码的组件名               | 无                                    |
| `list_archetypes`  | 每个 archetype 的组件名与长度        | 无                                    |
| `list_entities`    | 实体及其组件名                       | `limit`（默认 256）                   |
| `get_entity`       | 单个实体的组件、archetype、row       | `entity`                              |
| `get_component`    | 读一个组件为 JSON                    | `entity`、`component`                 |
| `set_component`    | 用 JSON 覆盖一个组件                 | `entity`、`component`、`value`        |
| `spawn_entity`     | 按「组件名 → JSON」整体 spawn        | `components`                          |
| `despawn_entity`   | 删除实体                             | `entity`                              |

组件名可以是裸类型名（`Transform`），也可以是模块限定名（`unlit3d::components::Transform`），后者用来区分不同模块里的同名组件。实体 spawn 后**不能增删组件**（ECS 的约定），所以 `spawn_entity` 一次给全，`set_component` 只就地覆盖字段。组件之所以能被 JSON 指名，是因为它们在 `unlit3d` 的 `reflect` feature 下用 `inventory::submit!` 提交了 link-time 表项；没有注册的组件既读不到也写不了。

### 资源图

| 工具                     | 作用                                     | 关键参数                                  |
| ------------------------ | ---------------------------------------- | ----------------------------------------- |
| `graph_summary`           | 按种类统计节点数、脏的与可重建的         | `context`                                 |
| `list_resources`          | 每个资源槽的 handle、种类、脏否、可重建否 | `context`                                 |
| `resource_dependencies`   | 一个资源依赖的 handle                    | `id`                                      |
| `graph_maintain`          | 丢掉无引用资源、重建脏资源               | `context`                                 |
| `read_buffer`             | 读 buffer 为 base64（不推荐）             | `id`、`offset`、`size`                    |
| `save_buffer`             | 读 buffer 写到文件（推荐）               | `id`、`directory`、`file_name`            |
| `read_texture_as_image`   | 读纹理为内联 base64 编码的图片（不推荐） | `id`、`format`                            |
| `save_texture_as_image`   | 读纹理写到文件（推荐）                   | `id`、`directory`、`file_name`            |

资源用 `list_resources` 报的 `u64` id 指名——它是 `ResId` 的 bits：低 32 位是槽号，高 32 位是槽的 generation；已被移除的 id 不会再解析到复用了该槽的新资源。`format` 取 `webp`（默认，无损）、`png`、`jpeg`；文件名扩展名决定格式，`save_buffer` 默认 `.bin`，其他不认识的扩展名会被拒绝。

### 输入

| 工具           | 作用                           | 关键参数             |
| -------------- | ------------------------------ | -------------------- |
| `input_state`   | 读输入状态（含本帧到达的事件） | `entity`             |
| `send_input`    | 注入事件并投递                 | `events`、`entity`   |

`send_input` 走窗口同一条 `dispatch_input`，行为组件看到的事件与真人操作一致。

### 绘制

| 工具              | 作用                                 | 关键参数                                                                                              |
| ----------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| `create_mesh`      | 用顶点数据建 mesh 并 spawn 绘制实体  | `positions`（必填）、`uvs`、`colors`、`normals`、`indices`、`transform`、`color`、`cutoff`、`hull_width`、`source` |
| `remove_mesh`      | 释放 mesh 并删除实体                 | `entity`                                                                                              |
| `render_frame`     | 渲染一帧                             | `renderer`                                                                                            |
| `read_screenshot`  | 渲染一帧并内联返回图像（不推荐）     | `renderer`、`target`、`format`                                                                       |
| `save_screenshot`  | 渲染一帧并保存图像为文件（推荐）     | `directory`（必填）、`file_name`、`renderer`、`target`                                             |
| `load_gltf`        | 从磁盘加载 glTF 并 spawn 默认场景    | `path`、`source`                                                                                      |

`renderer` / `source` / `context` 分别是携带 `Renderer` / `MeshSource` / 渲染上下文的实体 bits，`target` 是纹理资源 id——都省略时服务器取「该 world 唯一的一个」。**优先用 `save_*` 而不是 `read_*`**：内联图片或 base64 会永久占掉一块模型上下文，写到磁盘的文件可以按需读、也可以不读。

### 音频录音

| 工具                      | 作用                                                         | 关键参数                                    |
| ------------------------- | ------------------------------------------------------------ | ------------------------------------------- |
| `start_audio_recording`    | 开始录一个音频 world 填出的混音                              | `duration_seconds`（默认 60）               |
| `stop_audio_recording`     | 停止录音并把捕获的混音写成文件（格式看扩展名）               | `directory`（必填）、`file_name`            |
| `read_audio_recording`     | 停止录音并**在回复里**返回音频内容块，不碰文件系统           | `format`（默认 `flac`）                     |
| `cancel_audio_recording`   | 停止录音并丢弃已捕获的内容                                   | 无                                          |
| `audio_recording_status`   | 报告当前录到的帧数与秒数                                     | 无                                          |

录音器排在填充顺序的最末，录到的是所有音源与 world 的 `GlobalAudioGain` 之后的混音；格式（采样率与声道数）在第一次填充时学到，所以 `--stepped` 或设备尚未打开时也能录。同一个 world 同时只允许一段录音，第二段会被拒绝。两种格式装的是同一批 16-bit 样本，选法与图片工具一致：落盘时看 `file_name` 的扩展名（`.flac` 无损且更小，是无扩展名时的默认，另有 `.wav`），内联返回时看 `format` 参数。`read_audio_recording` 是给自动化断言用的：要在内存里判断「这段混音是不是静音」时，别先落盘再读回来。

### 宿主控制

| 工具            | 作用             | 关键参数 |
| --------------- | ---------------- | -------- |
| `request_exit`   | 让宿主正常退出   | 无       |

`request_exit` 把宿主 root scope 上每个 `ExitRequest` 置为 `true`。它只对**会检查这个组件**的宿主有意义：`unlit3dx_examples` 的 `--headless --mcp` 循环每轮读它、winit 宿主在 `about_to_wait` 里读它；没有这个组件的宿主会回 `the host has no exit request`。用它收尾能让自动化跑到进程自己退出，而不是靠 kill 看退出码。

## 按帧驱动（`--headless --mcp`）

`unlit3dx_examples --headless --mcp` 是不建 winit、只渲染到离屏纹理的宿主：它**不自己推进时间**，只有 `step_frames` 让场景往前走。这样一次完整的「输入 → 渲染 → 录音」流程不需要 sleep，帧号、模拟步长和截图内容都确定。它也关掉了自动掉球与背景自动播放，初始状态严格静止。

| 工具            | 作用                                                             | 关键参数        |
| --------------- | ---------------------------------------------------------------- | --------------- |
| `step_frames`    | 按固定 60 Hz 帧推进场景                                          | `frames`（≥1）  |
| `scene_status`   | 报告帧号、球数、MIDI 序列状态、各按钮中心、world 与纹理 id       | 无              |

`scene_status` 的字段：`ready`、`frame`、`balls`、`sequence_paused`、`sequence_time`、`sequence_finished`、`buttons`（`{"按钮标签": [中心 x, 中心 y]}`）、`render_world`、`audio_world`、`target`（离屏纹理的 `ResId` bits）。（`ready` 目前恒为 `true`，是硬编码的保留字段，不要拿它判断玩法 world 起没起来。）

一个典型回合（所有 `Handle` 与 `id` 都是示意值，照 `scene_status` 报的填；`C` 是这条连接的名字）：

```sh
mcpdo connect -- ./target/debug/unlit3dx-examples --headless --mcp
C='./target/debug/unlit3dx-examples'

mcpdo --connection "$C" tools/call scene_status --format json
mcpdo --connection "$C" tools/call step_frames frames:=60 --format json
mcpdo --connection "$C" tools/call send_input 'world:=<gameplay_world>' 'events:=[
  {"Mouse": {"Moved": {"position": <button_center>}}},
  {"Mouse": {"Button": {"position": <button_center>, "button": "Primary", "pressed": true}}},
  {"Mouse": {"Button": {"position": <button_center>, "button": "Primary", "pressed": false}}}
]' --format json
mcpdo --connection "$C" tools/call step_frames frames:=60 --format json
mcpdo --connection "$C" tools/call read_texture_as_image 'world:=<render_world>' 'id:=<target>' 'format:="png"' --format json

mcpdo disconnect "$C"
```

要点：

- **`world` 必须指名**：渲染是子 world、录音是音频 world、输入走玩法 world——每次启动的 bits 会变，照 `scene_status` 报的用。
- **点击要打给玩法 world 的 `send_input`**，坐标用 `scene_status` 里 `buttons` 给的中心（物理像素，`WINDOW_SIZE` 是 1024×720）。egui 一帧最多结算一次点击，所以连点两个按钮必须在两次点击之间 `step_frames`。
- **读图用 `read_texture_as_image` 而不是 `save_screenshot`**：两者都会先 `render_frame`（各多画一帧，与 `step_frames` 的绘制重复），但前者只把图内联进回复、不落盘。要落盘才用 `save_screenshot`。`id` 用 `scene_status` 的 `target`。
- **录音在内存里取**：`start_audio_recording`（给音频 world）→ `step_frames` → `read_audio_recording`，回复里是 `type` 为 `audio` 的内容块，解出来就是样本；结构化内容里另有帧数、秒数、采样率与声道数。暂停一个 MIDI 序列后，被按下的音符还要走完 patch 的 release，所以要再推进一段（几十到几百帧）才真正静音。
- **收尾用 `request_exit`**，然后断言进程退出码与 stderr 没有错误。
