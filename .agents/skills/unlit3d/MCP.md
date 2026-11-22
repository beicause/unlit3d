# 用 MCP 操纵运行中的 world

工具签名与数量以 `tools/list` 的返回为准；world / 资源 id 一律从返回值取，别抄示例里的数值。排查项见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)，源码位置见 [EXAMPLES.md](EXAMPLES.md)。

## 服务器与驱动

MCP 服务器住在 `unlit_ecs`（`crates/unlit_ecs/src/mcp/`）里，让 agent 读写一个**正在运行**的 world。它没有特权通道：每个工具都写在公开 API（`World`、`Renderer`、`MeshSource`、`ResourceGraph`）之上，且服务器本身不认识引擎——工具由定义其作用对象的 crate 各自用 `inventory::submit!` 注册（`unlit_ecs` 注册 world 与实体工具、`unlit3d` 注册渲染工具、`unlit3dx` 注册音频录音与宿主控制工具）。传输有两种：stdio——由客户端**启动进程**，没有常驻端口，仓库里也没有 `.mcp.json`；以及 `mcp-http` feature 的 streamable HTTP——`--mcp-http` 让宿主在 TCP 端口（默认 `127.0.0.1:0`，OS 选）上服务同一组工具，适合窗口宿主或跨机调试，连接地址以启动日志里形如 `http://127.0.0.1:…` 的地址行为准（窗口宿主打「listening on」，无头宿主打「is ready on」）。示例程序还能注册自己的工具：`unlit3dx_examples --headless --interactive --mcp-stdio` 就多两个按帧驱动的工具。

客户端用 **mcpdo**（MCP Inspector 的连接 CLI，随 `@modelcontextprotocol/inspector` 一起装）：它起一个常驻 daemon，`connect` 一次之后，后续每条命令都打到**同一个宿主进程**——world 的状态跨命令持续，这是它比「每次调用都重新启动宿主」的一次性客户端（`mcp-inspector --cli`）更合适的根本原因。`@modelcontextprotocol/inspector` 已全局安装就直接调 `mcpdo`（下文命令都这么写）；没装就在每条命令前加免安装前缀（首次要下载，慢）：

```sh
pnpm dlx --package @modelcontextprotocol/inspector mcpdo <子命令…>
```

```sh
# 连接：stdio 宿主以 -- 分隔写在后面；连接名就是 target 本身
mcpdo connect -- ./target/debug/unlit3dx-examples --headless --interactive --mcp-stdio

# HTTP 宿主直接给 URL：地址取宿主日志里的 listening on http://… 行
mcpdo connect http://127.0.0.1:7860/mcp

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
- 换宿主就再开一条连接：`mcpdo connect -- ./target/debug/unlit3d-examples --mcp-stdio --scene spin_cube`。

## 四个 MCP 宿主

两个 unlit3dx 宿主同时认 `--mcp-stdio`（stdio）与 `--mcp-http`（TCP，还可加 `--mcp-http-addr` 指定地址）；两个开关可以同开。unlit3d 系的两个宿主只认 `--mcp-stdio`，HTTP 传输仅在 unlit3dx 的应用骨架里接线。

| 宿主             | 命令                                                                                        | 说明                                       |
| ---------------- | ------------------------------------------------------------------------------------------- | ------------------------------------------ |
| 多 world 示例    | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --mcp-stdio`                     | 渲染/物理/音频/玩法各自成 world            |
| 多 world 无窗口  | `cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --headless --interactive --mcp-stdio` | 无 winit，按帧驱动，多两个工具，见下节     |
| 单 world 示例    | `cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp-stdio --scene spin_cube`     | glTF 场景与内建 egui                       |
| CLI              | `cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp-stdio`                                 | 一个空的离屏 world：固定 960×720、4 采样、带深度，其余选项一律忽略 |

## 必须知道的行为

