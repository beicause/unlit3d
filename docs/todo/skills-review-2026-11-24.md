# unlit3d 技能文档审查与实施计划

## 范围与结论

完整通读 [技能入口](../../.agents/skills/unlit3d/SKILL.md) 及其六份附件，对照当前源码、锁定依赖和已有测试核验关键规则。目录的「短入口 + 按任务路由 + 专题附件」结构方向合理，但不是无需整理：本次补充发现入口、教程、参考和 MCP 手册存在职责重叠、权威来源不够明确、代码片段分类和检索层级不统一的问题。主要 API 错误与结构问题分别列为 S1–S10、S11–S13，均需实施。

本文件的十三项发现全部纳入[主计划的统一排程](agent-implementation-plan-2026-11-24.md#补充任务的统一排程)，`SK-1`–`SK-13` 分别对应下文 S1–S13。实施与本机验收见[交付记录](#交付记录)；下文的问题描述保留审查时的状态。

## 分工与交付要求

技能整合负责人统一维护以下页面；生产代码负责人提供其工作包的签名、行为和测试变化，不无协调地并发覆盖同一页面。

| 任务 | 负责修改的页面或源码 | 依赖及交付 |
| --- | --- | --- |
| SK-1 | [入口](../../.agents/skills/unlit3d/SKILL.md)、[场景](../../.agents/skills/unlit3d/SCENE.md)、[索引](../../.agents/skills/unlit3d/REFERENCE.md)、[多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md)、[排查](../../.agents/skills/unlit3d/TROUBLESHOOTING.md) | 立即纠错；双 pass 尺寸/相机示例及验证，P2-B 后复验 |
| SK-2 | [场景](../../.agents/skills/unlit3d/SCENE.md)、[多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md)及关联的已有示例/测试 | 与 SK-7 同批；提供可复制片段与编译检查来源的对应清单 |
| SK-3 | [场景](../../.agents/skills/unlit3d/SCENE.md)、[排查](../../.agents/skills/unlit3d/TROUBLESHOOTING.md)、[egui rustdoc](../../crates/unlit3d/src/egui.rs) | 同步修复并在现有 GPU 测试中验证可见性，不只通过 doctest |
| SK-4 | [多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md)及其步进/音频示例 | 与 P0-D 协调；交付终止/失败/音频消费验证，P1-B/D 后复验 |
| SK-5 | [索引](../../.agents/skills/unlit3d/REFERENCE.md)及自定义 source 示例 | 先纠正 SurfaceKey 规则；P2-B 合并时更新实际身份 API 与测试 |
| SK-6 | [多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md)、[索引](../../.agents/skills/unlit3d/REFERENCE.md)、[排查](../../.agents/skills/unlit3d/TROUBLESHOOTING.md) | 先补当前行为表；P1-A/B/D、P2-D 后更新类型、失败与重试示例 |
| SK-7 | [入口](../../.agents/skills/unlit3d/SKILL.md)、[多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md)、[排查](../../.agents/skills/unlit3d/TROUBLESHOOTING.md) | 与 SK-2 同批；跨 world 校验用例，P1-D/P2-B 后复验 |
| SK-8 | [MCP](../../.agents/skills/unlit3d/MCP.md) | 无代码依赖，优先纠正权限与环境指引；不得为验证擅自安装 CLI 或绕过拒绝 |
| SK-9 | [MCP](../../.agents/skills/unlit3d/MCP.md)与[入口](../../.agents/skills/unlit3d/SKILL.md)的取图提示 | 交付工具选择表与宿主验证记录；宿主不可用时保留行为验收待办 |
| SK-10 | [入口](../../.agents/skills/unlit3d/SKILL.md)、[场景](../../.agents/skills/unlit3d/SCENE.md)、[索引](../../.agents/skills/unlit3d/REFERENCE.md) | 先校准当前能力；SUP-E/F 合并时同步输入/字体说明，P3-C 复核 |

每项交付记录必须列出任务编号、已改页面/源码、验证命令与结果、未验证条件。可复制片段优先引用或复用已有示例、doctest、单一集成测试 binary；确需新增验证时接入项目现有检查入口，不建立没人运行的平行样例。示意片段明确省略项，不得将所有错误片段改标「示意」来回避修复与编译验证。源码 rustdoc 的修订保持英文，相关双语 README 若受影响一并同步。

首次纠错不等待 P3；后续 API 迁移由对应代码任务同步完成，不能以「技能任务已做过」为由跳过复验。最终关闭要求下文每项验收和依赖迁移后的复验均通过。

验证范围：七份技能文档的 32 个 Markdown 本地链接均可解析；关键 API 已作静态对照。未运行 Cargo/GPU/wasm 测试，未连接 MCP 宿主；当前 shell 的 `mcpdo connect --help` 返回 `mcpdo: 未找到命令`，所以 CLI 安装来源、版本与命令可执行性不能记为已实测。

以下「高」表示可能导致照抄不能编译、不绘制或违反权限要求；「中」表示容易造成运行语义或设计误判。

## 结构与长期维护审查

上一版审查主要覆盖 API、示例和运行语义；进一步检查目录组织、信息分层和 Markdown 形式后，结论需要修正为：**路由方向合理，但当前附件之间存在职责重叠、权威来源不够明确、教程与参考资料混排的问题，应该做一次小范围结构整理，不需要推倒重写。** 这些问题比单个 API 名称更稳定，也应作为正式实施任务。

### 结构问题

1. `SKILL.md` 既是路由入口，又重复承载详细心智模型、硬约束、MCP 输出策略和多个易变 API 名称。agent 读完入口后仍可能无法判断哪个附件是权威来源；入口应只保留决策树、不可变约束和附件职责。
2. `SCENE.md`、`MULTI_WORLD.md` 同时包含教程、设计原则、完整代码片段、边界说明和参考性质的实现细节；`REFERENCE.md` 又包含大量签名、行为和版本敏感常量。相同规则因此散落多处，更新时容易只改一份。
3. `MCP.md` 把连接前置条件、宿主概念、工具目录、按帧教程和命令示例放在一篇长文中；`EXAMPLES.md` 又重复命令和源码入口。按任务检索时，agent 需要先读大量与当前动作无关的内容。
4. 文档没有统一的「适用范围 / 前置条件 / 可复制性 / 验证方式」元信息。标题层级虽然可解析，但教程代码、示意代码和必须逐字复制的代码主要靠正文措辞区分。
5. 表格适合索引，不适合承载长契约；当前一些表格单元格塞入多条规则和例外，窄屏或模型抽取时容易丢失条件。长规则应移到小节，表格只保留对象、用途和来源。

### 稳定的改进原则

- `SKILL.md` 只回答「先选哪一层、下一篇读什么、不能违反什么」；具体签名、命令、工具参数和版本敏感行为只在一个附件中定义。
- 每篇附件开头固定写「用途、读者、前置阅读、本文不负责什么」；每个代码块标为「可编译骨架」「示意片段」或「命令」，并在可编译骨架旁标出验证入口。
- 每条跨文档规则只保留一个权威表述，其他页面用链接和一句摘要；规则变化时先改权威页面，再更新索引、排查和示例。
- 参考索引只做发现入口，不复制完整契约；把长表拆成按主题的小节，保留源码路径和搜索词，不记录易过时的完整签名，除非该字段本身是 agent 必须知道的硬约束。
- MCP 文档按「连接与权限」「宿主/世界定位」「工具选择」「按帧工作流」「故障排查」分层；工具参数以 `tools/list` 为最终来源，示例只展示最小流程。
- 统一使用一个 Markdown lint/链接检查入口，并检查标题层级、表格列数、围栏代码语言标记、相对链接、尾随空格和重复的一级标题；不引入需要 agent 猜测的隐式格式。

<a id="sk-11"></a>

## S11：技能附件职责和信息层级需要整理（中）

**问题**：入口、教程、索引、MCP 操作手册和排查表各自重复规则；`REFERENCE.md` 的索引定位与实际详细契约不完全一致，`MCP.md` 和 `EXAMPLES.md` 也重复命令与源码入口。目录目前能用，但长期维护时一次 API 变更需要修改多处，容易产生相互矛盾的 agent 指令。

**实施要求**：保留七文件入口不变，先建立每篇文档的职责矩阵和唯一权威规则表，再按「决策 → 教程 → 参考 → 操作 → 排查」分层。删去重复长段，改为指向权威小节；不在本任务中顺便改变 API 或新增第二套技能目录。

**验收**：每个稳定规则能指向唯一权威小节；七份文档开头都有用途和边界；随机抽取 10 条跨文档规则，只有一处完整契约且其余位置链接到该处；agent 能从入口在两次跳转内到达对应操作页。

<a id="sk-12"></a>

## S12：代码片段和 Markdown 格式缺少机器可验证的分类（中）

**问题**：正文依赖「可直接抄」「示意」等自然语言区分代码，命令、完整 Rust 示例和伪代码没有统一标记；表格中混入长条件，标题层级和链接虽可用，却没有稳定的格式检查契约。

**实施要求**：定义并使用统一标记：`可编译骨架`、`示意片段`、`命令`、`输出示例`；Rust 骨架标注验证命令或现有 doctest/测试来源。补充仓库内一个轻量 Markdown 检查入口，至少验证围栏语言、标题层级、表格列数、相对链接、锚点和尾随空格；检查失败不能被「示意」标签掩盖。

**验收**：所有代码块有分类和语言；所有可编译骨架有可运行验证来源；Markdown 检查覆盖七份技能文档并在 CI 或 `cargo xtask check` 中有明确入口。

<a id="sk-13"></a>

## S13：高频操作文档上下文过大且检索路径不稳定（中）

**问题**：`MCP.md` 同时承载连接、工具清单、世界模型和完整交互回合，`REFERENCE.md` 包含大量不同主题的长表；agent 为完成一个局部任务需要加载整篇附件，增加上下文成本和误用旧规则的概率。

**实施要求**：按稳定主题拆分小节或附件，但先保持现有链接兼容；MCP 至少把连接前置条件、工具选择和按帧流程分成可独立定位的锚点，参考索引按 world/pass、mesh/material、frame/input、multi-world、MCP 分组。每个页面保留「继续阅读」链接，不复制另一页的完整内容。

**验收**：每个常见任务有唯一入口和不超过两次跳转的阅读路径；工具参数、源码位置和行为契约不在多个页面重复；常见局部任务无需读取整篇无关长文即可完成。

<a id="sk-1"></a>

## S1：Frame 与相机尺寸规则已过时（高）

**问题**

[入口第 34、47 行](../../.agents/skills/unlit3d/SKILL.md#L34-L47)、[相机说明](../../.agents/skills/unlit3d/SCENE.md#L94-L99)、[排查清单](../../.agents/skills/unlit3d/TROUBLESHOOTING.md#L9) 仍要求使用 `frame.size`；[索引](../../.agents/skills/unlit3d/REFERENCE.md#L120) 和[步进片段](../../.agents/skills/unlit3d/MULTI_WORLD.md#L58) 也仍列出该字段。但 [Frame 定义](../../crates/unlit3d/src/frame.rs#L42-L47) 只有 `delta_time`、`index`。

[相机说明](../../.agents/skills/unlit3d/SCENE.md#L98) 只介绍按 `active` 切换，遗漏了 pass 显式指定相机的路径；[PassInfo 索引](../../.agents/skills/unlit3d/REFERENCE.md#L30) 同样遗漏 `camera`。[MeshSource](../../crates/unlit3d/src/mesh_source.rs#L380-L394) 优先使用 `info.camera`，未指定时才回退到可达的活动相机。

**实施要求**

- 同步上述全部位置，不通过重新给 Frame 添加全局尺寸来迁就旧文档。
- 用目标 pass 的尺寸及其 viewport 内容尺寸确定 aspect，注明尺寸由宿主更新并提供给相机行为；同帧多个不同尺寸的 pass 不能共用一个无条件的窗口 aspect。
- 补充 `FramePass::camera` 的选择方式与未指定时的回退行为。

**验收**：技能内不再访问 `frame.size`；一个双 pass、不同 aspect/相机的示例能说明尺寸和相机各来自哪里。

<a id="sk-2"></a>

## S2：标为可抄的代码片段存在确定的类型错误（高）

**问题**

- [App 示例](../../.agents/skills/unlit3d/MULTI_WORLD.md#L20-L44) 缺少必需的 `type Args`；[App 定义](../../crates/unlit3dx/src/app.rs#L119-L127) 没有关联类型默认值。该片段还把 `setup_render_world` 的结果直接当作 `RenderSetup` 返回，但[实际 helper](../../unlit3dx_examples/src/scene.rs#L190-L195) 返回 `(RemoteEntity, RemoteEntity)`。
- [离屏片段](../../.agents/skills/unlit3d/MULTI_WORLD.md#L50-L80) 使用 `pass.0`、`renderer.0`；[RemoteEntity](../../crates/unlit_ecs/src/identity.rs#L35-L55) 是具名字段结构体。直接换成 `.entity` 虽能取出本地句柄，也不应该省略接收 world 的来源验证。
- [输入片段](../../.agents/skills/unlit3d/SCENE.md#L126-L132) 的 Rust `KeyEvent` 初始化缺 `location`；[字段定义](../../crates/unlit3d/src/input/event.rs#L388-L403) 上的反射默认值不能替代 Rust 结构体初始化字段。

**实施要求**

- 无参数 App 显式声明 `type Args = NoArgs`；按[实际 Demo](../../unlit3dx_examples/src/demo.rs#L99-L103) 解构并组装 `RenderSetup { renderer, pass }`。
- 跨 world 的 pass/renderer 先由目标 world 的 `resolve` 校验，再用于 `PersistentTarget::bind` 或查询；失败显式传播。
- 输入片段补 `location: KeyLocation::Standard` 或明确的目标位置。
- 区分「可编译最小示例」与「需要应用自定义项的示意片段」。后者列出依赖的变量、helper 和类型，不再暗示完整可复制。

**验收**：将可复制片段纳入编译检查；不把省略项、未定义变量或错误签名当成正常的示例占位。

<a id="sk-3"></a>

## S3：egui 可复制片段挂错层级，会不绘制（高）

**问题**

[egui 示例](../../.agents/skills/unlit3d/SCENE.md#L103-L112) 在根下生成 `Source`。普通根实体不属于任何 pass；[Renderer 的处理](../../crates/unlit3d/src/renderer.rs#L125-L145) 明确是不绘制并警告。紧随片段的第 116 行反而要求把 source 挂在 pass 下，示例与规则矛盾。[库内 egui doctest](../../crates/unlit3d/src/egui.rs#L21-L29) 也有同样写法，不能仅靠引用 doctest 避免问题。

**实施要求**

- 默认示例在已有目标 pass 的 `scope_mut(pass)` 下生成 source，面板继续放在 source 子树下。
- 同步修正库内示例，明确其是否只演示装配，还是演示实际绘制。
- [面板不可见排查项](../../.agents/skills/unlit3d/TROUBLESHOOTING.md#L35) 优先检查 pass 可达性、附件及面板层级；构造时间不是唯一条件。

**验收**：除编译外，复用现有 GPU 测试设施验证示例能画出面板；保留[无 pass 可达则不绘制的回归用例](../../crates/unlit3d/tests/passes.rs#L221-L260)。

<a id="sk-4"></a>

## S4：示例没交代步进结束与音频消费的必要处理（中）

**问题**

[离屏循环](../../.agents/skills/unlit3d/MULTI_WORLD.md#L59-L80) 忽略 `Stepping::step` 和 `tick_sub_world` 的结果。前者明确[返回 StepOutcome](../../crates/unlit3dx/src/app/scene.rs#L239-L283)，失败或正常结束后不应继续当作成功帧渲染。注释提到 AudioPull，但代码不拉取样本；[音频消费实现](../../crates/unlit3dx/src/app/scene.rs#L91-L103) 明确不由 Stepping 完成。

**实施要求**

- 正确区分继续、结束、停止和失败；帧计数与捕获只记录真正完成的帧。
- 只要示例承诺录音或混音，就展示获取 AudioFormat、调用 AudioPull 并消费样本的路径；纯图像示例则明确不演示音频消费，删除误导性的步骤标题。
- 读回返回值按实际 API 区分 future、Result 与像素，不隐含成功。

**验收**：结束或失败不会继续生成成功捕获；含音频的示例确实得到并消费样本。

<a id="sk-5"></a>

## S5：SurfaceKey 被误当成 pass 身份（中）

**问题**

[自定义 source 索引](../../.agents/skills/unlit3d/REFERENCE.md#L34) 要求「per-pass 状态按 info.surface 存」。但 [SurfaceKey](../../crates/unlit_wgpu/src/specialize.rs#L481-L493) 只有颜色格式、深度格式、采样数，特意不包含尺寸，更不包含 pass、相机或目标身份。两个 pass 可以有相同 key，但各自需要不同的状态。

**实施要求**

- 仅把按目标格式/采样特化的 GPU 缓存按 SurfaceKey 组织。
- pass 私有状态留给 pass 自己或有明确归属的组件；不能声称当前 PassInfo 已提供不存在的 PassId。
- 对确需区分 pass 身份的自定义 source，明确当前扩展限制，关联未来 API 设计，而不是发明伪签名。

**验收**：同格式、同采样、不同尺寸或相机的两个 pass 不串用私有状态；兼容的 pipeline 缓存仍可共享。

<a id="sk-6"></a>

## S6：通信说明遗漏超时、积压和入队失败语义（中）

**问题**

[通信说明](../../.agents/skills/unlit3d/MULTI_WORLD.md#L8-L10) 与[索引](../../.agents/skills/unlit3d/REFERENCE.md#L181-L187) 把「永不阻塞」「一次请求一次应答」写得过强，却未说明：

- [Pending](../../crates/unlit3dx/src/request.rs#L85-L144) 可以配置无超时，同 key 会合并；超时或 world 结束给 Expired，过期并不撤销已入队闭包，迟到回答会被丢弃。
- [Stream](../../crates/unlit3d/src/relay.rs#L148-L160) 每 reader 使用无界队列，慢消费者会积压；发布还会获取 sender 列表的 Mutex。不等待业务回复或队列空位，不等于严格 wait-free。
- JobSender 返回 Ok 只代表入队；当前 Full/Closed 不返还 FnOnce 载荷。不能让 agent 以为拿到错误后还能无损复用原闭包。
- [snapshot 排查项](../../.agents/skills/unlit3d/TROUBLESHOOTING.md#L55) 没覆盖 collector 一直返回 None、订阅未成功安装等情况；一个还能读取的 reader 不能同时被解释为已经 drop。

**实施要求**

- 增加一张小表：是否保留历史、队列是否有界、失败代表什么、谁推进、超时是否取消。
- 强调 Pending 的 state 与 sender 必须来自同一 world；超时重试涉及幂等性和可能重复副作用。
- 保留内置机制的优先使用规则，但不用它推导绝对无锁、必定执行、必定回答或无内存积压的保证。

**验收**：agent 可据文档正确处理 Full、Closed、Expired、未首次发布、慢 reader，而无需猜测「重试是否会重复生成对象」。

<a id="sk-7"></a>

## S7：跨 world 句柄规则在入口与附件中矛盾（中）

**问题**

[入口](../../.agents/skills/unlit3d/SKILL.md#L48) 与[自检](../../.agents/skills/unlit3d/TROUBLESHOOTING.md#L15) 仍让调用者自建 newtype；[多 world 附件](../../.agents/skills/unlit3d/MULTI_WORLD.md#L88) 已正确推荐 RemoteEntity。单纯按角色包 newtype 不能校验同角色的不同 world 实例。

**实施要求**：统一推荐 `World::remote` / `World::resolve`；角色 newtype 可以另加，但不能代替 WorldId 校验。同步清理 S2 中旧 tuple 访问。

**验收**：跨 world 消息中的实体不会仅以裸 Entity 或仅角色包装传递；错误 world 的句柄明确被拒绝。

<a id="sk-8"></a>

## S8：MCP 环境处理包含绕过沙箱的建议（高）

**问题**

[MCP 环境说明](../../.agents/skills/unlit3d/MCP.md#L29-L36) 建议 home 只读时改 `HOME` 指向可写目录。这把权限拒绝当成路径问题，且可能改变认证、配置、daemon 与 GUI 环境，不能作为默认修复。

**实施要求**

- 删除因沙箱拒绝而重定向 HOME 的指令；改为报告受限操作、按环境审批流程请求必要权限，未获批则停止该操作。
- mcpdo 安装来源、版本、登录与 daemon 行为交由环境提供的 mcpdo 技能或实际帮助确认；项目技能只保留宿主启动与工具语义。
- 连接后先确认是否已可用，不能把等待认证的 exit 0 当作已连接；自动重连可能重启 stdio 宿主，不把连接名持久化写成 world 状态永续保证。
- 图形环境的配置交给对应环境指引，不把某个固定 Wayland 名称当作通用解法。

**验收**：文档不指导通过更换 HOME 等方式绕过拒绝；缺 CLI、缺权限、待认证和宿主重启均有明确下一步。

<a id="sk-9"></a>

## S9：取图规则造成不必要的重绘或内联大结果（中）

**问题**

[MCP 交互要点](../../.agents/skills/unlit3d/MCP.md#L200) 把选择写成 read_texture_as_image 或 save_screenshot，遗漏 save_texture_as_image。该工具[直接读已有纹理并落盘](../../crates/unlit3d/src/mcp/host.rs#L462-L471)，不需要为落盘改走[先渲染的 screenshot 路径](../../crates/unlit3d/src/mcp/host.rs#L591-L616)。现说明也与入口「大结果优先落盘」冲突。

**实施要求**：按两个独立选择说明工具：是否先渲染，以及内联还是落盘。按帧驱动后保留既有帧并落盘时优先 save_texture_as_image；确需主动重绘时才用 screenshot 系列。不得把额外渲染自动说成额外推进了一次完整模拟。

**验收**：可在不额外渲染、不内联大图的前提下检查 step_frames 产生的纹理；参数从 tools/list 和运行结果获取。

<a id="sk-10"></a>

## S10：关键回调限制与能力边界缺失（中）

**问题**

- [egui 专节](../../.agents/skills/unlit3d/SCENE.md#L101-L118) 未提醒面板回调可因 discard 重入，而[源码](../../crates/unlit3d/src/egui.rs#L85-L99) 明确不是每帧一次。agent 容易在绘制回调里推进模拟、生成对象或重复发 job。
- [Blitz 输入说明](../../.agents/skills/unlit3d/REFERENCE.md#L155) 概括键盘 code/location，但字符路径实际会合成 Unidentified/Standard 并丢重复信息；[字体说明](../../.agents/skills/unlit3d/REFERENCE.md#L158) 的跨平台一致性未限定字体特性、系统字体和页面资源。同进程多文档还存在字体集合共享的问题，详见[独立补审](supplemental-review-2026-11-24.md)。
- [入口硬约束](../../.agents/skills/unlit3d/SKILL.md#L37-L48) 把架构偏好与类型/渲染前置条件混写，并笼统称违反任一条都会编译失败、panic 或空画面；这不是所有设计建议的真实后果。

**实施要求**

- egui 专节写清：面板可多次执行，需要每帧一次的工作由明确节拍的兄弟行为组件负责；不武断声称所有 UI 点击副作用必然重复。
- 不把已知实现缺陷包装成使用者必须长期绕开的规范。修实现前精确限制能力描述，修实现后由回归测试支撑承诺。
- 分为「类型/运行前置条件」「默认装配建议」「扩展与限制」，保留项目设计选择，删除没有依据的必然失败断言。

**验收**：agent 不把 UI 构建次数当 tick 次数；Blitz 能力说明不超出验证范围；架构建议和强制前置条件可清楚区分。

## 实施顺序与防回归

1. 先处理 S1、S2、S3、S8：修无法使用的示例、空绘制与权限建议。
2. 接着处理 S4–S7、S9、S10；同一规则只在专题页完整解释，入口和排查表保留短提示与链接。
3. 保留七文件路由结构，不为了减少文件数合并成大技能。签名表优先链接拥有定义的位置，少复制高变动字段。
4. 将可复制代码片段关联到可执行示例、doctest 或现有集成测试；后续增量维护时校验这些来源，不另建无人运行的平行示例。
5. 链接检查、编译检查、行为检查分别验收：链接存在不代表 API 正确；编译通过不代表 source 会被 pass 画到。文档与测试生成方式的约束可补入项目既有检查入口，不要求每次文档改字都重跑全仓 GPU 测试。

### 已排除的疑点

`context().egui_wants_pointer_input()` 不是过期方法：[锁文件](../../Cargo.lock) 中的 `egui` 为 0.37.0，该版本提供此方法。不能根据其他 egui 版本的记忆把它改成旧名称。技能内部本地链接无断链，也没有必要为了审查改掉现有路由设计。

## 交付记录

| 任务 | 交付与本机验收 |
| --- | --- |
| SK-1 | [场景](../../.agents/skills/unlit3d/SCENE.md#相机与-viewport) 区分目标、内容 viewport 与相机；[pass 测试](../../crates/unlit3d/tests/passes.rs)的 `a_pass_uses_its_own_camera_and_content_size` 验证两个目标分别使用自己的尺寸、viewport 和相机。 |
| SK-2 | [场景骨架](../../.agents/skills/unlit3d/SCENE.md#最小骨架)与[多 world 骨架](../../.agents/skills/unlit3d/MULTI_WORLD.md#两种骨架)指向实际 README doctest、示例应用和无头测试，不维护平行的手抄 Rust 实现。 |
| SK-3 | [egui 模块 doctest](../../crates/unlit3d/src/egui.rs) 将 source 挂在 pass 下；[GPU 测试](../../crates/unlit3d/tests/gpu_egui_source.rs)的 `egui_only_with_two_panels` 读回两块面板的像素。 |
| SK-4 | [多 world](../../.agents/skills/unlit3d/MULTI_WORLD.md#展示路径与离线取帧)区分推进、绘制和音频消费；[无头测试](../../unlit3dx_examples/tests/headless.rs)覆盖正常终止、失败保因、不捕获终止帧、跳过中间绘制和用真实音源消费离线混音。 |
| SK-5 | [身份与缓存键](../../.agents/skills/unlit3d/REFERENCE.md#pass-与-source)区分 pass、帧、附件组合和管线特化；由 [pass 测试](../../crates/unlit3d/tests/passes.rs)的 `a_build_is_told_the_pass_target_and_frame_it_belongs_to` 和 `one_colour_view_is_two_targets_when_the_depth_differs` 验证。 |
| SK-6 | [通信协议](../../.agents/skills/unlit3d/MULTI_WORLD.md#通信协议)集中说明拒绝、安装、快照、流终止、请求及取消；[订阅集成测试](../../crates/unlit3dx/tests/subscriptions.rs)和[请求单测](../../crates/unlit3dx/src/request.rs)验证区分终态与迟到响应。 |
| SK-7 | [装配规则](../../.agents/skills/unlit3d/MULTI_WORLD.md#装配规则)使用 `World::remote` 和 `World::resolve`；[ECS 测试](../../crates/unlit_ecs/src/world/tests.rs)的 `a_handle_from_another_world_is_turned_down` 验证跨 world 拒绝。 |
| SK-8 | [MCP 连接指引](../../.agents/skills/unlit3d/MCP.md#服务器与驱动)遵循已有客户端、审批与认证流程，不建议安装或更改 `HOME` 绕过拒绝；静态核对通过，本机无 `mcpdo`。 |
| SK-9 | [取图表](../../.agents/skills/unlit3d/MCP.md#绘制)分开是否重绘、内联或落盘。[真实 stdio 宿主测试](../../unlit3dx_examples/tests/mcp_input.rs)的 `the_mcp_input_and_audio_match_the_snapshots` 在步进后保存现有纹理，核对路径、像素、无内联图片和不推进帧；[读回实现](../../crates/unlit3d/src/mcp/host/readback.rs)的 `save_texture_as_image` 不进入 `screenshot_frame` 的重绘路径。 |
| SK-10 | [egui 回调](../../.agents/skills/unlit3d/SCENE.md#egui)、[Blitz 输入与字体](../../.agents/skills/unlit3d/REFERENCE.md#blitz)和[技能入口](../../.agents/skills/unlit3d/SKILL.md)区分前置条件、建议与能力边界；原生输入与字体隔离回归复验通过。 |
| SK-11–SK-13 | 七页职责分离；[API 定位](../../.agents/skills/unlit3d/REFERENCE.md#查当前版本的-api)优先结构化数据，源码按文件和符号定位；[Markdown 检查](../../xtask/src/skill_docs.rs)验证围栏、表格、标题和链接，拒绝手写行号锚点。 |

本机原生验收：`cargo nextest run --workspace --all-features` 通过 1,405 项；之后补入的无头终止捕获回归和固定行号检查分别定向通过。`cargo test --workspace --all-features --doc` 通过 49 项、忽略 11 项。全工作区全目标全特性严格 Clippy、格式检查与 `cargo xtask skill-docs` 通过；393 个 Rust 文件无达到 1000 行。未验证浏览器、WebGL2 档位、Android/Windows、`mcpdo` 客户端或跨平台系统字体一致性。
