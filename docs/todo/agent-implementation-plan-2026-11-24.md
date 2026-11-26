# unlit3d / unlit3dx 审查结果实施计划

本文位于 `docs/todo/`，随实施推进更新。截至 2026-11-26，P0-A–P0-J、P1-A–P1-D 与 P2-A 已实施、验收并提交；P2-B–P2-D、P3-A–P3-D 与 SK-1–SK-13 未开始。逐包交付记录与未验收项见[实施进度](#实施进度)；各包的步骤与验收条件仍以下文正文为准。

## 实施进度

### 已完成的包

每个包一个提交，提交边界与下文「建议的提交边界」的分组不完全一致：本次按包提交，便于逐包回退。改动规模一列是「文件数 +增行/−删行」。

| 包 | 提交 | 改动规模 | 新增回归测试 | 状态 |
| --- | --- | --- | --- | --- |
| P0-A glTF skin | `e92732d` | 8 文件 +267/−98 | 三条模型导入路径：短于 joints、完全缺失、多于 joints | 已验收 |
| P0-B glTF 静默损坏 | `27e8b18` | 20 文件 +1198/−320 | Draco 解码错误、流外 morph accessor、浮点 HDR 保真、归一化 clamp、单关键帧动画、morph 数量不一致、拓扑过短、索引越界、像素不足 | 已验收 |
| P0-C 帧/输入/渲染状态 | `0900235` | 19 文件 +327/−47 | 首个 stencil 引用写入、缺 `InputState` 的 buffer 被跳过、panic 后帧可再排队、`FrameSkip` 单例、模型形状不符、已结束 world 拒绝步进、outlet 重新认领 | 已验收 |
| P0-D 音频/线程状态 | `44f7a23` | 19 文件 +1015/−210 | 混音上限与整帧截取、`Pulled` 与自带线程的组合被拒、synth 回绕/积压/sustain、录音的单一上限、`JoinOutcome` 四变体、线程创建失败的 typed error、executor panic 后归还 pool | 已验收 |
| P0-E 纹理行宽 | `2e8ee43` | 5 文件 +374/−4 | 对齐行宽上限、最大对齐容量、48/80/96 字节边界、真实 device 的 staged 与 queue 上传读回、超限拒绝 | 已验收 |
| P0-F 批初输入状态 | `633bfc2` | 13 文件 +1425/−743 | baseline 单测 + 7 条页面重放用例（批初状态、跨批、换页重同步等） | 已验收 |
| P0-G 多 pointer 按钮 | `50fa7ed` | 8 文件 +644/−311 | 笔按下、手指持续按压、鼠标松开不抬起笔、抬一指另一指仍按下 | 已验收 |
| P0-H 取消/失焦 pointer | `4390f9e` | 8 文件 +415/−17 | 无位置取消/松开/离开、失焦结束全部触点、失焦清修饰键 | 已验收 |
| P0-I Key/Text 去重 | `c73a746` | 7 文件 +441/−31 | 带文本的按键只报一次、无文本按键、repeat、快捷键、粘贴 | 已验收 |
| P0-J 文档级字体集合 | `c802d95` | 4 文件 +125/−8 | 一页注册的 family 另一页匹配不到、同名 family 各自保留 | 已验收 |
| P1-A 队列与订阅 | `e44a993` | 20 文件 +726/−123 | 拒绝的 job 返还、排队中的订阅无值、世界停止前的安装答 `Ended`、同 key 合并不跑 `make` 也不续 timeout | 已验收 |
| P1-B 失败原因 | `b599b75` | 34 文件 +1441/−538 | `WorldError` 到达场景/step 出口、`ToolError` 的 kind 可分支、拒绝与入队后停机分别结算 | 已验收 |
| P1-C `RunShape` | `754c3a8` | 13 文件 +472/−223 | 8 条开关组合解析、`TurnOutcome` 区分 `Suspended` 与 `Unprepared` | 已验收 |
| P1-D `WorldEndpoint` | `0ab3d7b` | 38 文件 +1792/−1297 | endpoint 排队、三种 world 的 ownership/driving、`NoHandle`/`Detached`/二次等待、setup panic 报因且不留半成品 | 已验收 |
| P2-A mesh 资源模型 | `d34701a` | 28 文件 +991/−426 | `MeshDescriptor` 的 8 类拒绝（无图元、索引/顶点缓冲过短、slot 越界、零 stride、无 morph target、位移不足、pose 短于 joints） | 已验收 |

### 验收命令与结果

在 `main` 的 `0ab3d7b` 上运行：

- `cargo fmt --all --check`：通过。
- `cargo clippy --workspace --all-targets --all-features`：0 警告。
- `cargo nextest run -p unlit3d -p unlit3dx -p unlit_wgpu -p unlit_ecs --features unlit3d/gltf,unlit3d/mcp,unlit3d/egui,unlit3d/blitz_epaint_fonts,unlit_wgpu/egui`：1204 passed，0 skipped。
- `cargo nextest run -p unlit3d --features blitz_epaint_fonts,egui,gltf,mcp --test unlit3d`：113 passed，含 Blitz 页面重放、pointer、按键全部 GPU 用例。
- `cargo nextest run -p unlit3dx --all-features`：210 passed，含 audio、physics、synth、MCP 与生命周期集成用例。
- `cargo nextest run -p unlit_wgpu --features egui`：277 passed，含 P0-E 的 staged 上传读回。
- `cargo nextest run -p unlit3d --features blitz_epaint_fonts,blitz_sc_fonts,egui,gltf,mcp --lib`：364 passed，覆盖中文 subset 字体路径。
- `cargo test -p unlit3d -p unlit3dx --doc`：unlit3d 16 passed、2 ignored；unlit3dx 0 passed、3 ignored。
- `cargo xtask line-count`：377 files，124428 lines，0 个达到 1000 行。

### 未验收项

- **wasm 与 WebGL2 档位**：`cargo xtask test-wasm` 与 `UNLIT3D_DEVICE_TIER=webgl2` 未在本机运行，因此 P0-C、P0-D、P0-F–P0-I 在 wasm 目标下的行为只经 `cargo check` 覆盖。
- **非 Linux 平台**：`target_os = "android"` 与 Windows 的 `cfg` 分支未运行，`RunShape` 在这些平台上把 ownership 定为 `Hosted` 的路径未实测。
- **真实音频设备**：本机只有 PipeWire 提供的 sink，音频用例按 `has_output_device()` 分支运行，两种分支均通过；但未在无声卡环境验证「永久静音」那条诊断路径的日志文本。
- **`blitz_system_fonts`**：只做了 `cargo check`，未运行宿主字体枚举下的排版用例。
- **P2-B–P2-D / P3 / SK**：未开始。SK-1–SK-10 的 API 纠错所需的新签名已经就位（`WorldEndpoint`、`TickOutcome`、`RunShape`、`ToolError`、`StepOutcome::Failed(WorldError)`、`MeshDescriptor`），但技能页面尚未按这些签名复核。

## 目标与执行原则

本计划把 glTF、unlit3d（不含 glTF）、unlit3dx，以及基于 Cargo Doc JSON 的公共 API 审查结果合并为可分工实施的任务，并统一登记补审与技能审查的全部实施任务。原四会话工作包和补审工作包统一采用 P0–P3 格式；技能任务另有配套文档，本计划负责全部任务的优先级、依赖与完成门槛。四份审查记录的来源如下：

- `session-3e8e90a3-efca-4b94-8101-66a3255c925a`：审查 glTF 相关代码。
- `session-86e5fd52-0e09-4b43-887e-1c31f09c5681`：审查 unlit3d 相关代码，排除 glTF。
- `session-dbf05a96-3b38-4f17-84be-550a326aeb20`：审查 unlit3dx 相关代码。
- `session-31979f4a-e9fd-46a4-880e-e825e6872591`：基于 Cargo Doc JSON 审查 unlit3d 与 unlit3dx API 设计。

目标不是逐条添加包装函数，而是：

1. 先消除正常输入即可触发的 panic、静默数据损坏、永久停帧和消息丢失。
2. 再统一错误、world 生命周期、队列和请求的语义，让失败原因与所有权边界进入类型。
3. 收紧可构造出矛盾状态的公共 API，减少由调用者、注释和约定维护的不变量。
4. 最后处理热路径分配、重复遍历、文档漂移和 API 索引工具。

实施期间遵守以下约束：

- 每个工作包先补回归测试，再改实现；不要用放宽断言或吞错来掩盖问题。
- 破坏性 API 重构可以一次完成，不保留只为兼容旧设计的薄包装。
- 运行时错误用 `thiserror` 类型；预期的用户输入、资源缺失、队列满和设备失败不得以 `String` 或无上下文日志代替。
- 缓冲区大小、顶点属性、纹理像素和 shader 布局使用已有类型或 `size_of`、`VertexFormat::size`、`ShaderLayout::SIZE` 等计算，不新增魔数。
- Rust 检查优先使用 `cargo check` / `cargo clippy`；测试使用 `cargo nextest run`，文档测试使用 `cargo test --doc`。文档和格式改动不重复跑全量测试。
- 一个代码文件不得增长到 1000 行；拆分生产逻辑与内联测试时，保持模块职责清晰。

## 快速代码核验结果

以下结论来自对当前实现的直接抽查，不仅来自四份会话摘要：

- **确认 glTF skin 的跨路径不一致**：`crates/unlit3d/src/gltf/skin.rs:47-69` 已将缺失或短 `inverseBindMatrices` 按 joint 逐项用单位矩阵补齐，且 `crates/unlit3d/src/gltf/tests/scene.rs:267-285` 将短 accessor 明确定义为合法行为；但 `crates/unlit3d/src/gltf/model.rs:159-170` 只收集实际矩阵，`crates/unlit3d/src/lit/model.rs:747` 仍按 joints 数量下标访问。因此 P0-A 的正确修复是共享「补齐到 joints 数量」策略并补回归测试，而不是无条件把短 accessor 改成错误。
- **确认 Draco morph 遗漏**：`crates/unlit3d/src/gltf/draco.rs:318-333` 只收集 `primitive.attributes`，没有收集 `primitive.targets`；这会让压缩 morph accessor 没有被重写。另一方面，`crates/unlit3d/src/gltf/draco.rs:139-165` 已把图元改写为普通三角形/普通 buffer accessor，并删除已消费的 Draco 扩展声明；这一步本身是合理的，后续应验证 `extensions_used` / `extensions_required` 与重写结果一致，而不是强行保留声明。
- **确认图像 HDR 风险**：`crates/unlit3d/src/gltf/image.rs:488-513` 对包括 `Rgba32Float` 在内的所有格式执行 `clamp(0.0, 1.0)`，且 `crates/unlit3d/src/gltf/image.rs:679-697` 的测试把该行为固化为预期；如果 glTF float HDR 要保真，应先改契约和测试，再改编码实现。
- **确认状态/队列问题**：`crates/unlit_wgpu/src/scene.rs:715-723,818-826` 用 `0` 作为 stencil 默认值，首个 `0` 会跳过实际写入；这说明缓存可能吞掉需要显式写入 `0` 的调用，但不能仅凭此断言普通 pass 的首个 draw 必然错误，仍需结合 wgpu 状态和实际 pipeline 验证；`crates/unlit3d/src/input.rs:286-299,476-493` 的文档说缺失状态时跳过，但实现 `expect`；`crates/unlit3d/src/winit/presenter.rs:630-646` 只在正常返回时清理 `pending`；`crates/unlit3dx/src/job.rs:25-70` 的 `SendError` 丢弃 `TrySendError` 携带的原始 job，和文档承诺矛盾；`crates/unlit3dx/src/channel.rs:193-204` 安装 outlet 失败时静默返回。
- **确认失败原因被压扁**：`crates/unlit3dx/src/turn.rs:18-31,158-170` 的 `StepOutcome::Failed` 不携带原因，`fail` 仅把 `String` 写入 `WorldState::Failed(Arc<str>)`；`crates/unlit3dx/src/app/headless.rs:186` 等入口还重新生成泛化字符串。因此 P1-B 的结构化错误传播是合理方向，但应保留 panic、command apply 和 collector 的阶段信息及 source，而不是只把 `Arc<str>` 换个名字。

- **已有基础机制，不应重复设计**：`crates/unlit3dx/src/turn.rs:53-95,121-146` 已共用 admission、jobs → apply → body → apply → publish，sub-world 使用不含 body 的 jobs → apply → publish；`crates/unlit3dx/src/request.rs:115-192` 已有同 key 合并、调用方 tick 内结算、超时及 world 结束后的过期处理、迟到答案丢弃；`crates/unlit3dx/src/app/options.rs:114-173` 已集中计算 transport、client pacing 和 frame budget。P1/P2 应扩展这些机制，而不是把它们列为尚不存在的能力。
- **拒绝入队与入队后结束要分开**：`crates/unlit3dx/src/mcp/bridge.rs:27-41` 已在 host 队列 Full/Closed 时调用 `command.fail(...)`；`crates/unlit3dx/src/turn.rs:179-183` 会在结束时丢弃未执行 job。需要逐路径验证响应如何完成，不能概称所有入队失败都会悬挂。

上述核验也标出审查结论中需要避免的过度修复：不要因 glTF 规范数量约束而破坏项目已经测试并记录的短 inverse-bind 容错语义；不要把 Draco 内部完成解码后的扩展删除一概视为错误。其余未在代码核验中直接展开的条目仍保留为待实施假设，实施 agent 必须按责任范围读取源码并先补回归测试，不能把会话审查结论当作免验证事实。

## 优先级与依赖总览

| 阶段 | 目的 | 依赖 | 交付门槛 |
| --- | --- | --- | --- |
| P0 | 修复立即可触发的正确性与安全问题 | 无 | 目标回归测试通过，不能引入新的 panic/静默丢失 |
| P1 | 统一队列、错误、world 形态和生命周期 | P0 的相关测试稳定 | 跨 threaded / stepped / sub-world 行为一致，失败原因可追踪 |
| P2 | 收紧资源、pass、句柄和请求 API | P1 的类型与错误模型 | 矛盾状态无法由公共构造路径产生 |
| P3 | 性能、架构、文档和 agent 工具 | P0–P2 完成或明确迁移点 | 热路径无已知重复分配，文档和生成索引可验证 |

建议按以下依赖图排程：

- `P0-A glTF skin`、`P0-B glTF 数据完整性`、`P0-C unlit3d 帧/输入`、`P0-D unlit3dx 音频/线程` 可并行。
- `P1-A Job/SendError` 必须先于 request、channel、egui 的跨层整合；`P1-B 失败原因` 先于 app/MCP 错误出口整合。
- `P1-C RunShape` 与 `P1-D WorldEndpoint` 可并行设计，但 P1-D 必须使用 P1-B 冻结后的停止/失败状态；P1-C 的运行形态结果再接入各 driver。
- `P2-A mesh 资源模型`、`P2-B pass/target 身份`、`P2-C glTF 资源 API`、`P2-D snapshot/request/time` 可并行，但都需要先冻结 P1 的错误命名和生命周期语义。
- `P3` 的性能优化和最终 API 索引在对应模块的 P0–P2 任务合并后实施，避免优化旧接口。下列 P0-E–P0-J/SK 正确性、示例和权限指引修复不受此等待条件限制。

## 补充任务的统一排程

补审来源记录见[独立来源页](supplemental-review-2026-11-24.md)；其中六项实现内容已全部迁入本计划 P0-E–P0-J，技能实施计划的十三项问题仍为配套任务。`SK-11`–`SK-13` 是本次结构、信息分层和格式复核新增的稳定性任务，不是新的 API 缺陷。审查完成、隔离探针复现或文档写入不等于实现与验收完成：一个包只有在回归测试补齐、验收命令通过并提交后才算完成。

P0-A–P0-J、P1-A–P1-D 与 P2-A 已按此门槛完成，交付记录见[实施进度](#实施进度)；P2-B–P2-D、P3-A–P3-D 与 SK-1–SK-13 待实施。下文各包的步骤仍是唯一权威说明，实施时按它执行，不要以进度表代替。

### 库实现任务总表（主任务与补审任务）

下表只汇总补审任务；P0-A–P0-D、P1–P3 的状态见[实施进度](#实施进度)与各包正文。

| 编号与详细步骤 | 阶段 | 问题与必交付改动 | 依赖与完成门槛 |
| --- | --- | --- | --- |
| [P0-E](agent-implementation-plan-2026-11-24.md#p0-e) | P0 | 纹理行宽截断破坏 copy 对齐；统一对齐行宽、容量与 pack/upload 布局 | 可独立实施；48/80/96 字节边界、最大容量加一、真实 device 的 queue/staged 上传读回均验证 |
| [P0-F](agent-implementation-plan-2026-11-24.md#p0-f) | P0 | 批末状态被当作批初状态重放；由输入拥有者提供同 tick 批初快照与重同步规则 | 与 P0-C 协调输入改动；实际 refresh → BlitzSource 链路校验事件时状态及首订阅/重建 |
| [P0-G](agent-implementation-plan-2026-11-24.md#p0-g) | P0 | 笔/触摸 payload 使用旧按钮且被鼠标覆盖；按 pointer 身份推进状态后生成事件 | 依赖 P0-F 的状态设计；同批/跨批、多设备和多触点 payload、选择与滚动均验证 |
| [P0-H](agent-implementation-plan-2026-11-24.md#p0-h) | P0 | 无位置终止与失焦遗留活动 pointer；使用最后位置结束生命周期并按顺序取消 | 与 P0-G 共用 P0-F；取消、失焦/恢复、modifier 清理无幽灵 contact |
| [P0-I](agent-implementation-plan-2026-11-24.md#p0-i) | P0 | 字符键丢 code/location/repeat；关联物理键与文本提交，适配默认编辑以避免双插入 | P0-F/G/H 状态设计稳定后实施；普通键、无 Text、repeat、快捷键、IME、粘贴均验证 |
| [P0-J](agent-implementation-plan-2026-11-24.md#p0-j) | P0 | 页面字体注册污染进程级共享集合；缓存只读基线，每文档创建独立可变集合 | 可独立实施且先于后续字体资源改动；双上下文、双页面、加载顺序与重建隔离测试通过 |

### 已决定的设计分歧

以下 17 项设计分歧已经作出唯一选择。每项仍列出未采用方案及其代价，便于实施者理解取舍；「已决定」是本计划的冻结方向，后续代码、公开 API、技能文档和测试都必须按此迁移。这里冻结的是目标语义，不表示当前代码已经实现。

#### 如何阅读这些设计分歧

这 17 项不是 17 个互不相关的小修复，而是三类选择：一是「状态究竟由谁拥有」，二是「跨线程操作成功后还能保证什么」，三是「公共 API 要不要把这些保证写进类型」。例如，修复一个错误的输入字段很容易；但如果不先决定批初状态由输入缓冲还是 Translator 拥有，修复只会在某一条事件路径上有效。又例如，给 `send` 增加一个返回值很容易；但必须先决定队列满时闭包是否归调用方所有、超时后任务是否仍可能执行，否则调用方无法安全地重试。

阅读每一项时，可以依次问四个问题：

1. 当前实现已经承诺了什么，实际又保证了什么？
2. 哪一个具体场景会把两者的差异暴露出来？
3. 方案是在修复正确性、控制资源，还是改变调用者可观察的语义？
4. 一旦选定，哪些调用方、测试和文档必须一起迁移？

这里的几个术语按以下含义理解：

- **tick / turn**：一个 world 的一次推进周期；stepped world 不会自行运行，必须由宿主继续驱动。
- **pointer**：鼠标、触笔或某一个触摸接触点；**payload** 是交给下游的事件字段。
- **collection / endpoint**：前者是字体注册与查找集合，后者是绑定某个 world 身份及通信状态的句柄。
- **背压（backpressure）**：消费者处理不过来时，让生产者等待或暂缓发布，而不是无限积压或静默丢数据。
- **newtype**：用不同的 Rust 类型包装相同的底层数值，让编译器拒绝把两种业务含义不同的值直接互换。

#### 1. P0-C：stencil 缓存起点

**现状与问题**：渲染 recorder 为了避免重复调用，会把上一次写入的 stencil reference 缓存在普通 `u32` 中；当前缓存初值是 0，所以第一次需要 reference=0 时会被误判为「已经正确」。问题不在于每个 draw 都必须机械地调用 setter，而在于 recorder 可能接手一个实际状态未知、或之前被设成非零的 pass。

**可选方案**：

- **A**：初始缓存为未知值，首次 draw 必须写入 reference，之后消除重复调用。
- **B**：明确 recorder 仅接收已重置的 pass，并将 wgpu 初始值作为缓存起点。
- **C**：不改缓存，只弱化文档。

**已决定**：采用 A。WebGPU 规范规定新建 render pass 的 `stencilReference` 初始值为 0（[规范](https://gpuweb.github.io/gpuweb/#renderstate-stencilreference)），但 recorder 不能假定自己接手的 pass 仍是该值；缓存初值改为未知，首个 draw 显式设置，之后仅对相同值去重。

**实施约束**：recorder 可以接手调用方已配置或已部分使用的 pass；验收每个 draw 的有效 reference、非零到零转换和连续录制，不把「每个 draw 都调用 setter」当作目标。

#### 2. P0-F–H：批初输入状态

**现状与问题**：一个输入批次中的事件必须从批次开始前的状态按顺序重放，而不能拿处理完整批次后的状态当作起点。比如上一批仍按住鼠标，本批依次是 `Moved → Released`；如果先把整批折叠成 buttons=0，再翻译 `Moved`，拖动末端会被错误地当成未按下。这里还涉及首批订阅、漏掉一批事件、source 重建和多个 tick 频率不同的消费者：只修一个字段覆盖点并不能解决初始化来源不一致。

**可选方案**：

- **A**：输入拥有者在 `refresh` 时把 cursor、buttons、modifiers、focus、contacts 等批初快照和 batch 一起交给 Translator；Translator 只按设备身份推进。
- **B**：Translator 自己保存跨帧状态，并由每个 `Frame` 只传事件。
- **C**：每次以 `InputState` 批末值初始化，改动最小但保留重放错误。

**已决定**：采用 A。输入拥有者在 `refresh` 时把同一 tick 的批初快照和事件批次一起交给 Translator；Translator 不再把自己的跨帧状态当作权威。

**实施约束**：一个输入 writer 只能向明确绑定的消费批次发布；不同 tick 频率的消费者各自获得批初快照。Translator 重建时从输入拥有者的当前平台状态重同步，不延续旧 Translator 的私有状态。

#### 3. P0-G：多 pointer 状态

**现状与问题**：当前 mouse、pen 和 touch 可能共用一个 buttons 状态，且 down/up payload 在状态更新前构造。因此 touch down 可能报 buttons=0，随后 touch up 才报 Primary；下一帧 mouse 快照又可能把仍在拖动的 touch 状态覆盖掉。实际后果是依赖 `buttons` 判断拖动的选择、平移和手势会中途断掉，两个设备同时操作时还会互相清状态。

**可选方案**：

- **A**：按 mouse、pen、每个 touch id 分开保存位置、按钮和生命周期，payload 使用当前 pointer 状态。
- **B**：保留一个共用 `buttons`，在每个事件前后临时修正 Primary。
- **C**：只修正 down/up 的 payload，不承诺跨帧 move 和并发设备。

**已决定**：采用 A。mouse、pen 和每个 touch id 分开保存状态；`pointer.buttons` 只表示该 pointer 的按钮，多个 pointer 的并集不伪装成单个 pointer。

**实施约束**：`pointer.buttons` 表示当前 pointer；多指继续以稳定的首个活动 contact 作为 primary，直到该 contact 结束，再按明确顺序选择下一个。

#### 4. P0-I：Key/Text 去重

**现状与问题**：平台通常会同时产生物理按键事件和文本提交事件：前者携带 code、location、repeat，后者适合处理 IME、粘贴和组合输入。当前实现丢掉普通字符的物理 keydown，又把 Text 伪造成 `Unidentified`、标准位置且 `repeat=false` 的 keydown；如果简单地把两者都放行，Blitz 默认编辑逻辑又可能把一个字符插入两次。

**可选方案**：

- **A**：输入归一化层给物理 Key 与 Text 建立关联，Translator 发出一次保留 code/location/repeat 的事件。
- **B**：在 Translator 以相邻事件、时间或字符内容猜测配对。
- **C**：修改 Blitz 默认编辑行为，使 Key 和 Text 可同时转发，由下游去重。

**已决定**：采用 A。输入归一化层显式记录物理 Key 与 Text 的关联；关联成功时只向默认编辑路径提交一次，并保留 code、location、repeat；IME、粘贴和无法关联的文本走独立 Text 路径，不靠相邻事件猜测。

**实施约束**：改变 `blitz-dom` 默认插入语义以适配归一化结果；无 Text 的按键仍产生物理 Key，IME/粘贴不伪造物理 Key，非 US 布局以平台提供的 code/location 为准。

#### 5. P0-J：字体 collection 所有权

**现状与问题**：当前进程级 `OnceLock<FontContext>` 返回共享 collection 的 clone，页面的 `@font-face` 会把可变字体注册进这个共享集合。结果是页面 A 注册的 family 可能被页面 B 匹配到，渲染结果取决于页面加载顺序；页面销毁也没有自然的全局注销边界。

**可选方案**：

- **A**：每个文档从 unshared 基线 clone 后建立自己的 shared collection；仅共享只读字体 bytes/source cache。
- **B**：进程共享 collection，通过 family 命名空间、引用计数和回收隔离文档。
- **C**：禁止页面自定义字体，保留全局系统/内置字体。

**已决定**：采用 A。每个文档从只读基线建立独立可变字体 collection；只读字体 bytes/source cache 可以共享，文档注册、fallback 和销毁状态不得共享。

**实施约束**：接受文档级可变 collection 的内存成本；所属文档的 worker 必须使用该文档 collection；销毁或重建后不得残留其他文档的注册字体。

#### 6. P1-A：发送失败时 job 的所有权

**现状与问题**：`Job` 是 `FnOnce` 闭包，不是可复制值。当前 `try_send` 在 Full/Closed 时把闭包消耗掉，但 `SendError` 只有原因，没有把闭包交还；这与 rustdoc 所说的「job comes back」不一致，Full/Closed 已能表明本次未入队；真正缺失的是未执行闭包的所有权，调用方只能重建工作，不能直接重试原闭包。

**可选方案**：

- **A**：`SendError::Full(Job)` / `Closed(Job)` 返还闭包，调用方可重试或显式丢弃。
- **B**：新增明确的 `send_lossy`，当前 `send` 继续丢弃 job。
- **C**：改用可等待的 async send，在容量可用时背压。

**已决定**：采用 A。`SendError::Full(Job)` 和 `SendError::Closed(Job)` 必须返还未入队 job；调用方可以重试、传播或显式 drop，不增加隐含丢弃 job 的发送路径。

**实施约束**：拒绝入队的 job 可安全重试；已入队但尚未收到答复的 job 不得自动重发。只有调用方显式 drop 返还的 job 才允许放弃，且文档必须说明副作用风险。

**实施约束**：拒绝入队的 job 尚未执行，和已入队后丢失响应不是同一种重试条件。`Job` 不实现当前 `SendError` 的 `Clone`、`Copy`、`Debug`、`Eq` 派生；若错误携带闭包，应拆开可复制的拒绝原因与不可复制的 payload，不能照搬整套派生。

#### 7. P1-A：订阅安装失败

**现状与问题**：`watch`/`subscribe` 先创建 reader，再把「安装 collector」作为 job 发给目标 world；入队成功便返回 reader，不等 collector（每 tick 读取目标 world 并发布数据的采集函数）实际安装。Full/Closed 已通过 `Err` 返回，此时不会向调用方交付 reader。真正的缺口在入队成功之后：若 job 在 world 停止时被丢弃，或安装时拿不到 outlet 而直接返回，调用方手里的 reader 可能一直没有数据，且无法区分「尚未安装」「安装失败」和「已安装但暂时没有值」。

**可选方案**：

- **A**：在目标 world 完成安装，通过完成回执交付 reader，不阻塞调用线程。
- **B**：返回带安装状态的 reader，读者必须处理 Pending/Failed。
- **C**：保持异步安装，借不到 outlet 时静默放弃。

**已决定**：采用 A。只有目标 world 返回安装成功回执后才交付 reader；安装失败、world 结束或 outlet 不可用都通过失败回执结算，禁止静默生成永远无数据的 reader。

**实施约束**：`watch`/`subscribe` 改为异步完成安装的 API；调用线程不得阻塞等待 stepped world，安装成功不等于第一份数据已经发布。

**时序约束**：安装回执必须异步处理。若调用线程阻塞等待，而目标 stepped world 又依赖该线程继续 step，安装永远不会发生。确认安装成功也不等于保证第一份数据已经发布。

#### 8. P1-A：同 key request 合并

**现状与问题**：request 用 key 合并尚未完成的重复请求，而不是合并多个 answer；当前第二个相同 key 的 `make` 会被消费，但不新增 job，也不重置第一个请求的 timeout。这是避免重复工作的一种语义，不是普通发送失败。问题在于调用者可能误以为每次 `make` 都会执行，或者需要知道自己是排队者还是合并者。

**可选方案**：

- **A**：返回 `Queued` / `Coalesced`，后者仍消费未执行的 `make`。
- **B**：`Coalesced(make)` 返还原始闭包，由调用方决定保留或丢弃。
- **C**：保留 `Result<(), SendError>`，仅文档说明合并。

**已决定**：采用 A。相同 key 的第二个请求返回 `Coalesced`，消费其 `make`，不新增 job，也不重置首请求 timeout；新请求入队才返回 `Queued`。

**实施约束**：合并不重置首请求 timeout；第二个 `make` 明确由合并操作消费且不执行。需要保留闭包时必须另设计泛型回收结果，不能把本决定改成隐式重复执行。

**所有权约束**：合并的第二个闭包从未执行且由合并操作消费；不采用返还原始 `F` 的方案。发送失败错误返还的装箱 `Job` 还封装了 answer sender 等状态，不能自动还原成原始 `F`。

#### 9. P1-B：失败原因传播

**现状与问题**：当前 `StepOutcome` 已能区分运行、完成、停止和失败，`WorldState::Failed(Arc<str>)` 也保留了可读失败消息；缺失的不是所有失败信息，而是失败的结构化分类与底层 source。例如加载失败和用户 job panic 都可能变成一段文字，MCP 客户端若想决定是否重试，只能解析不稳定的消息文本。source 指底层原始错误，downcast 指按它的具体 Rust 类型识别它。

**可选方案**：

- **A**：`WorldError`/阶段化 `thiserror` 从 turn 到 app/MCP 全链路传播，`WorldState::Failed` 保留可读原因和 source 摘要。
- **B**：保留 `Arc<str>`，另加 stage/code 字段，在 app 边界映射结构化错误。
- **C**：继续用 `String`，只改善日志上下文。

**已决定**：采用 A。`WorldError` 以可共享的结构化错误和阶段信息贯穿 turn、thread、app 与 MCP；MCP 对外提供稳定 error code，`WorldState` 保留可读摘要，底层 source 在可跨线程时保留。

**实施约束**：MCP 必须返回稳定 error code；`StepOutcome::Failed` 携带共享错误，`WorldState` 同时保留终态摘要；只有明确需要机器分支的底层错误才要求 downcast，其余保留 source/message 上下文。

#### 10. P1-C：运行形态

**现状与问题**：当前 `RunOptions` 已集中计算 `has_transport`、`client_paced`、`frame_budget` 和 `pacing`，但多个入口仍可能根据布尔值和 `Option` 重复组合。新增 `RunShape` 的目的不是重写现有解析，而是把「window/headless、threaded/stepped、stdio/HTTP、offscreen、pacing」的最终组合冻结成一个不可变结果。

**可选方案**：

- **A**：`RunOptions` 解析一次生成不可变 `RunShape`，入口只消费已解析的 ownership/driving/pacing/offscreen/transport。
- **B**：保留现有 `RunOptions::{client_paced,frame_budget,pacing}`，各入口调用统一 helper。
- **C**：以 builder/trait 把 window/headless/MCP 组合交给调用方。

**已决定**：采用 A。解析一次生成不可变 `RunShape`，所有入口只消费该结果；`stdio` 与 HTTP 可同时启用，不能压成互斥单选。

**实施约束**：解析阶段立即拒绝不支持的显式组合，不自动降级；平台默认值可以适配平台，但显式请求的配置不被静默改写。wasm、offscreen、interactive、stdio 和 HTTP 的组合规则由 `RunShape` 统一校验。

#### 11. P1-D：world 句柄与生命周期

**现状与问题**：目前 sender、state reader、host entity 可以被分别 clone 或传递，类型上不保证它们属于同一个 world；例如 `Pending` 用 A 的 sender 执行请求，却观察 B 的 state：B 停止可能使 A 的未完成请求被过早判为 Expired；A 停止而 B 仍运行时，则可能无法及时结算，若未配置 timeout 甚至持续等待。

**可选方案**：

- **A**：`WorldEndpoint` 封装同 world 的 sender、state 与身份，`Pending` 绑定 endpoint。
- **B**：保留可拆的 `WorldHandle`，在跨边界操作中校验 `WorldId`。
- **C**：仅写明调用方必须自行配对，不增加身份校验。

**已决定**：采用 A。使用唯一名称 `WorldEndpoint`，把同一 world 的身份、sender、state reader 和生命周期控制封装在一起；`Pending` 只接受 endpoint，不再分别接受 sender 与 state。

**实施约束**：host entity 只通过 endpoint 的受控视图访问；sub-world 也使用绑定自身 world 的 endpoint；可 clone 的只读视图不能提供重新拼装 sender/state 的构造入口。

#### 12. P1-D：join 与终止原因

**现状与问题**：线程是否已经回收，和 world 为什么停止，是两个正交问题：失败的 world 可能正常 join，正常停止的 world 也可能因超时被 detach；当前 `join_timeout` 的 bool 不能表达这些组合，无 join handle 时甚至可能返回 true。

**可选方案**：

- **A**：回收结果单独表达 Joined/Detached/TimedOut，world 成败从独立 state 或嵌套字段读取。
- **B**：返回一个包含回收结果、world 终态及原生 join panic 的完整报告。
- **C**：保留 bool，并强化「true 不代表线程已退」文档。

**已决定**：采用 A。join 只返回线程回收结果，`WorldState` 独立表达 world 终态；区分 `Joined`、`Detached`、`TimedOut`、`AlreadyJoined` 和无 handle，禁止用一个 bool 合并两类信息。

**实施约束**：超时后保留 join handle；重复 join 返回 `AlreadyJoined`，主动 detach 与无 handle 分开表示；drop 继续有限等待后 detach；原生线程 panic 不伪装成 world failure，wasm 按无线程实现返回对应回收结果。

#### 13. P2-A/P2-B：资源与 pass 身份

**现状与问题**：当前一些资源字段可以单独构造或替换，导致 mesh、pass、frame、render target 之间出现逻辑上不相容的组合；这类错误可能直到 GPU 提交才暴露。例如 `indexed=true` 却没有 index buffer，或 resize 后 target 与 attachment 尺寸没有同步更新；两者都不是单纯换一个字段名称就能解决的。

**可选方案**：

- **A**：以 `MeshDescriptor → validate → GpuMesh`、`PassId/FrameId/RenderTargetId` 等不变量类型重构。
- **B**：先保留字段，新增 validated constructors 和运行时校验，逐步隐藏字段。
- **C**：只补 debug/release 断言和文档，不改变构造 API。

**已决定**：采用 A。mesh、skin、morph、pass、frame 和 render target 使用 validated 构造与身份类型，删除能制造矛盾状态的公开字段和构造路径。

**实施约束**：接受破坏性字段删除和示例迁移；构造器负责本地不变量，跨 world/pass 入口负责归属校验，校验结果进入类型或明确错误，不以 release 断言替代。

#### 14. P2-D：snapshot/stream 读取与队列策略

**现状与问题**：两种订阅的语义本来就不同：snapshot 只保留最新值，适合姿态和布局；stream 保留每个事件，适合输入和碰撞。若统一使用一种队列，就会在「低延迟但允许丢旧值」与「不能丢历史但可能积压」之间被迫做隐含选择。例如生产者每秒产生 120 个事件，消费者每秒只处理 30 个，无界 stream 的积压会持续增长；改成只留最新值能控制内存，却可能丢掉必须配对的 down/up 或业务完成事件。

**可选方案**：

- **A**：保持 snapshot 最新值和 stream 历史队列语义，补完整读取/关闭/失败状态；有界策略显式选择。
- **B**：所有 stream 统一改为有界丢旧或丢新。
- **C**：所有 stream 使用可靠背压。

**已决定**：采用 A 的明确有界版本。snapshot 保留最新值及读取/关闭/失败状态；stream 使用每 reader 有限容量，溢出使该 reader 显式失败，不静默丢弃、不无限增长、不阻塞其他 world。

**实施约束**：stream 溢出不是正常丢失，而是可观察的 `Overflow` 终态；关闭允许读取已排队数据后报告终态；失败原因由 channel 返回并关联 world 终态。每个 reader 的容量由创建者显式配置并设上限。

#### 15. P2-D：request 超时与取消

**现状与问题**：当前 request 在调用方 tick 中结算；`Settled::Expired` 只表示等待结束，timeout 不会撤销已经排队或正在执行的 job，迟到 answer 会被丢弃。把 timeout 误认为 cancellation 会导致调用方在超时后重试，原任务和重试任务都产生副作用。

**可选方案**：

- **A**：保持 tick 内结算，区分 TimedOut/WorldStopped/WorldFailed；主动取消另设协作协议。
- **B**：保留 `Settled::Expired`，从独立 world state 诊断原因。
- **C**：timeout 立即保证目标任务不再执行。

**已决定**：采用 A，并加入协作式取消。timeout 仍按调用方 tick 结算；取消请求通过 token 传递，在排队前或执行检查点生效；不强制中断、不回滚副作用。

**实施约束**：只有确认任务未执行或在检查点提前结束，结果才可报告 `Cancelled`；已执行完或已产生副作用的任务不得伪称取消；timeout 后迟到 answer 丢弃，但不表示任务没有执行。

#### 16. P2-D：时间类型

**现状与问题**：frame delta、simulation time、wall-clock timeout、audio sample time 和 animation time 都叫「时间」，但单位、是否受暂停影响、是否允许负值并不相同。全部使用 `Duration` 虽然能统一时长单位，却仍无法阻止墙钟预算与模拟时长混用；audio sample index 也不能直接等同于时长，必须结合采样率转换。例如同样的 1 秒，暂停后的模拟时间可以不推进，墙钟却会继续走。

**可选方案**：

- **A**：按 simulation/frame/wall-clock/audio/animation 分域 newtype，入口验证有限且非负。
- **B**：保留数值类型，仅在入口验证并写清单位与时钟。
- **C**：全部统一为 `Duration`。

**已决定**：采用 A。simulation/frame/wall-clock/audio/animation 使用分域 newtype；拒绝 NaN、无穷和负值；不改变现有 tick 时间语义。

**实施约束**：timeout 只由调用方 tick 的有效 delta 扣减；暂停不扣预算，恢复后继续剩余预算；NaN、无穷和负值直接报错，不规范化为其他值。

#### 17. SK-11–13：技能文件结构

**现状与问题**：技能文档的问题不是文件数量本身，而是同一规则可能在教程、参考和 MCP 说明中各写一份，修改后出现互相矛盾；Agent 还可能要跳过很多文件才能找到权威答案。

**可选方案**：

- **A**：保留七文件，建立职责矩阵、唯一权威规则和锚点；按任务补小节。
- **B**：立即把 MCP、参考、教程拆为更多专题文件并保留兼容入口。
- **C**：合并为少数长文件。

**已决定**：采用 B。直接按任务拆分 MCP、参考、教程和排查内容；每条稳定规则只有一个权威位置，旧入口只保留兼容锚点和路由，不复制契约。

**实施约束**：每个常见任务必须在两次跳转内到达权威内容；旧锚点保留到迁移完成；Markdown 结构、链接、锚点和重复契约检查接入自动化验收。

#### 已冻结的决策记录

上述 17 项均按「采用方案、未采用方案的主要代价、公开 API 影响和测试门槛」写入本计划。实施时不得把旧的候选描述重新解释为可选路线；若代码发现无法满足某项决定，必须暂停该工作包并报告具体冲突，而不是自行降级。

### 技能与示例任务

技能纠错先反映当前实际可用的 API；P1/P2 后续改变 API 时，在对应代码提交中同步迁移并复验，不能把未来设计提前写成已实现能力。

| 编号与详细步骤 | 首次实施阶段 | 问题与必交付改动 | 依赖与完成门槛 |
| --- | --- | --- | --- |
| [SK-1](skills-review-2026-11-24.md#sk-1) | P0 配套 | 删除失效的 Frame.size 用法；按 pass/viewport 说明尺寸和相机选择 | 不等待 P2-B；双 pass 不同 aspect/相机示例可编译且行为正确；P2-B 合并时复验 |
| [SK-2](skills-review-2026-11-24.md#sk-2) | P0 配套 | 修 App::Args、RenderSetup、旧 tuple 访问和 KeyEvent.location，登记可复制片段的编译来源 | 与 SK-7 同批改多 world 片段；编译检查覆盖全部可复制片段，P1/P2 签名变更时同步迁移 |
| [SK-3](skills-review-2026-11-24.md#sk-3) | P0 配套 | egui source 挂目标 pass；同步技能、源码 doctest 与排查规则 | 可独立；除编译外，现有 GPU 测试证明面板可见，无 pass 不绘制的测试保留 |
| [SK-4](skills-review-2026-11-24.md#sk-4) | P0 配套 | 示例处理步进结束/停止/失败；承诺音频时实际拉取和消费样本 | 与 P0-D 协调；失败/结束不记成功帧；P1-B/D 改 outcome 后同步复验 |
| [SK-5](skills-review-2026-11-24.md#sk-5) | P0 纠错，P2-B 收尾 | SurfaceKey 仅用于格式/采样特化，不作为逻辑 pass identity | 先删除错误缓存规则；P2-B 合并时更新实际身份 API，同 key 双 pass 私态不串用 |
| [SK-6](skills-review-2026-11-24.md#sk-6) | P0 纠错，P1/P2 收尾 | 补通信状态表：入队不等于执行、超时非取消、无界积压与同 world 绑定 | 先描述当前限制；P1-A/B/D、P2-D 合并时同步更新，失败/重试/迟到副作用用例可据文档处理 |
| [SK-7](skills-review-2026-11-24.md#sk-7) | P0 配套 | 统一 RemoteEntity 与 remote/resolve，角色 newtype 不代替 WorldId 校验 | 与 SK-2 同批；错误 world 句柄拒绝测试；P1-D/P2-B 后续迁移仍保持来源校验 |
| [SK-8](skills-review-2026-11-24.md#sk-8) | P0 配套 | 删除更换 HOME 绕拒绝的建议；区分缺 CLI、缺权限、待认证与宿主重启 | 无代码依赖，优先修；环境说明交对应技能/实际帮助，不擅自安装或改权限 |
| [SK-9](skills-review-2026-11-24.md#sk-9) | P0 配套 | 按是否重绘、内联/落盘两个轴选择 MCP 取图工具 | 先静态修说明；可用宿主中验证 step 后直接保存既有纹理不额外渲染，无法实测则记录未验收项 |
| [SK-10](skills-review-2026-11-24.md#sk-10) | P0 配套，P0-I/F 收尾 | 说明 egui 可重入；区分强制条件与建议；校准 Blitz 输入、字体能力承诺 | 先写当前限制，不把缺陷固化为使用规范；P0-I/F 合并时同步能力与回归测试，P3-C 最终复核 |
| [SK-11](skills-review-2026-11-24.md#sk-11) | P0 配套 | 直接按稳定任务拆分入口、教程、参考、MCP 和排查页；每条规则只保留一个权威位置 | 无代码依赖；拆分页面、迁移规则、保留旧锚点并检查重复契约，不能以链接存在替代内容归属验收 |
| [SK-12](skills-review-2026-11-24.md#sk-12) | P0 配套 | 统一可编译骨架、示意片段、命令和输出示例的标记；补 Markdown 结构检查 | 与 SK-11 同批完成；检查围栏、标题、表格、链接、锚点和空白，并接入明确的 CI 或 xtask 入口 |
| [SK-13](skills-review-2026-11-24.md#sk-13) | P0 配套 | 按任务拆分高频 MCP/参考内容，保持兼容锚点并缩短 agent 检索路径 | 与 SK-11 同批完成；每个常见任务两次跳转内到达，工具契约和源码位置不重复，拆分后链接全量复验 |

### 分工、交付与关闭规则

1. 纹理任务负责人实施 P0-E；输入任务负责人共同实施 P0-C 与 P0-F/G/H/I，并先按职责拆分过长的 translator 文件；字体任务负责人独立实施 P0-J。
2. 技能整合负责人维护 SK-1–SK-10。共享的技能页面不得由多个实施 agent 无协调地同时改写；代码任务负责人提供签名、行为与测试变更，由页面负责人整合，或明确转交整个页面的编辑权。
3. 先交付无 API 重构依赖的纠错，不为等待 P3 而保留错误示例或权限建议。P1/P2 的代码负责人必须在同一工作包中更新受影响技能；P3-C 只做最终汇总检查，不能代替这些同步任务。
4. 每个任务交付时记录任务编号、修改范围、关联测试/示例、执行命令及结果、未验证的平台/feature。未执行必需行为测试的任务保留未验收状态，不以静态链接通过替代运行验证。
5. 只有原 P0–P3 与全部 P0-E–P0-J/SK 任务各自验收通过，且后续 API 改动的复验完成，才可宣告整份计划完成。禁止仅补风险说明而不修 P0-E–P0-J 的实现缺陷，或仅改技能文字而不验证可复制示例。

## P0：立即修复正确性、崩溃和静默失败

本阶段 P0-A–P0-J 全部已实施、验收并提交；逐包记录见[实施进度](#实施进度)。

<a id="p0-e"></a>

### P0-E：纹理行宽截断破坏上传对齐（高）

**状态**：已实施并提交 `2e8ee43`，验收记录见[实施进度](#实施进度)。

**问题**

[`row_elements`](../../crates/unlit_wgpu/src/texel_array.rs#L488-L519) 先以 `base = alignment / gcd(element_size, alignment)` 保证行对齐，却在第 517 行用 `.min(max_elements_per_row)` 截断到不一定是 `base` 倍数的值。例：`element_size=48`、`capacity=1_100_000`、`max_dimension=2048` 得到每行 682 个元素、宽 2046、高 1613，尺寸合法，但 `row_bytes=32736`，除以 256 余 224。

[`TexelArray::upload`](../../crates/unlit_wgpu/src/texel_array.rs#L632-L651) 把该值直接作为 `bytes_per_row`，随后 [`StagingBuffer::write_texture`](../../crates/unlit_wgpu/src/staging.rs#L216-L221) 在 CPU 侧断言失败。问题不依赖驱动；走 queue 上传可能正常，不能据此证明 staged 上传无误。

**实施步骤**

1. 在保持「行内紧密排列、行间不额外 padding」的设计下，将上限计算为 `floor(max_elements_per_row / base) * base`，只在对齐候选中选择行宽，不能最后截断到任意元素数。
2. 按实际对齐上限计算最大容量并明确拒绝超限请求；本例最大对齐行宽为 672 个元素，对应容量 `672 * 2048 = 1_376_256`。若产品要求使用纹理剩余宽度，须同时引入独立 copy pitch 与逐行 pack，不能只放宽 staging 的对齐断言。
3. 核查 [`TexelArrayLayout`](../../crates/unlit_wgpu/src/texel_array.rs#L349-L466) 的尺寸、容量、pack 与上传始终使用同一布局计算；同步说明容量受行对齐约束。

**测试与验收**

- 添加上述 48 字节复现，以及 80/96 字节等非二次幂尺寸跨越行宽截断点的测试。
- 对所有成功构造的布局检查 `row_bytes % COPY_BYTES_PER_ROW_ALIGNMENT == 0`、宽高不超限、实际容量不小于请求容量；测试最大对齐容量及其加一。
- 除纯布局测试外，在现有单一集成测试 binary 中加入真实 device 的 staged upload/readback；queue 和 staged 上传读取相同元素，不新增测试 binary。

<a id="p0-f"></a>

### P0-F：以批末状态作为批初状态重放（高）

**状态**：已实施并提交 `633bfc2`，验收记录见[实施进度](#实施进度)。

**问题**

[`InputEvents::refresh`](../../crates/unlit3d/src/input.rs#L171-L185) 先把整批事件折叠进 `InputState`；[`BlitzSource`](../../crates/unlit3d/src/blitz.rs#L231-L243) 读取的 cursor/buttons 已是批末状态，再传给 [`Translator::translate`](../../crates/unlit3d/src/blitz/events.rs#L272-L282) 作为重放初值。这个调用与 [`Frame`](../../crates/unlit3d/src/blitz/events.rs#L253-L256) 声称的「前帧末状态」相反。

例如上一批仍按住主键，当前批为 `Moved → Released`，最终 buttons 为 0；第一个 move 就被翻译成未按住，漏掉松开前最后一段拖动。反向的 `Moved → Pressed` 会过早报告按下。`Wheel → Moved` 还会让前一个 wheel 使用未来的坐标，可能命中错误的滚动容器。

**实施步骤**

1. 在输入缓冲拥有者处提供与 batch 同 tick 的批初快照，至少覆盖 cursor、mouse buttons、modifiers、focus、活动 pointer/contact；Translator 从该快照顺序推进。批末 `InputState` 保留原有用途，不伪装成批初状态。
2. 不要只把「不用覆盖 self.buttons」当作修复：首次订阅、source 重建、缺过一批事件、共享 page 去重等情形仍需要可靠的初始化与重同步规则。
3. 将 `Frame` 字段及说明改成真实时点；无事件帧和布局/scale 更新不能重置其他设备的活动按键。B/C/D 共用这一状态推进设计。

**测试与验收**

- 经真实 `refresh_input → BlitzSource` 链路测试 `Moved → Released`、`Moved → Pressed`、`Wheel → Moved`，逐个校验事件时的 buttons/坐标，而不只校验最终 checkbox 状态。
- 覆盖首批订阅、无事件帧和 source 重建；已有「同一 page 每 tick 只接收一次」测试继续通过。

<a id="p0-g"></a>

### P0-G：笔和触摸事件携带旧按钮状态，跨帧还被鼠标状态覆盖（高）

**状态**：已实施并提交 `50fa7ed`，验收记录见[实施进度](#实施进度)。

**问题**

[`pointer`](../../crates/unlit3d/src/blitz/events.rs#L394-L407) 先构造携带 `self.buttons` 的 payload，再插入/移除 Primary；[`touch`](../../crates/unlit3d/src/blitz/events.rs#L434-L464) 同样先构造 payload 再改变按钮。初始无按键时，同批笔/触摸 down 报 buttons=0，随后 up 反而报 Primary。

此外，[`translate`](../../crates/unlit3d/src/blitz/events.rs#L277) 每帧用仅含鼠标按钮的快照覆盖共用 buttons；活动触摸在下一帧的首个 move 因而仍报 0。隔离探针已复现。Blitz 的 pointermove 默认处理在 buttons 非空时才启动拖动，故这不只是 callback 字段不美观，而会影响跨帧选择/触摸滚动。

**实施步骤**

1. 将鼠标、笔和各 touch contact 的位置/按键/生命周期按 pointer 身份维护，不让鼠标快照覆盖笔或触摸；`pointer.buttons` 描述当前 pointer，而不是所有设备按键的并集。
2. down/up/cancel 必须先推进相应 pointer 的状态，再生成 payload；最后一个 contact 抬起与仍有其他 contact 时，不混淆当前 pointer 的 buttons 和活动 contacts 总表。
3. 活动 pointer 在无事件帧和跨帧移动中保持状态；以 B 的批初快照初始化/重同步。不要靠每个 move 临时补一次 Primary 来掩盖状态错位。

**测试与验收**

- 单 pointer 的 down/move/up/cancel payload 分别满足预期按钮状态；每种设备都覆盖同批和跨批两种情况。
- 两指交错、鼠标与笔/触摸同时输入，验证一个设备松开不会清掉另一个设备状态，也不会把按键串给另一个 pointer。
- 在 [`Blitz 集成测试`](../../crates/unlit3d/tests/blitz.rs) 中增加跨帧 pen selection/touch pan，并记录 DOM pointer payload，而不仅是最终 click/checkbox。

<a id="p0-h"></a>

### P0-H：取消或失焦不能可靠结束 pointer 生命周期（中）

**状态**：已实施并提交 `4390f9e`，验收记录见[实施进度](#实施进度)。

**问题**

[`pointer`](../../crates/unlit3d/src/blitz/events.rs#L389-L391) 对 `position=None` 直接返回，连 release/cancel/leave 也不处理。输入类型允许没有位置；已按下的笔收到无位置取消时不发 `PointerCancel`，状态也不释放。隔离探针确认取消被丢弃且 Primary 残留。

[`Translator::event`](../../crates/unlit3d/src/blitz/events.rs#L294-L301) 还忽略 `FocusChanged(false)`；外层 [`BlitzSource`](../../crates/unlit3d/src/blitz.rs#L271-L275) 只清文档焦点，不清 Translator 的 contacts/modifiers。于是触摸后未收 up 就失焦，旧 contact 可一直占据 primary；只发生 modifier-change 后失焦，也可能留下过期 modifier。

**实施步骤**

1. 无位置 release/cancel/leave 使用该 pointer 的最后已知位置完成终止，不得直接丢弃；若从未见过该 pointer，定义不伪造 down 的安全终止行为。
2. 失焦按事件顺序取消活动 pointer，清 contacts/buttons/modifiers 及相关拖动状态；文档 clear_focus 不能替代 pointer cancel。先处理批中事件，再得到一致的批末状态，避免批末 focused=false 提前影响同批较早事件。
3. 重新获得焦点和文档/source 重建时使用 B 的初始化规则，不继承旧文档的活动 contact。

**测试与验收**

- `Pen Pressed → Released(cancelled=true, position=None)` 确实产生取消并清状态；无位置正常 release/leave 也有定义行为。
- `Touch Started → FocusChanged(false) → FocusChanged(true) → 新 Touch Started` 中新接触没有幽灵旧 contact；modifier 失焦清理后，下一次 wheel/pointer 的 mods 不残留。

<a id="p0-i"></a>

### P0-I：字符键的物理键码、位置和重复信息丢失（中）

**状态**：已实施并提交 `c73a746`，验收记录见[实施进度](#实施进度)。

**问题**

[`key`](../../crates/unlit3d/src/blitz/events.rs#L476-L492) 丢弃所有无 command modifier 的字符 keydown；[`text`](../../crates/unlit3d/src/blitz/events.rs#L497-L510) 另造 `Code::Unidentified`、Standard location、repeat=false 的 keydown。因此字母/数字键的物理 keydown 不可见，无 Text 的字符键只剩 keyup，长按重复与 Alt+字符的物理快捷键信息也丢失；这与模块声明保留 physical code/location 的行为不一致。

不能简单删除过滤条件：锁定版本 Blitz 的 `node/text.rs::apply_keypress_event` 根据 `Key::Character` 插字，不检查 `event.text` 是否为 None；原始 Key 和 Text 各发送一次会造成双重插入。

**实施步骤**

1. 明确物理键、逻辑字符、文本提交的关联，在输入归一化层携带关联信息，或在 translator 将匹配的 Key+Text 合成为一次保留 code/location/repeat 的事件。不要依赖未记录的「字符 Key 一定紧邻一个 Text」假设。
2. 保留无 Text 的物理 keydown；粘贴/IME 提交作为独立文本路径，不冒充真实物理键。若依赖 API 无法表达 keydown 与文本分离，显式适配其默认编辑行为，必要时升级/修改依赖；仅设 `text=None` 不足以阻止插入。
3. 更新输入文档的可保证范围，非 US 布局不能把猜出的字符当作真实 logical key。

**测试与验收**

- `Key(A down) + Text(a) + Key(A up)` 仅插入一个 a，keydown/up 的 code 和 location 对应；无 Text 的 A 仍有 down。
- 覆盖 repeat、Alt+字符、command shortcut、非 US 文本、IME commit 与粘贴；保留已有文本输入去重和默认操作取消测试。

<a id="p0-j"></a>

### P0-J：页面自定义字体写入进程级共享集合（中）

**状态**：已实施并提交 `c802d95`，验收记录见[实施进度](#实施进度)。

**问题**

[`fonts()`](../../crates/unlit3d/src/blitz/fonts.rs#L37-L44) 从进程级 `OnceLock` 克隆 `FontContext`，而 [`build`](../../crates/unlit3d/src/blitz/fonts.rs#L81-L87) 已对其 collection 调用 `make_shared()`。在 `fontique 0.12.0` 中，这保证一个 clone 的修改对所有 clone 可见；`blitz-dom 0.3.0-beta.4` 的 `document.rs:1351-1388` 又将 `@font-face` 字体直接注册进该 collection。

因此开启 `blitz_epaint_fonts` 时，页面 A 的自定义字体进入其他已有/后建页面的可见集合；即使关闭系统字体，页面 B 也能匹配 A 注册的 CSS family，渲染依赖页面加载顺序。文档销毁或 markup 替换也没有将其从全局集合注销。源码链已确认；尚未执行双页面真实排版复现，不将具体像素差或内存增长量当作已测事实。

**实施步骤**

1. 缓存的模板保留为 unshared、仅含内置/系统字体的基线；每个文档先 clone 基线，再对自己的 collection 调用 `make_shared()`，使共享范围限定在该文档及其内部 worker。Fontique 当前实现会在此为每个 unshared clone 创建独立 shared store。
2. 字体 bytes 解码结果、只读 source cache 可以复用；不能为了免去注册成本而复用承载 `@font-face` 注册的全局可变集合。不要通过每次重枚举系统字体或加全局锁解决隔离问题。
3. 文档替换应获得新的文档字体集合；保留 bundled 字体、中文 fallback 和 system-font feature 的原有选择行为。

**测试与验收**

- 构造两个字体上下文：仅向 A 注册唯一 CSS family alias，B 的 family 查询不可见；分别测试 B 先建和后建，避免遗漏缓存模板污染。
- 双页面各自 `@font-face` 使用同名但不同字体；交换加载顺序、销毁/重建 A 后，B 的字体选择和布局保持不变。
- 运行 bundled Latin、中文 subset 和 system fonts 对应 feature 的现有测试；跨文档隔离不应破坏单文档内部 worker 的字体可见性。





### P0-A：统一 glTF skin 解码并消除越界 panic

**状态**：已实施并提交 `e92732d`，验收记录见[实施进度](#实施进度)。

**问题**

`crates/unlit3d/src/gltf/model.rs:139` 在缺少 `inverseBindMatrices` 时产生空 Vec，`crates/unlit3d/src/lit/model.rs:747` 随后按 joint 下标访问；`lit/model.rs:832` 的空 Vec 特判又使该状态通过校验。结果是合法的「未提供逆绑定矩阵」资产可能在帧回调中 panic。`skin_pose` 与 `to_model` 还各自实现了一套不一致的解码策略。

**责任范围**

- `crates/unlit3d/src/gltf/model.rs:139`
- `crates/unlit3d/src/gltf/skin.rs`
- `crates/unlit3d/src/lit/model.rs:747,832`
- 对应 glTF 测试目录 `crates/unlit3d/src/gltf/tests/`

**实施步骤**

1. 保留当前公开契约：缺失或短于 joints 的 `inverseBindMatrices` 按缺失项使用单位矩阵补齐；不把短 accessor 一概改成错误。对超出 joints 的矩阵明确截断或在加载阶段返回带索引和数量的结构化错误，并在文档中固定选择。
2. 抽出一个共享的、单一事实来源的 skin 解码函数；`skin_pose` 与 `model_skin` 都调用它，确保 `to_model` 也得到与 `skin_pose` 相同长度、相同补齐策略的矩阵。
3. 修正 `LitModel::new` 的校验与构造顺序，使补齐后的 joints/inverse-bind 不会通过空 Vec 状态后在 `lit/model.rs:747` 越界。
4. 检查 joint index、node index 和矩阵数量的错误上下文，优先使用类型化 id。

**测试与验收**

- 无 `inverseBindMatrices` 的合法 glTF：`skin_pose` 和 `to_model` 都得到单位矩阵，不 panic。
- inverse bind 数量为 0、1、少于 joints、多于 joints 的输入均有确定行为；少量输入不得在帧回调中 panic。
- 模型路径与文档路径对同一资产的 joint pose 结果一致。
- 新测试覆盖此前 `to_model` 没有覆盖的路径；运行 `cargo nextest run -p unlit3d`。

### P0-B：修复 glTF morph、Draco、图像和管线键的静默损坏

**状态**：已实施并提交 `27e8b18`，验收记录见[实施进度](#实施进度)。

**问题**

primitive 的 morph target 数量在不同阶段各自推导，Draco 路径遗漏 morph accessor，图像读取可能越界或静默补零，HDR 被无条件 clamp，单关键帧动画被丢弃，且 texture filtering 存在两个真相源。这些问题会把格式错误或合法数据转成静默错误，或在上传/绘制阶段才 panic。

**责任范围**

- `crates/unlit3d/src/gltf/scene.rs:111`
- `crates/unlit3d/src/gltf/skin.rs:95`
- `crates/unlit3d/src/pipeline.rs:723`
- `crates/unlit3d/src/gltf/draco.rs`
- `crates/unlit3d/src/gltf/image.rs:243`
- `crates/unlit3d/src/gltf/upload.rs:63,225`
- `crates/unlit3d/src/gltf/animation.rs:35`
- `crates/unlit3d/src/gltf/topology.rs`

**实施步骤**

1. morph target 数量必须有单一来源。对同一 mesh 的 primitive 数量不一致时，在加载验证阶段返回结构化错误；不要一处取节点级 `max`、另一处按单 primitive 断言。
2. Draco `collect` 必须收集 `primitive.targets` 的 accessor；增加 Draco + morph 的真实解码测试。
3. Draco 解码完成后可以删除已经被消费的 `KHR_draco_mesh_compression` 声明，但必须验证 primitive、`extensions_used`、`extensions_required` 和普通 accessor 的状态一致；不要用「保留声明」或「无条件删除声明」作为脱离重写状态的固定规则。
4. joints 从大整数转换到 Draco 表示时先做范围校验，超出目标类型返回错误；不得使用会静默截断的 `as`。保留底层 Draco 错误或把它作为 source error，不统一伪装成无上下文的 validation 错误。
5. 修正整型归一化在 2^24 以上的精度策略，明确支持范围；`Checked::value()` 入口改为验证后返回错误。
6. 图像像素读取同时检查起止边界；短输入必须返回错误，不得切到缓冲区末尾继续读。`EncodedMip` 不得用 `chunks_exact(...).zip(...)` 静默补零。
7. `Rgba32Float` 保留 HDR 范围，只对需要归一化的格式执行 clamp；同步修正文档和格式映射，明确 16 位 normalized 图像为何使用何种 GPU 格式。
8. `texture_filtering` 只由一个已解析的图像/材质配置决定。管线键和绑定组布局必须使用同一值，替换图像时重新验证，不允许「文档 image」和调用方 `GltfImage` 各算一遍。
9. 单关键帧 animation channel 应保留为恒定通道；与 `lit/sampling.rs` 的 single 分支建立回归测试。
10. 非仿射矩阵要么保留完整矩阵，要么在 API 文档中明确拒绝/降级；不能继续承诺 shear 会保留而实际 `Transform::from_matrix` 丢失它。
11. 拓扑模式和索引不足要返回结构化加载错误。公开上传路径不得 `panic!`，也不得将损坏图元静默变成空绘制。

**测试与验收**

- 覆盖 primitive morph 数量不一致、Draco morph、HDR float、短像素缓冲、短 mip、单关键帧、非法拓扑和越界 index。
- 所有失败包含资产/mesh/primitive/accessor 的上下文。
- glTF 模块文档中的 supported subset 与实际实现逐项一致。

### P0-C：修复 unlit3d 帧、输入和渲染状态缺陷

**状态**：已实施并提交 `0900235`，验收记录见[实施进度](#实施进度)。

**问题**

stencil reference 的 0 值可能被裸字段的默认状态吞掉：缓存会跳过首个相等的 0，但这本身不能证明普通 pass 的首帧必然错误，仍需结合 wgpu 状态和实际 pipeline 验证；输入文档承诺 skip+warn 但实现会 `expect`，帧 pending 只在成功尾部清理，多个 `FrameSkip` 实例共享状态，release 构建又缺少关键配套校验。这些状态错误可能表现为需要显式写入 0 的 draw 未满足、panic 后永久停帧或 release-only 越界。

**责任范围**

- `crates/unlit_wgpu/src/scene.rs:715,820`
- `crates/unlit3d/src/input.rs:280-292,476`
- `crates/unlit3d/src/winit/presenter.rs:630`
- `crates/unlit3d/src/frame.rs:101`
- `crates/unlit3d/src/lit/model.rs:747` 及 `evaluate_model` 配套校验
- `crates/unlit3dx/src/world.rs:370,386,470,496`
- `crates/unlit3dx/src/channel.rs:194`

**实施步骤**

1. 用 recording spy 验证首次 0、非零 → 0、重复值及复用已有 pass 的状态。若 recorder 不保证独占新建 pass，优先用 `Option<u32>` 区分未知与已写入 0；若选择依赖初始值，必须在入口明确并验证此前提。将「set on every draw」改为每个 draw 的有效状态保证，允许不改变行为的重复调用消除。
2. `refresh_input` 与 `dispatch_input` 按文档选择一种策略：缺少 `InputState` 时返回结构化错误，或记录 warning 后跳过；禁止文档写 skip 而实现 `expect`。
3. `DrawFrame::apply` 用 drop guard 清理 `pending`，使 `pre_dispatch` 或 `dispatch_frame` panic 后仍能再次 `queue_draw`；增加 panic 后恢复测试。
4. `FrameSkip` 只允许 singleton，或将状态放入唯一组件；修复多个实例互相 set/reset 的语义冲突。
5. `evaluate_model` 的配套关系在 release 也必须检查，不能依赖 `debug_assert_eq!` 后静默越界。
6. 统一 `StepError::Unavailable` 与 `step_stepped_world`、`with_stepped_world*` 的行为；停止后的 stepped world 必须在所有入口返回相同状态。
7. `channel::install` 借不到 outlet 时不得把已返回的 reader 变成永远无数据；返回安装错误，或仅在注册成功后交付 reader。

**测试与验收**

- stencil 首个 0 的实际写入/缓存、非零 → 0、已有 pass 状态，以及输入状态缺失、帧回调 panic 后继续绘制、多个 FrameSkip、release 构建的 model mismatch、停止 stepped world、订阅安装失败均有针对性测试；不把普通 pass 首个 draw 必错作为未验证前提。
- `cargo nextest run -p unlit3d -p unlit3dx` 通过。

### P0-D：修复 unlit3dx 运行时的明确行为错误

**状态**：已实施并提交 `44f7a23`，验收记录见[实施进度](#实施进度)。

**问题**

unlit3dx 的 headless 音频上限和声道切片存在边界错误，默认 Pulled audio 组合可能永久静音，physics、synth、录音和 capture 的状态语义不一致。detached/join 的结果表达不完整，executor panic 存在 pool 所有权恢复风险；MCP 已显式处理 host 队列 Full/Closed，仍需验证已入队后 world 停止、panic 或 job 被丢弃时，响应是否以失败或取消完成，不能直接断言这些路径均会永久悬挂。

**责任范围**

- `crates/unlit3dx/src/app/headless.rs:168-199`
- `crates/unlit3dx/src/audio.rs`
- `crates/unlit3dx/src/physics.rs`
- `crates/unlit3dx/src/synth.rs`
- `crates/unlit3dx/src/record.rs`
- `crates/unlit3dx/src/app/interactive.rs`
- `crates/unlit3dx/src/thread.rs`
- `crates/unlit3dx/src/executor.rs`
- `crates/unlit3dx/src/mcp/bridge.rs`

**实施步骤**

1. 修正 headless mix 上限：有限帧数必须受上限保护，无限运行才使用有界的 `UNBOUNDED_MIX_SECONDS` 策略；限制和警告必须一致。
2. 音频样本截取按完整声道帧对齐，`AUDIO_CHANNELS` 不得被奇数 `room` 切开。
3. `AudioConfig::output = Pulled` 与 threaded audio 的默认组合必须在构造时拒绝，或改成能实际填充的默认；不能只 warning 后永久静音。
4. 统一 physics self-driven 与 stepped 对零债务的 pacing；文档声称相同就必须共享同一策略函数。
5. 修复 synth 回绕空转、积压事件 delay 被全部压成 0、sustain CC 64 未处理的问题。
6. 录音未开始时 `is_full`、`frame_count`、`seconds` 统一返回同一语义；不要由 `format.is_some()` 隐式决定三个不同答案。
7. interactive capture 使用已计算的本帧 index，不依赖 `advance` 的内部赋值顺序。
8. `join_timeout` 在 detached 状态不得返回「已完成」；`is_finished` 不得保留死分支；线程创建失败返回 typed error。
9. executor 任务 panic 时恢复 pool 所有权；分别测试 MCP 拒绝入队、入队后停机、执行 panic、响应 sender 被 drop 的路径。调用方必须收到可识别的失败或取消，不得只因 job 被销毁而留下未完成请求；已有 `command.fail` 分支保持覆盖。

**测试与验收**

- 覆盖超大有限帧数、奇数样本房间、默认 audio、零债务、synth loop point、sustain、未开始录音、capture frame、detach/join、任务 panic 和 MCP job drop。
- 错误日志包含原始失败阶段；音频设备初始化不能把所有失败折成一个 `None`。

## P1：统一队列、错误和 world 生命周期

本阶段 P1-A–P1-D 全部已实施、验收并提交；逐包记录见[实施进度](#实施进度)。

### P1-A：让发送失败返还 job，修复所有权语义

**状态**：已实施并提交 `e44a993`，验收记录见[实施进度](#实施进度)。

**问题**

bounded 队列满或关闭时，当前 `SendError` 会丢弃 `FnOnce` job，但 API 文档承诺返还；订阅异步安装失败还可能留下永远无数据的 reader。`Pending::request` 的同 key 合并则是已有的明确行为：不发送第二个 job，保留首个请求及 timeout，第二个 `make` 被销毁。本计划把拒绝入队、订阅安装和同 key 合并分别规定为可观察的结果，不能把合并本身误判为注册项丢失。

**责任范围**

- `crates/unlit3dx/src/job.rs`
- `crates/unlit3dx/src/handle.rs`
- `crates/unlit3dx/src/channel.rs`
- `crates/unlit3dx/src/request.rs`
- `crates/unlit3dx/src/egui.rs`
- 所有调用 `JobSender::send` 的模块

**设计决定**

将 `SendError` 固定为携带原始 job 的 `Full(Job)` / `Closed(Job)`。文档写的是「hands the job back」，签名必须兑现。区分：

- `try_send`：立即成功或返回带 job 的失败；
- 放弃重试：调用方显式丢弃返回的 job；不增加会隐式吞掉 job 的 `send_lossy` API；
- 等待发送只通过显式、可取消的 await API 提供，不因队列满而默认阻塞其他 world。

**实施步骤**

1. 先改错误类型和所有调用点：底层返还 Full/Closed 中的 job；调用方明确选择重试、传播或丢弃，不让错误转换偷偷消费 closure。`Job` 是不实现 `Clone`/`Copy`/`Debug`/`Eq` 的 `FnOnce` trait object，不能保留当前错误类型的整套派生；区分可复制的拒绝原因与不可复制的 payload，按需设计 Debug，而非要求闭包实现这些 trait。
2. `channel::install` 在 outlet 借用失败时返回带原因的安装失败；成功入队但 world 停止、job drop 或安装失败时，安装回执必须结算为失败，不能先交 reader 后丢订阅。
3. 固定同 key 合并返回 `Queued` / `Coalesced`：`Coalesced` 消费第二个 `make`，不新增 job、不重置首请求 answer/timeout，并测试第二个闭包不执行。发送失败只返还装箱后的 `Job`，不承诺把它还原为原始 `F`。
4. 删除 `egui::publish` 为了重试而做的常态深拷贝，改用返还的 job/明确拒绝项。
5. 增加队列满、队列关闭、订阅安装竞争、请求合并和重试测试。

### P1-B：统一失败原因和 app 错误类型

**状态**：已实施并提交 `b599b75`，验收记录见[实施进度](#实施进度)。

**问题**

当前 `WorldState::Failed(Arc<str>)` 已能观察失败消息，但 `StepOutcome::Failed` 不携原因，部分 app 分支重新生成泛化字符串；底层 `thiserror` 与 app 层 `String` 并存。缺口是阶段、错误类型和 source 不能稳定贯穿 world/app/MCP，而不是完全没有失败观测；需要分别定义 world failure、单次操作失败、停止和超时的结果。

**责任范围**

- `crates/unlit3dx/src/turn.rs`
- `crates/unlit3dx/src/thread.rs`
- `crates/unlit3dx/src/app/{headless,serve,window_frame,interactive,offscreen}.rs`
- `crates/unlit3dx/src/gpu.rs`
- `crates/unlit3d/src/mcp/host.rs`
- `crates/unlit_ecs/src/mcp/tools.rs`

**设计决定**

1. `StepOutcome::Failed` 携带结构化 `WorldError`，其中保留可共享的 source error；只有在错误本身确实不可结构化时才使用 `Arc<str>` 作为消息字段。`WorldState::Failed` 的真实原因必须能到达 headless、serve、window frame 和 MCP。
2. app 层用 `thiserror` 枚举，不再使用 `Result<_, String>`；底层错误通过 `#[source]` 保留。
3. 区分「世界失败」「当前操作失败」「用户停止」「请求超时」，不要把一次退出请求失败升级为 world failed，也不要一边静默忽略一边让另一入口致命。
4. 盘点公共枚举的扩展承诺：现有 `StepOutcome` 已有 `#[non_exhaustive]`；其余公共枚举按实际扩展需求标记，不机械重复添加。项目不以维持旧 API 为优先，不为避免破坏性变化额外保留无用包装。

**验收**

- 构造一个带具体错误信息的 world failure，四种 app 入口都能收到同一根因。
- MCP 返回可按错误类型分支的结构化数据，而不是只能匹配字符串。
- `cargo test --doc` 不产生未解析链接。

### P1-C：引入 `RunShape`，统一运行形态

**状态**：已实施并提交 `754c3a8`，验收记录见[实施进度](#实施进度)。

**问题**

`RunOptions` 已提供 `has_transport`、`client_paced`、`frame_budget`、`pacing` 等统一推导；入口和 feature/cfg 仍有组合这些结果及 offscreen/presenter 条件的逻辑。已决定用 `RunShape` 把最终运行组合冻结成一个不可变结果，不能继续让入口自行组合布尔值和 `Option`。

**责任范围**

- `crates/unlit3dx/src/app/options.rs:129-131`
- `crates/unlit3dx/src/app/windowed.rs:179-186`
- `crates/unlit3dx/src/app/serve.rs`
- `crates/unlit3dx/src/app/headless.rs`
- `crates/unlit3dx/src/app/interactive.rs`
- presenter/offscreen 初始化调用链

**设计决定**

将命令行输入与解析后的运行形态分开：

```rust
struct RunShape {
    ownership: WorldOwnership,
    driving: DrivingMode,
    paced: bool,
    offscreen: bool,
    transports: TransportConfig,
}
```

这是目标 API 结构，当前代码尚未实现。`TransportConfig` 必须保留 stdio 与 HTTP 可同时启用的现有组合，不能以单个 `Option<TransportKind>` 排除合法输入。`run` 在解析后只计算一次 `RunShape`，其余模块只消费该结果。

**实施步骤**

1. 列出所有现有推导及 feature/平台条件，作为 `RunShape` 的解析测试输入。
2. 让 windowed/headless/serve/interactive 接收同一个 `RunShape`，删除入口重复推导。
3. 统一 presenter 缺失、多个 presenter、offscreen 和 transport 的错误。
4. 为每种 feature 组合添加解析测试，避免 wasm/android/mcp-http 分支重新分叉。

### P1-D：统一 world endpoint、驱动阶段和停止语义

**状态**：已实施并提交 `0ab3d7b`，验收记录见[实施进度](#实施进度)。

**问题**

`WorldHandle` 的公开 entity、sender、state 可被独立替换，`Pending` 也分别接受 state 与 sender，缺少同 world 绑定保证。现有 `WorldRunner` 和 `turn` 已共享驱动基础，但 ownership/driving 的公共表达仍可整理；`join_timeout() -> bool` 在没有 join handle 时直接返回 true，不能证明线程已退出。线程是否 join 与 `WorldState` 的停止/失败原因是两个维度，不能靠一个 bool 或单一互斥枚举替代。

**责任范围**

- `crates/unlit3dx/src/handle.rs`
- `crates/unlit3dx/src/world.rs`
- `crates/unlit3dx/src/thread.rs`
- `crates/unlit3dx/src/turn.rs`
- `crates/unlit3dx/src/host.rs`

**设计决定**

把 world 的身份、command sender、state reader、生命周期状态封装到一个不可拆分的 `WorldEndpoint`。同时把 ownership 与 driving 正交化：

- ownership：`HostOwned` / `ThreadOwned`；
- driving：`Continuous` / `Stepped`。

保留现有共享 turn 顺序：admit → 处理本次开始时已排队的 jobs → apply → body → apply → publish；sub-world 的 `drain_jobs` 不运行 body，按 jobs → apply → publish。input、local tasks、user tick 属于各 body 的内部次序，需另行列出并验证，不把这些内部步骤误写成当前通用 turn 已有的独立阶段。

**实施步骤**

1. `WorldHandle` 去除可独立替换的公开 `entity`、`sender`、`state` 字段；提供明确的只读 state、command、control 视图。
2. `Pending` 绑定同一个 `WorldEndpoint`，调用方不再同时传入可能来自不同 world 的 sender 和 state reader。
3. `JoinOutcome` 固定表达 `Joined` / `Detached` / `TimedOut` / `AlreadyJoined` / `NoHandle`；world 终止原因继续由独立 `WorldState` 表达，线程原生 join panic 不得冒充 world failure。区分主动 detach、已经 join 和从未持有 handle；timeout 后保留 join 能力。
4. `WorldRunner` 的 tick 不再用裸 bool，统一为 `TickOutcome`；初始化错误返回 typed error。
5. 为 threaded、stepped、sub-world 写同一套生命周期状态表和集成测试。

## P2：收紧公共 API 不变量

P2 阶段进行中：P2-A 已完成，P2-B–P2-D 未开始。

### P2-A：重构 mesh / skin / morph 资源边界

**状态**：已实施并提交。

**问题**

mesh 描述与 GPU 资源混在同一组可公开构造的字段中，`indexed`、index buffer、count、skin pose 和 morph binding 可以互相矛盾；关键校验只在 draw 前或 debug 配置生效，错误因此延迟到 GPU 越界、空绘制或难以诊断的渲染异常。

**责任范围**

- `crates/unlit3d/src/mesh.rs`
- `crates/unlit3d/src/components.rs`
- `crates/unlit3d/src/lit.rs`
- `crates/unlit3d/src/lit/bundle.rs`
- `crates/unlit3d/src/lit/model.rs`

**目标模型**

将描述数据和已验证 GPU 资源分开：

```text
MeshDescriptor -> validate() -> GpuMesh
```

用 `MeshIndexing::NonIndexed { vertex_count }` / `Indexed { index_buffer, index_format, index_count }` 代替互相矛盾的 `indexed`、`index_buffer`、`count` 字段。skinning/morphing 的 binding、pose 和 mesh 兼容性在构造实例时验证，不把 GPU 越界风险留给 draw 阶段。

**验收**

- indexed 无 buffer、有 buffer 但 indexed=false、count 与 allocation 不一致等状态无法由公共构造器生成。
- `SkinPose` 长度、joint 最高索引、inverse bind 数量和 pipeline 能力在 `SkinnedMeshInstance::new` 或等价入口验证并返回错误。
- 现有示例与测试迁移后不再依赖公开字段直接拼状态。

### P2-B：重构 pass、target 和 frame 身份

**问题**

pass、frame、surface/render target 和 attachment 尺寸由多个字段重复表达，`FramePass` 还重复持有 target size；跨 world/pass/source 的实体和资源句柄缺乏归属标识，resize 或多逻辑 pass 场景容易出现状态错配。

**责任范围**

- `crates/unlit3d/src/pass.rs`
- `crates/unlit3d/src/attachments.rs`
- `crates/unlit3d/src/source.rs`
- `crates/unlit3d/src/frame.rs`
- `crates/unlit3d/src/renderer.rs`

**设计决定**

1. `PassInfo` 明确区分 `PassId`、`FrameId`、`RenderTargetId`，不要用 `surface` 猜逻辑 pass。
2. `FramePass` 不重复持有可变 `target_size`；尺寸由唯一 `RenderTarget` 所有，resize 走统一入口。
3. `FrameAttachments::new` 改为描述结构，消除相邻 bool `with_depth` / `persistent_depth`。
4. `FrameSource` 提供分层扩展点：高层 `SceneSource::build(&SceneBuildContext, &mut Scene)`，低层 raw encoder API 明确生命周期和兼容性承诺。
5. `Entity`、`ResHandle` 跨 world/pass/source 边界时使用带语义的包装类型；至少在跨边界入口携带所属 world 或资源类型。

**验收**

- 同一 target 上两个逻辑 pass 可以独立缓存状态。
- resize 后不存在 pass、attachment、view 三者尺寸不一致的可见状态。
- 自定义 source 有文档化的调用时序和 frame/pass identity 测试。

### P2-C：收紧 glTF 资源与错误 API

**问题**

类型化 id 仍可直接构造越界，`GltfResources` 的多个公开 Vec 依赖调用方维护索引对齐，加载流程要求手工串联多个阶段；skin、animation 和 sampler 还存在重复解码/查询，MCP 路径会把结构化错误压成字符串。

**责任范围**

- `crates/unlit3d/src/gltf.rs`
- `crates/unlit3d/src/gltf/upload.rs`
- `crates/unlit3d/src/gltf/primitive.rs`
- `crates/unlit3d/src/gltf/model.rs`
- `crates/unlit3d/src/gltf/playback.rs`
- `crates/unlit3d/src/gltf/mcp/host.rs`（若路径存在则按实际模块调整）

**实施步骤**

1. 类型化 id 的构造和访问改为可验证路径；越界访问返回 `Result`/`Option`，公开方法补齐 `# Panics` 或移除 panic。
2. `gltf::Error` 加 `#[non_exhaustive]`，错误字段使用 `NodeId`、`MeshId`、`PrimitiveId` 等类型化 id；拆分「material 未上传」与「没有 base color 纹理」。
3. `GltfResources` 不再暴露可任意重排的三个公开 Vec。使用私有字段和构造器维护 index 对齐；删除 `uploaded_primitive` 的线性扫描兜底。
4. 将 `LitGltf::load` → `insert_resources` → `to_model` 的手工串联收进一个拥有资源关系的加载结果，或返回带生命周期证明的 `GltfAsset`。`mip_chain` 不要只支持消费式 builder。
5. 合并 skin、animation 的两套解码入口和 duration 计算；`to_model` 支持按需选择 clip，避免无条件转换所有动画并深拷贝关键帧。
6. 播放时钟提供不需要调用方每帧二次 `evaluate` 的明确模式，区分自动时钟与手动采样。
7. sampler/device 查询缓存提升到资源上传会话或 world 生命周期，避免逐 primitive 重算。
8. MCP 保留结构化 glTF 错误；JSON 输入先处理 BOM，再决定 JSON/base64。

### P2-D：统一 snapshot、stream、request 和时间类型

**问题**

snapshot 和 stream 已提供最新值/历史队列，但未完整表达读取、关闭、失败及背压状态。request 已在调用方 tick 内结算，timeout 和 world 停止/失败均为 `Settled::Expired`；当前 timeout 只结束等待，不取消目标 job，不能称为已有 cancellation。后续扩展应保留这些基础语义，再明确新增结果和主动取消。多个子系统使用裸 `f32` 表达不同时间域，需校验 NaN、负值及暂停行为。

**责任范围**

- `crates/unlit3dx/src/channel.rs`
- `crates/unlit3dx/src/request.rs`
- `crates/unlit3dx/src/app/options.rs`
- 音频、physics、synth、animation 使用时间值的模块

**设计决定**

1. snapshot 固定提供 `Uninitialized`、`Unchanged`、`Updated`、`Closed`、`Failed` 状态，并保留 `latest()` 作为快捷 API。
2. stream 固定为每 reader 有限容量；溢出返回 `Overflow` 并终止该 reader，不丢弃、不无限增长、不阻塞其他 world；关闭时先 drain 已排队数据再返回终态。
3. request 结果区分 `Answered`、`TimedOut`、`WorldStopped`、`WorldFailed` 和已确认的 `Cancelled`；timeout 不是 cancellation。主动取消固定使用协作式 token：排队前或执行检查点可结束任务，不强制中断、不回滚副作用；同 key 合并遵循 P1-A 的 `Queued` / `Coalesced` 语义。
4. 用轻量 newtype 区分 frame delta、simulation delta/time、wall-clock timeout、audio sample time、animation time，并规定 NaN、负数、暂停和超时的时间域。

## P3：性能、架构、文档与 agent 工具

本阶段尚未开始。

### P3-A：热路径分配和重复遍历

**问题**

渲染、输入、ECS、request、capture 和音频路径存在每帧临时分配、重复查询、重复遍历及潜在 O(n²) 匹配；这些开销会在 P0/P1 的语义修复前被掩盖，也不能仅凭代码外观证明优化有效。

按模块批处理，不在 P0/P1/P2 API 未稳定前优化：

- `crates/unlit3d/src/egui.rs:511`、`crates/unlit_wgpu/src/scene.rs:565`、`crates/unlit_ecs/src/scope.rs:105`：把 scratch Vec/stack 放入长期对象复用。
- `crates/unlit3d/src/input.rs:476`：无订阅时不复制整个 input batch；必要时用可复用缓冲。
- `crates/unlit3d/src/renderer.rs:542` 与 scene build/record：合并可达性和 placement 的重复遍历。
- `GpuMesh` 查询、light describe/pack、egui InputEvents 查询：在同一阶段缓存结果。
- `crates/unlit_wgpu/src/resources/graph.rs:336`：将重复全图 recount 改为增量 dirty 维护，证明复杂度不退化。
- `crates/unlit3dx/src/request.rs:172`：避免 `position + remove` 的 O(n²) 匹配。
- `crates/unlit3dx/src/app/capture.rs`：目录创建只做一次，排序后的 frames 统一二分查找。
- FLAC 写入避免先收集完整 `Vec<i32>`；physics velocity、synth queue 使用复用和有序结构。

验收必须包含基准或至少分配计数/复杂度回归，不能只凭代码外观判断优化有效。

### P3-B：消除重复真相源和魔数

**问题**

binding 数量、feature 数量、帧/物理常量、排序规则、模块 key 和 bundle singleton key 在多处重复声明或依赖字面量；任一处变更都可能只更新部分消费者，形成 shader/layout、调度顺序或初始化行为漂移。

1. `MAX_GLOBAL_BINDINGS` 与 `ArrayVec<..., 12>` 从同一 shader/layout 描述派生。
2. `FEATURE_COUNT`、variant 数组和 WESL feature 列表由单一声明生成或编译期校验。
3. mesh pool 的 1024、`bounds.rs:113` 的阈值、placeholder `LitVariant` 等改为命名常量或类型计算，并记录语义。
4. 模块名字符串改为集中常量或类型化注册 key。
5. 合并 `FRAME_SECONDS` / `FIXED_FRAME_SECONDS`、`PHYSICS_HZ` / `DEFAULT_PHYSICS_HZ` 等重复常量。
6. 统一排序策略：frame dispatch 与 `Ordering::resolve` 使用相同 tie-break 规则。
7. `DynamicBundle` 支持一个 bundle 声明多个 singleton key，或引入 `WorldTemplate`，消除初始化序列的重复散落。

### P3-C：文档、测试和文件拆分

**问题**

多处 rustdoc 与实现行为已经漂移，部分长文件接近 1000 行，测试覆盖不均且未覆盖的模块容易被误认为已验证；如果不先修正契约和拆分职责，后续 agent 会继续依据过时文档实施。

1. 修正以下已确认漂移：
   - `crates/unlit3d/src/input.rs:280-292` 的 skip+warn 与 `expect`；
   - `crates/unlit_wgpu/src/scene.rs:33` 的 stencil「every draw」；
   - `crates/unlit3dx/src/thread.rs:291` 的 channel closed 注释；
   - `crates/unlit3dx/src/handle.rs:26-29` 的 send 返还 job 承诺；
   - `crates/unlit3dx/src/audio.rs` 的 `add_stereo` 多声道说明；
   - `crates/unlit3d/src/renderer.rs:272` 的 graph panic 说明；
   - `crates/unlit3d/src/components.rs` 的 `WorldId` 顺序措辞。
2. 为 `Transform::from_matrix` 增加镜像矩阵、零轴、shear 约定测试；若改用 glam，则删除重复手写分解。
3. 重写 glTF 模块长文档，只写当前支持的格式、错误、所有权和时序；英文与中文 README 同步。
4. 将 `gltf/image.rs`、`lit/model.rs`、`unlit3dx/src/world.rs`、`mcp/host.rs` 等接近 1000 行的文件按生产逻辑/测试或职责拆分。
5. 清理 `#[allow]`，改用有理由的 `#[expect]`。维护实现审查覆盖清单：`session-86e5fd52-0e09-4b43-887e-1c31f09c5681` 原报告未逐行细读的 [`blitz/events.rs`](../../crates/unlit3d/src/blitz/events.rs)、[`blitz/fonts.rs`](../../crates/unlit3d/src/blitz/fonts.rs) 和 [`texel_array.rs`](../../crates/unlit_wgpu/src/texel_array.rs) 已完成正文及关键调用链补审，发现与实施步骤单独记录在[补审及实施计划](supplemental-review-2026-11-24.md)。这不代表已执行完整 GPU/浏览器测试。原报告另外列出的 unlit3dx app 已由 `session-dbf05a96-3b38-4f17-84be-550a326aeb20` 的全源码报告覆盖；ECS MCP host、wgpu light 和两个未指名的 tests.rs 仍标记为「尚未确认完整实现审查覆盖」。不要用 Cargo Doc JSON 的 API 审查替代函数实现审查。
6. 若实施中确需绕开驱动或后端 bug，在触发问题的位置实施绕过，将症状、成因、绕法与代价同步记录到 [`WORKAROUND.md`](../../WORKAROUND.md) 与 [`WORKAROUND.zh-CN.md`](../../WORKAROUND.zh-CN.md)，并在绕过代码旁留注释指向对应条目。普通逻辑缺陷应直接修复，不作为绕过记录；没有新增或变更的驱动/后端绕过时，无需修改这两份文档。

### P3-D：加入 Cargo Doc JSON agent API 索引

**问题**

Cargo Doc JSON 能提供大量 API、签名和源码 span 信息，但当前没有稳定的 agent 查询入口；直接依赖 nightly、未知 `format_version` 或把 synthetic/blanket impl 当作主要 API，会产生不可复现或误导性的索引。

**责任范围**

- `xtask` 子命令和 README/TESTING 文档
- 生成物只放 `target/`，不提交构建产物

**实施步骤**

1. 增加 `cargo xtask api-index`：固定 nightly/toolchain 版本，执行 `cargo +nightly doc --workspace --no-deps -Z unstable-options --output-format json`。
2. 解析 `format_version`，若与工具支持版本不匹配则明确失败；不要静默解析未知 schema。
3. 剥掉 synthetic/blanket impl 后输出 `target/api-index.tsv` 或等价结构化索引，至少包含：完整路径、签名、源文件与行号、文档首行、可见性、弃用信息。
4. 文档说明 JSON 不含函数体，需按 `span` 回到源码；`--document-private-items` 仅在需要实现索引的模式启用。
5. 提供增量运行和 `--all-features` 选项，记录 toolchain 与 `format_version`。
6. 为 agent 技能文档补充 API 索引使用方式；代码改动触及渲染管线、光照、附件、帧输入、MCP 或多 world 时同步更新 `.agents/skills/unlit3d/` 对应页面。

**验收**

- 新环境能按文档生成 JSON 与索引；稳定版不误报为支持 `-Z`，nightly 缺失时给出可操作错误。
- 索引路径可定位到实际源码行，且不把 synthetic impl 当作主要 API。
- toolchain/schema 变更有失败提示和更新步骤。

## 集成验收清单

每个工作包合并前：

1. `cargo fmt --check`。
2. 受影响 crate 的 `cargo check`；需要 lint 时运行 `cargo clippy --all-targets --all-features -- -D warnings`，按项目现有配置处理预期 lint。
3. 改变实现时运行受影响 crate 的 `cargo nextest run`；修改 rustdoc 可编译片段时运行对应 `cargo test --doc`。仅修订实施计划等 Markdown 时检查内容、链接和格式，不要求编译或全量运行 Rust 测试。
4. glTF、render、threaded/stepped、audio、MCP 的新增回归测试必须在对应 feature/平台条件下运行，不能只跑默认 feature。
5. 检查失败日志是否保留 source error、world、entity/resource、frame/pass、mesh/primitive 等上下文。
6. 检查是否引入新的裸 `String` 错误、`expect`、`panic!`、硬编码尺寸/数量、每帧分配或重复真相源。
7. 检查公共 API 的字段、枚举、文档、prelude 导出和 `.agents/skills/unlit3d/` 是否同步；删除不再有意义的薄 wrapper/getter/setter。
8. 最终运行 `cargo xtask line-count`，确认没有生产文件达到 1000 行；若环境不允许运行，使用等价行数检查并在交付记录中说明。
9. 核对 P0-E–P0-J、SK-1–SK-13 的交付记录与依赖复验。纹理上传、逐事件输入、跨文档字体、示例编译/实际绘制与 MCP 取图分别验证；不以链接存在、noop device 或审查探针替代正式行为测试。

## 建议的提交边界

- `fix(gltf): Validate skin and asset data before model conversion`
- `fix(render): Restore frame state and input error semantics`
- `fix(runtime): Preserve jobs and failure causes across world boundaries`
- `refactor(runtime): Model run shape and world endpoint explicitly`
- `refactor(api): Make mesh, pass, and request invariants explicit`
- `perf(render): Reuse frame scratch buffers and remove duplicate queries`
- `docs(api): Add rustdoc JSON index workflow and refresh contracts`

每个提交只包含一个工作包及其测试；不要把未相关的格式化、文档重排或性能改动混入正确性修复。

已交付的 14 个包按包各自提交，未按上面的分组合并：逐包提交让每个包可以单独回退，代价是提交数多于上表；具体提交见[实施进度](#实施进度)。