- **桌面端需要 `DISPLAY` 或 `WAYLAND_DISPLAY`**。winit 没有无窗口模式，缺了会 panic：`neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set.` 要完全无窗口就用 `unlit3dx_examples --headless --interactive --mcp-stdio`，它不建 winit 事件循环。
- **`--mcp-stdio` 与 `--mcp-http` 会改变渲染路径**：带任一开关时场景自己的 pass 画进常驻纹理（`PresentationPath::Intermediate`），同一个 world 里再加一个 blit pass 把结果贴到交换链。world 数量不变，只是多一个 `FramePass` 实体。不改这条路径的话，MCP 截图拿不到画面——交换链图像不是资源图节点。
- **节奏轴独立于传输轴**：不带 `--interactive`，宿主自己推进时间，MCP 只是观察与注入（无头自驱运行在帧与帧之间泵命令；窗口宿主本来就这样）；带 `--interactive`，时间只随 `step_frames` 前进。无头下它建 Interactive 宿主；窗口下它关掉自动步进、强制 stepped 世界（自成线程的世界无法被客户端拦住），`step_frames` 通过 host world 里的 `Pacing` 组件（记录客户端配速和帧预算的 `Pacing` 组件；窗口帧由 `WindowFrames::submit` 逐帧提交）推进场景，返回 `{"frame": n, "stepped": k}`——`frame` 是跑过这些步之后 run 走到的帧数（两条路径同一个计数），`stepped` 是这次真的推进了几帧。给了 `--max-frames` 时步数会被截到预算末尾，所以 `stepped` 可能小于请求的 `frames`：`k` 的小于请求值是预算用尽，不是命令丢了，而 `scene_status` 这类只认交互式无头宿主的工具会报错——窗口宿主的帧与音频世界要问 `frame_clock` 与 `list_resources`。`--interactive` 要生效必须搭配一个传输：没有传输时客户端到不了，运行会打一条 warn 说明该开关不生效、然后自己配速，而不是拒绝启动。
- **截图只能读到常驻纹理，且 `target` 必填**：`read_screenshot` / `save_screenshot` 按资源 id 从资源图里取 `COPY_SRC` 纹理。离屏目标/中间纹理由 `PersistentTarget` 建，创建时就带 `COPY_SRC | TEXTURE_BINDING`，所以离屏目标与中间纹理天然可读；`PresentationPath::Surface` 没有可读纹理，只能改造场景或换 `Intermediate`。`target` 既可以给纹理的 id，也可以给它任意一个 view 的 id（画进离屏目标的 pass 手里通常只有 view）。服务器不再替你挑：一个 world 里有几张可读纹理时，「哪张是本帧」只有建它的人知道，猜出来的图可能不是你要的那一帧。`PersistentTarget` 建的纹理带 `label`（缺省是 `scene color`），在 `list_resources` 里按它找就是本帧落点；`scene_status` 的 `target` 也是同一个 id。
- `unlit3dx_examples` 的 `--stepped` 让桌面端走浏览器那条单线程路径（world 由帧循环步进，而不是各自开线程）。
- `unlit3d_examples` 的 MCP 只在桌面可用，命令经 winit 用户事件队列投递，不是 `async_channel`。
- **命令要有人泵**。`unlit3dx` 的桥接把命令投进各 world 的队列，桌面端由 winit 的 `about_to_wait` 行为泵，无窗口的宿主必须自己循环调 `unlit3dx::mcp::pump_mcp(&mut host)`。它除了 drain 命令还会把排队到 stepped world 的 job 跑掉——stepped world 的队列本来只在 `step_stepped_world` 里推进，只等回复而不推进帧的客户端会一直等不到答复。

## WorldRef 与世界名

每个工具都用 `WorldRef` 指名 world：字符串 `"Host"` 指宿主线程自己持有的 world，`{"Handle": bits}` 指某个宿主实体携带的 world（`bits` 是该实体的 `u64`）。**先调 `list_worlds` 再指名别的 world。**

宿主暴露它携带的每一个可调度的 world：带 `&World` 组件的子 world（`spawn_sub_world`），以及由线程或步进驱动的 world（`spawn_threaded_world*` / `spawn_stepped_world*`，它们把命令通道留在代理实体上的 `WorldSender` 里）。子 world 的命令在宿主线程上同步执行；其余 world 的命令作为 job 投进它的执行者——它自己的线程，或 `step_stepped_world`。

`list_worlds` 返回的是 `{"world": <WorldRef>, "name": "..."}` 对象数组，名字是 spawn 时给的 `&'static str`（留在实体上的 `WorldName`）。**每个被报告的 world 都一定有名字**：宿主携带但不带 `WorldName` 的 world 不会出现在结果里。例如 `unlit3dx_examples --mcp-stdio` 会依次报出 host、render、physics、audio、gameplay 这些名字——具体句柄值每次启动都变，以返回值为准。

