# 示例与验证入口

**用途**：给 agent 选择已编译或实际运行的示例。应先从[技能入口](SKILL.md) 选中任务；此页不定义 API、运行约束或工具参数。命令从仓库根执行。

## 跑起来调起来

**命令**：本地窗口示例、无窗口 MCP 宿主、可脚本读取的测试结果。连接仍按 [MCP 连接步骤](MCP.md#服务器与驱动) 操作；不使用截屏软件调试运行中的场景。

```sh
cargo run -p unlit3d_examples --bin unlit3d-examples -- --scene spin_cube
cargo run -p unlit3dx_examples --bin unlit3dx-examples -- --headless --interactive --mcp-stdio
cargo nextest run -p unlit3d -p unlit3dx_examples --all-features
```

单 world 程序的 `--list-scenes` 给出当前 id。测试入口、快照路径和更新约定见 [TESTING](../../../TESTING.md)。

## 示例索引

| 任务 | 现有示例或测试 |
| --- | --- |
| 最小场景与 mesh | [unlit3d README Example](../../../crates/unlit3d/README.md#example) |
| 相机、目标与多个 pass | [pass GPU 测试](../../../crates/unlit3d/tests/passes.rs)、[letterbox GPU 测试](../../../crates/unlit3d/tests/letterbox.rs) |
| 单 world egui、Blitz 与 mesh 组合 | [示例合成](../../../unlit3d_examples/src/scenes/mesh_and_ui.rs)、[Blitz GPU 测试](../../../crates/unlit3d/tests/blitz.rs) |
| glTF 导入、动画与出图 | [glTF 场景](../../../unlit3d_examples/src/scenes/gltf.rs)、[CLI 用法](../../../unlit3d_cli/README.md) |
| 自定义管线与光照 | [自定义 family](../../../unlit3d_examples/src/scenes/custom_pipeline.rs)、[光照示例](../../../unlit3d_examples/src/scenes/lit.rs) |
| 多 world App 与离屏 | [Demo](../../../unlit3dx_examples/src/demo.rs)、[帧和音频端到端测试](../../../unlit3dx_examples/tests/headless.rs) |
| MCP 帧推进、图像与音频 | [进程级 MCP 测试](../../../unlit3dx_examples/tests/mcp_input/native.rs)、[原生渲染工具测试](../../../crates/unlit3d/tests/mcp_server/native.rs) |

继续阅读：[结构化 API 查询](SKILL.md#查当前版本的-api) · [排查表](TROUBLESHOOTING.md)。
