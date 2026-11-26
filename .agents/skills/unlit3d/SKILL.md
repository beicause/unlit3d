---
name: unlit3d
description: 用 unlit3d 写单 world 渲染场景，用 unlit3dx 装配多 world，或用 MCP 检查运行中的 world。适用于网格、材质、光照、pass、相机、帧、输入、egui、Blitz、跨 world 通信、离屏捕获及渲染排查。
---

# unlit3d / unlit3dx

**用途**：为写代码或排查现场的 agent 选择库层级和下一篇资料。不在入口复制 API 签名、示例启动命令或工具参数。

## 先选层

- 一棵实体树即可完成渲染、输出图像或嵌入现有宿主：用 `unlit3d`。
- 要分别驱动玩法、物理、音频或渲染 world：用 `unlit3dx` 装配与通信；渲染仍遵循 `unlit3d` 的规则。

| 任务 | 权威入口 |
| --- | --- |
| 搭场景、选相机、接输入或 UI | [场景教程](SCENE.md) |
| 装配、跨 world 传值、步进与消费音频 | [多 world 教程](MULTI_WORLD.md) |
| 连接调试宿主 | [MCP 连接与权限](MCP.md#服务器与驱动) |
| 检查已有帧或按帧推进 | [取图选择](MCP.md#绘制)、[按帧驱动](MCP.md#按帧驱动) |
| 查当前版本 API | [结构化索引与源码定位](#查当前版本的-api) |
| 找可编译范例或运行命令 | [示例索引](EXAMPLES.md) |
| 画面、输入、通信或工具异常 | [排查表](TROUBLESHOOTING.md) |

## 类型与运行前置条件

- 渲染先核对 [pass、source 与附件的可达性](SCENE.md#最小骨架)，再核对 [目标尺寸与相机](SCENE.md#相机与-viewport)。渲染不是模拟 tick。
- 跨 world 的实体用 [来源验证](MULTI_WORLD.md#装配规则)，而不是裸 `Entity` 或仅按角色包装。
- `World` 不可跨线程传递；不要尝试用锁把同一个 world 交给多个线程。见 [通信语义](MULTI_WORLD.md#通信协议)。

## 默认装配建议

- 优先用 ECS 组件存状态，用行为组件推进；使用 [场景教程](SCENE.md#状态怎么放) 判断数据放在哪个 scope。
- 优先使用库的 world 通信、app 驱动与离屏路径；只有需要改变调度或呈现所有权时再用底层部件。见 [多 world 教程](MULTI_WORLD.md#两种骨架)。
- 自定义绘制源按归属选择[缓存键](SCENE.md#绘制源缓存键)。
- 调试运行中的示例用引擎提供的 MCP 工具，大图、缓冲和录音优先落盘。见 [MCP 操作页](MCP.md)。

## 查当前版本的 API

- **本库仓库**：运行 `cargo xtask api-index`，按实际 feature 选用[索引选项](../../../xtask/README.md#cargo-xtask-api-index)。在 `target/api-index.json` 中按 `entries[].path` 筛选，只读匹配项的签名、文档首行和源码位置。
- **下游项目**：用 `cargo metadata --format-version 1 --locked` 确认依赖的 `id`、`manifest_path` 和 `resolve.nodes` 中的实际 feature。已有 nightly 时，保留项目的 target 与 feature 选项，运行 `cargo +nightly doc -p unlit3d --no-deps -Z unstable-options --output-format json --message-format=json`，从 `compiler-artifact.filenames` 找到 JSON。多 world API 换用 `-p unlit3dx`；同名多版本时加 `@版本`。先核对 `format_version`，再用 `paths` 查名称、`index` 查声明。没有 nightly 就读依赖 `manifest_path` 所在目录的源码，不需要安装工具。

Rustdoc JSON 不含函数体；判断副作用、取消、时序和错误传播时沿 `span` 回源码。查可运行的装配顺序见[示例索引](EXAMPLES.md#示例索引)。