## 工具清单

工具按注册 crate 分六组，**名称、参数与数量以 `tools/list` 返回为准**；`unlit3dx_examples --headless --interactive --mcp-stdio` 另外注册 2 个自有工具（见下节）。所有工具的 `world` 都可省略（默认 `"Host"`）。

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
| `graph_summary`           | 按种类统计节点数、脏的与可重建的         | —                                         |
| `list_resources`          | 每个资源槽的 handle、种类、脏否、可重建否 | —                                         |
| `resource_dependencies`   | 一个资源依赖的 handle                    | `id`                                      |
| `graph_maintain`          | 丢掉无引用资源、重建脏资源               | —                                         |
| `read_buffer`             | 读 buffer 为 base64（不推荐）             | `id`、`offset`、`size`                    |
| `save_buffer`             | 读 buffer 写到文件（推荐）               | `id`、`directory`、`file_name`            |
| `read_texture_as_image`   | 读纹理为内联 base64 编码的图片（不推荐） | `id`、`format`                            |
| `save_texture_as_image`   | 读纹理写到文件（推荐）                   | `id`、`directory`、`file_name`            |

资源用 `list_resources` 报的 `u64` id 指名——它是 `ResId` 的 bits：低 32 位是槽号，高 32 位是槽的 generation；已被移除的 id 不会再解析到复用了该槽的新资源。`format` 取 `webp`（默认，无损）、`png`、`jpeg`；文件名扩展名决定格式，`save_buffer` 默认 `.bin`，其他不认识的扩展名会被拒绝。

`read_buffer` / `save_buffer` 的 `offset` 与 `size` 必须是 `COPY_BUFFER_ALIGNMENT`（4 字节）的整数倍，工具按原样报错而不是替你向下对齐——读一个结构体中间的字段时，把范围取到对齐的边界上，再把多读的字节丢掉。读纹理要求该纹理带 `COPY_SRC`、不是多重采样、格式有 block copy size（深度/模板纹理没有），否则工具直接报出是哪一条不满足。

### 输入

| 工具           | 作用                           | 关键参数             |
| -------------- | ------------------------------ | -------------------- |
| `input_state`   | 读输入状态（设备此刻的状态，不含事件） | `entity`             |
| `send_input`    | 注入事件并投递                 | `events`、`entity`   |

`send_input` 走窗口同一条 `dispatch_input`，行为组件看到的事件与真人操作一致。

### 绘制

| 工具              | 作用                                   | 关键参数                                               |
| ----------------- | -------------------------------------- | ------------------------------------------------------ |
| `spawn_gltf`      | 从路径或内联文档 spawn glTF 的默认场景 | `path` 与 `document` 二选一、`source`                  |
| `read_screenshot` | 渲染一帧并内联返回图像（不推荐）       | `target`（必填）、`renderer`、`format`     |
| `save_screenshot` | 渲染一帧并保存图像为文件（推荐）       | `target`（必填）、`directory`（必填）、`file_name`、`renderer` |

`spawn_gltf` 的 `document` 可以是 glTF JSON 文本，也可以是把 GLB 的字节 base64 编码后的字符串——JSON 参数装不下裸二进制；与 `path` 只能给其中一个。工具返回模型根和全部后代实体；删除根实体即可通过 ECS 子树清理整份场景，绘制 mesh 由 source 下一次 `maintain` 回收。

`renderer` / `source` 分别是携带 `Renderer` / `MeshSource` 的实体 bits，省略时服务器取「该 world 唯一的一个」；渲染上下文不是工具参数——它由 `spawn_context` 落在世界的唯一一行上，GPU 工具自己找它；`target` 是纹理资源 id，**必须给**，没有「唯一可读纹理」这种默认。**优先用 `save_*` 而不是 `read_*`**：内联图片或 base64 会永久占掉一块模型上下文，写到磁盘的文件可以按需读、也可以不读。删除绘制实体时应删除返回句柄中的根实体；ECS 会清理整棵模型子树，mesh 由 source 下一次 `maintain` 回收。

