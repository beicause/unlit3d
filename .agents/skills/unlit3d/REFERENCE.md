# API 与源码索引

**用途**：为 agent 定位当前版本的声明和实现。应已知道要查的功能；不知道先从[技能入口](SKILL.md) 选任务。本页不复制完整签名，也不代替运行教程。下面的身份与 Blitz 小节分别是相应使用规则的入口。

## 查当前版本的 API

- **在本库仓库**：用 [API 索引命令](../../../xtask/README.md#cargo-xtask-api-index) 生成 `target/api-index.json`，按 `entries[].path` 精确筛选，只读命中的签名、文档首行及 `file` / `line`。按所需 feature 生成；全特性输出不表示项目已启用那些 feature。
- **在下游项目**：用 `cargo metadata --format-version 1 --locked` 查 `packages` 中的 `id`、`manifest_path`，并按对应 `resolve.nodes` 查实际 feature。需要结构化声明且已有 nightly 时，在下游项目运行 `cargo +nightly doc -p unlit3d --no-deps -Z unstable-options --output-format json --message-format=json`，从 `compiler-artifact` 的 `filenames` 找到 JSON。查多 world 换成 `-p unlit3dx`，有多个版本时写 `-p unlit3d@版本`；沿用下游实际构建的 target 和 feature 选项。先读取 `format_version`，按相应 schema 用 `paths` 查名称、`index` 查声明和 `span`。没有 nightly 则直接读 `manifest_path` 所在目录的源码；不需要打开浏览器、IDE 或安装工具。

Rustdoc JSON 不含函数体；判断副作用、取消、时序与错误传播时沿 `span` 回源码。技能中的源码链接以文件和符号为定位入口；精确行号以当前版本生成的索引或 rustdoc JSON 为准。找可运行的调用顺序用[示例索引](EXAMPLES.md#示例索引)。

## 启动与 world

| 搜索词 | 源码 |
| --- | --- |
| `spawn_context`, `RenderContext`, `RenderDevice` | [渲染上下文](../../../crates/unlit3d/src/source.rs) |
| `World`, `Scope`, `try_query_one` | [World 与模块入口](../../../crates/unlit_ecs/src/world.rs)、[Scope](../../../crates/unlit_ecs/src/view.rs) |
| `WorldId`, `RemoteEntity`, `World::resolve` | [身份类型](../../../crates/unlit_ecs/src/identity.rs)、[来源验证](MULTI_WORLD.md#装配规则) |

## pass 与 source

按归属选择缓存键：`PassInfo.pass` 标识逻辑 pass，`target` 标识整套颜色、深度及 MSAA 附件，`frame` 是所属帧的序号；`surface` 只有格式与采样数，适合 pipeline 特化，不能用来识别 pass、目标或尺寸。多个 pass 可共用 target 和 surface 而不共用 pass 状态。resize 可能保留目标身份；按尺寸派生的内容还要检查实际尺寸。

| 搜索词 | 源码 |
| --- | --- |
| `PassInfo`, `FramePass` | [pass](../../../crates/unlit3d/src/pass.rs) |
| `Source`, `SourceOrder`, `DrawSource`, `SceneSource`, `SceneBuild` | [绘制源](../../../crates/unlit3d/src/source.rs) |
| `PassId`, `RenderTargetId`, `FrameId` | [身份](../../../crates/unlit3d/src/target.rs) |
| `SurfaceKey`, `PersistentTarget`, `AttachmentDesc` | [管线特化](../../../crates/unlit_wgpu/src/specialize.rs)、[离屏目标](../../../crates/unlit3d/src/offscreen.rs)、[附件](../../../crates/unlit3d/src/attachments.rs) |

## mesh 与材质

| 搜索词 | 源码 |
| --- | --- |
| `MeshSource`, `MeshDescriptor`, `GpuMesh` | [绘制源](../../../crates/unlit3d/src/mesh_source.rs)、[网格描述](../../../crates/unlit3d/src/mesh.rs)、[GPU 网格](../../../crates/unlit3d/src/components.rs) |
| `LitMeshDesc`, `LitOptions`, `LitModel`, `AnimationTime` | [lit 入口](../../../crates/unlit3d/src/lit.rs)、[模型](../../../crates/unlit3d/src/lit/model.rs)、[时间域](../../../crates/unlit3d/src/time.rs) |
| `LitGltf::insert_asset`, `GltfResources`, `AnimationId` | [资源上传](../../../crates/unlit3d/src/gltf/upload.rs)、[资源表与支持范围](../../../crates/unlit3d/src/gltf.rs)、[文档 ID](../../../crates/unlit3d/src/gltf/ids.rs) |
| `Light`, `LightSource`, `LightSink` | [光照](../../../crates/unlit3d/src/light.rs) |

## 帧与输入

| 搜索词 | 源码 |
| --- | --- |
| `Frame`, `FrameClock`, `FrameBehaviour`, `FrameSkip` | [帧调度](../../../crates/unlit3d/src/frame.rs) |
| `InputEvent`, `InputEvents`, `InputState`, `InputBaseline` | [输入装配](../../../crates/unlit3d/src/input.rs)、[事件类型](../../../crates/unlit3d/src/input/event.rs)、[状态与批初基线](../../../crates/unlit3d/src/input/state.rs) |
| `EguiSource`, `EguiPanel`, `ExternalFrame` | [egui](../../../crates/unlit3d/src/egui.rs)、[场景装配与回调限制](SCENE.md#egui) |

## Blitz

`BlitzSource` 读取其作用域内的 `BlitzDocument`；资源由宿主按路径提供，不联网。`OnBlitzEvent` 可按事件名筛选，单个物理动作可能生成多种兼容事件。查[页面与回调源码](../../../crates/unlit3d/src/blitz.rs)。

输入按批初状态重放并逐事件更新；同一文档被多个 pass 绘制时不重复消费该 tick 的输入。鼠标、笔与触点各自维护按键；失焦取消活动指针并清除修饰键。字符按下与对应文本配成一次事件，保留可用的 code、location 与 repeat；单独文本（如粘贴）没有物理按键。预编辑与提交可送入文档，但不等于已经接通平台输入法的启停、剪贴板或光标形状。见[事件翻译](../../../crates/unlit3d/src/blitz/events.rs)和[GPU 用例](../../../crates/unlit3d/tests/blitz.rs)。

字体由构建 feature、实际系统字体及页面提供的资源决定；不保证不同平台或 feature 下的像素完全一致。`blitz_epaint_fonts` 提供内置默认字体，`blitz_sc_fonts` 添中文回退，`blitz_system_fonts` 才加载系统字体。基础集合可复用，页面注册的 `@font-face` 只在自己的文档匹配。见[字体配置与隔离测试](../../../crates/unlit3d/src/blitz/fonts.rs)。

## 多 world

| 搜索词 | 源码 |
| --- | --- |
| `WorldEndpoint`, `WorldRunner`, `StepOutcome` | [端点](../../../crates/unlit3dx/src/endpoint.rs)、[runner](../../../crates/unlit3dx/src/world.rs)、[单轮结果](../../../crates/unlit3dx/src/turn.rs) |
| `Subscription`, `Pending`, `SnapshotPoll`, `StreamEnd` | [订阅](../../../crates/unlit3dx/src/channel.rs)、[请求](../../../crates/unlit3dx/src/request.rs)、[relay](../../../crates/unlit3d/src/relay.rs) |
| `App`, `RunShape`, `SteppedRun` | [应用](../../../crates/unlit3dx/src/app.rs)、[运行配置](../../../crates/unlit3dx/src/app/shape.rs)、[步进装配](../../../crates/unlit3dx/src/app/stepped.rs) |

## MCP

注册与实现定位：[ECS 服务器](../../../crates/unlit_ecs/src/mcp.rs)、[渲染工具](../../../crates/unlit3d/src/mcp.rs)、[应用桥接](../../../crates/unlit3dx/src/mcp/bridge.rs)。现场参数与工具选择归 [MCP 操作页](MCP.md#工具清单)。

继续阅读：[场景](SCENE.md) · [多 world](MULTI_WORLD.md) · [排查](TROUBLESHOOTING.md)。
