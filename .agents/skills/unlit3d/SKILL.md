---
name: unlit3d
description: 用 unlit3d（单 world 渲染库）与 unlit3dx（多 world 运行时）写场景，并用 MCP 调试运行中的 world。涵盖搭 world、挂 source、加几何体与材质、打光、接 UI 与输入、拆多 world，以及读写组件、抓帧截图、注入输入、无窗口按帧驱动。当需要新增或修改场景、扩展 unlit3d/unlit3dx、判断渲染代码该放哪一层、排查画面异常，或观察与操纵运行中的示例时使用。
---

# unlit3d / unlit3dx

**本文只给决策、心智模型和硬约束**。具体骨架、签名、工具清单都在同目录附件里，按路由按需读取——不要一次全读。通用约定（`cargo nextest run`、快照目录、中文直角引号、调试示例用本技能而非截屏软件）见根 AGENTS.md；`cargo xtask check` 里 clippy → doc → `fmt --check` 的顺序见 `xtask/README.md`。本文不重复。

| 我要…                                                        | 读                                    |
| ------------------------------------------------------------ | ------------------------------------- |
| 写新场景 / 改渲染代码：world、mesh、材质、光照、相机、UI、输入、帧行为 | [SCENE.md](SCENE.md)                  |
| 拆多 world、判断代码该放哪层、跨 world 通信                 | [MULTI_WORLD.md](MULTI_WORLD.md)      |
| 调试运行中的 world：MCP 宿主、工具、按帧驱动、录音           | [MCP.md](MCP.md)                      |
| 画面或行为不对，找原因                                       | [TROUBLESHOOTING.md](TROUBLESHOOTING.md) |
| 查某个 API 的签名、常量、字段类型                             | [REFERENCE.md](REFERENCE.md)          |
| 找可抄的例子、命令、源码位置                                 | [EXAMPLES.md](EXAMPLES.md)            |

## 先选层

`unlit3dx` 不自带渲染器：它的 render bridge 建在 `unlit3d` 的 `MeshSource` 上（参考实现 `unlit3dx_examples/src/render.rs`），所以单 world 的规则在多 world 里一条都不改。

| 层        | 什么时候用                                             | 入口                                                                    |
| --------- | ------------------------------------------------------ | ----------------------------------------------------------------------- |
| `unlit3d`  | 单 world 就够：离线出图、快照测试、嵌进别人的宿主程序 | `spawn_context` + `MeshSource` + `Renderer`                              |
| `unlit3dx` | 渲染/物理/音频/玩法各自成 world，靠 job 与 snapshot 相连 | `spawn_sub_world` / `spawn_threaded_world` + `request_gpu` + `setup_presenter` |

## 心智模型

1. **world 是一棵树**，不是一堆组件。`root` → `shared` → 各 scope 的子树；每个 scope 只看得到自己的子树加 `shared` 子树，看不见兄弟。
2. **source 画它自己子树里的东西**。`Renderer::render` 遍历每个 pass，对 pass 下面的每个 `Source` 用 `world.scope(source_entity)` 取作用域，只有挂在该 scope 下的实体才被画。相机同理——相机是为「持有它的那个 source」取景的。
3. **pass 是一个 scope，source 又是一个 scope**。pass 录制它下面的 source，按 `FramePassOrder` 排序；pass 内按 `FrameSourceOrder` 排序。挂在 pass 下、与 source 平级的渲染实体不会被画。
4. **一帧两个阶段**：先让每个 pass 的每个 source 调 `build_scene` 填 `Scene`（上传也走这里给的同一个 encoder），再逐个 pass 录制提交。所以上传和读它的 draw 在同一次 submission 里。
5. **`Renderer::render(&world)` 是唯一的驱动点**。它只录帧、不推进世界；`Frame`（`delta_time` / `index` / `size`）由调用方构造，用 `dispatch_frame` 发给 `OnFrame` 行为组件。
6. **资源句柄是组件**。`GpuMesh` 用 `Rc<MeshParts>` 共享底层缓冲区、`GpuMaterial` 是资源图里的 bind group 句柄，clone 都只是拷贝句柄；想再画一份就 clone 出来再 spawn 一个实例实体，不要重新创建 GPU 资源。

## 硬约束

违反任一条都会导致编译失败、panic 或空画面；细则在对应附件里。

- 状态进 ECS：一个对象 = 数据组件 + 它的 tick（行为组件），一个 bundle 一次 spawn；不另立 `Vec<MyObject>` 或 `GameState` 再手动同步；不做塞下一切的巨大组件或大 enum 状态机。
- 跨线程 / 跨 world 通信只用内置机制：`World` 是 `!Send`，`Arc<Mutex<World>>` 编不过；进去靠 job，回来靠 `watch` / `subscribe` / `Pending`。不自造 mpsc、`Mutex`、共享 `Arc` 或回答式 job。
- 渲染实体与相机必须 spawn 在**画它的那个 source** 之下；两边都要读写的状态放 `world.shared()`。
- 行为回调只拿 `&World`：结构变更走 `Scope::queue` + `World::apply`；回调里不能再取自己这个组件类型（运行时已借用 → panic）。
- 深度是 reverse-z：`Greater` + 清成 0.0 + `perspective_infinite_reverse`，三者缺一画面就空。
- `spawn_context` 每个 world 只调一次；同一个 world 里同一 key 类型只能注册一次 family。
- 相机每帧按 `frame.size` 重建 aspect，不要在构建时捕获尺寸。
- 跨 world 的实体用 newtype 包一层：每个 world 的实体编号都从 0 开始，裸 `Entity` 一定会混。
- 光要自己开、自己摆：`LitOptions::unlit = false` 才受光（示例骨架默认 `true`），灯是 `LightSource` 组件、spawn 在画它的 source 子树里、沿自己 `-Z` 照。见 [SCENE.md](SCENE.md) 的光照一节。

完整自检清单见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。

## 上下文成本（所有附件共用）

- MCP 结果用 `max` 截断，别把整个实体表灌进上下文。
- 优先 `save_*` 落盘而不是 `read_*` 内联：内联图片 / base64 会永久占掉一块模型上下文，落盘文件可按需读、也可不读。
- 示例里的 world 句柄、资源 id、按钮坐标都是示意值；**每次启动都会变**，一律从 `list_worlds` / `scene_status` / `list_resources` 的返回值取。

## 两个入口怎么衔接

写代码与改设计看 [SCENE.md](SCENE.md) / [MULTI_WORLD.md](MULTI_WORLD.md)；连上 world 观察与操纵运行时看 [MCP.md](MCP.md)。用 MCP 查「这个 pass 为什么没画东西」时要带着本文的心智模型去读 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)；反过来，`--mcp` 会改变渲染路径（见 [MCP.md](MCP.md)），别拿桌面帧去推断离屏帧。