### 帧与时序

| 工具           | 作用                                       | 关键参数 |
| -------------- | ------------------------------------------ | -------- |
| `frame_clock`   | 报告某个 world 的帧序数、帧时长、瞬时与窗口平均帧率 | `world` |
| `frame_clocks`  | 一次报出 `list_worlds` 列出的每个 world 的帧钟     | 无       |

`frame_clock` 报告该 world 帧循环的 `index`（帧序数，从零起）、`delta_time` 与 `time`（上次与前次的帧时长与累计秒数）、`fps`（`1.0 / delta_time`，帧时长为零时省略）与 `average_fps`（最近 120 帧的滑动平均，窗口帧数见 `average_window_frames`）。第一次调用才往该 world 派生监控行为，它只从那之后的帧开始计数——所以在首次调用前 `average_fps` 是空的，多 world 宿主里每个 world 各有自己的窗口。

节奏随 world 而不同，也随 world 而报：每个真正按自己节奏循环的 world 都在自己那一拍里派发帧——渲染子世界每帧一次，物理 world 每个 tick 一次（`delta_time` 是该 tick 实测的步长，不是配置的名义 hz），音频 world 每个 tick 一次（即每次填充），gameplay 在它自己的 tick 里派。所以同一个宿主里物理、音频与渲染三个 world 的帧率各自不同，问哪个 world 就得到哪个 world 的节奏。

不派帧的 world 报空：窗口宿主自己携带的 host world 没有场景、也不派帧，它的 `index` / `delta_time` / `time` / `fps` 都是 `null`（平均值仍可来自监控行为），客户端据此区分「没有节奏」与「停在第零帧」。`frame_clocks` 把同一份报告按 `list_worlds` 的顺序排成数组（每项带 `world`、`name` 与 `clock`），比逐个 world 各采样一次更适合并排比较节奏。

### 音频录音

| 工具                      | 作用                                                         | 关键参数                                    |
| ------------------------- | ------------------------------------------------------------ | ------------------------------------------- |
| `start_audio_recording`    | 开始录一个音频 world 填出的混音                              | `duration_seconds`（默认 60）               |
| `stop_audio_recording`     | 停止录音并把捕获的混音写成文件（格式看扩展名）               | `directory`（必填）、`file_name`            |
| `read_audio_recording`     | 停止录音并**在回复里**返回音频内容块，不碰文件系统           | `format`（默认 `flac`）                     |
| `cancel_audio_recording`   | 停止录音并丢弃已捕获的内容                                   | 无                                          |
| `audio_recording_status`   | 报告当前录到的帧数与秒数                                     | 无                                          |

录音器是一段 `OnAudioPostFill` 后处理，录到的是所有音源与 world 的 `GlobalAudioGain` 之后、输出流会收到的混音；格式（采样率与声道数）在第一次填充时学到，所以设备尚未打开时也能录。录到的内容取决于该 world 有没有音源：无头与交互式运行装的是 `AudioOutputMode::Pulled` 加 soundfont，录得到真实混音；桌面端 `--stepped` 的窗口运行是 `AudioSetup::Silent`，既不装合成器也不开设备，录到的是静音。同一个 world 同时只允许一段录音，第二段会被拒绝。两种格式装的是同一批 16-bit 样本，选法与图片工具一致：落盘时看 `file_name` 的扩展名（`.flac` 无损且更小，是无扩展名时的默认，另有 `.wav`），内联返回时看 `format` 参数。`read_audio_recording` 是给自动化断言用的：要在内存里判断「这段混音是不是静音」时，别先落盘再读回来。

### 宿主控制

| 工具            | 作用             | 关键参数 |
| --------------- | ---------------- | -------- |
| `request_exit`   | 让宿主正常退出   | 无       |

`request_exit` 把宿主 root scope 上每个 `ExitRequest` 置为 `true`。它只对**会检查这个组件**的宿主有意义：`unlit3dx_examples` 的 `--headless --interactive --mcp-stdio` 循环每轮读它、winit 宿主在 `about_to_wait` 里读它；没有这个组件的宿主会回 `the host has no exit request`。用它收尾能让自动化跑到进程自己退出，而不是靠 kill 看退出码。

## 按帧驱动（`--headless --interactive --mcp-stdio`）

`unlit3dx_examples --headless --interactive --mcp-stdio` 是不建 winit、只渲染到离屏纹理的宿主：它**不自己推进时间**，只有 `step_frames` 让场景往前走。这样一次完整的「输入 → 渲染 → 录音」流程不需要 sleep，帧号、模拟步长和截图内容都确定。它也关掉了自动掉球与背景自动播放，初始状态严格静止。

去掉 `--interactive` 就是自驱形态（`--headless --mcp-stdio`，可加 `--max-frames N`）：宿主按真实时钟推进、帧间泵命令，客户端只观察与注入。给了 `--max-frames` 就在走满 N 帧后自己退出；不给则一直跑下去，直到客户端 `request_exit` 或进程被 kill——适合「让 agent 看着一个正在跑的场景」。要让客户端看得下去，帧数别太小：客户端连上要花一点时间。

自驱与客户端配速这两条路上，捕获都可以直接由命令行点名：`--capture-frames 0,10,20 --capture-out DIR` 让运行在走到这些帧号时各写一个 `frame_<index>.png`，无头或交互式运行末尾再把它播放过的混音写成 `audio.flac`（没播过东西就不写）。两个开关必须成对；只给 `--capture-frames` 不给 `--max-frames` 时，最后一帧的下一帧就是运行的终点。

窗口运行上的捕获**只写帧**，不写 `audio.flac`：窗口的混音由音频设备按设备自己的时钟填充，与客户端可能一次一步推进的帧号没有共同时间基，样本对应不到捕获点名的帧上。要连混音一起留，用 `--headless`（自驱）或 `--headless --interactive`（客户端配速），那里由帧循环自己拉样本。

| 工具            | 作用                                                             | 关键参数        |
| --------------- | ---------------------------------------------------------------- | --------------- |
| `step_frames`    | 按固定 60 Hz 帧推进场景，到预算末尾为止                          | `frames`（≥1）  |
| `scene_status`   | 报告帧号、world 与纹理 id（库提供的那一半）                      | 无              |

`scene_status` 由库提供，字段只有 `frame`、`render_world`、`audio_world`、`target`（离屏纹理的 `ResId` bits，可直接当 `read_texture_as_image` 的 `id` 与 `save_screenshot` 的 `target`；同一张纹理在 `list_resources` 里带 `label` `scene color`，两者是同一个 id）。场景自己的状态——球数、MIDI 序列、各按钮中心——由**示例**注册的 `scene_extra_status` 报：`balls`、`sequence_paused`、`sequence_time`、`sequence_finished`、`buttons`（`{"按钮标签": [中心 x, 中心 y]}`）。两个工具分开调，别指望 `scene_status` 里有按钮坐标。

一个典型回合（所有 `Handle` 与 `id` 都是示意值，照 `scene_status` 报的填；`C` 是这条连接的名字）：

```sh
mcpdo connect -- ./target/debug/unlit3dx-examples --headless --interactive --mcp-stdio
C='./target/debug/unlit3dx-examples'

mcpdo --connection "$C" tools/call scene_status --format json
mcpdo --connection "$C" tools/call scene_extra_status --format json
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
- **点击要打给玩法 world 的 `send_input`**，坐标用 `scene_extra_status` 里 `buttons` 给的中心（物理像素，`WINDOW_SIZE` 是 1024×720）。egui 一帧最多结算一次点击，所以连点两个按钮必须在两次点击之间 `step_frames`。
- **读图用 `read_texture_as_image` 而不是 `save_screenshot`**：`read_texture_as_image` 只把目标纹理读回来、不渲染也不落盘；`save_screenshot` 会先渲染一帧再读回（比 `step_frames` 的绘制多画一帧）。要落盘才用它。`id` 用 `scene_status` 的 `target`。
- **录音在内存里取**：`start_audio_recording`（给音频 world）→ `step_frames` → `read_audio_recording`，回复里是 `type` 为 `audio` 的内容块，解出来就是样本；结构化内容里另有帧数、秒数、采样率与声道数。暂停一个 MIDI 序列后，被按下的音符还要走完 patch 的 release，所以要再推进一段（几十到几百帧）才真正静音。
- **收尾用 `request_exit`**，然后断言进程退出码与 stderr 没有错误。
