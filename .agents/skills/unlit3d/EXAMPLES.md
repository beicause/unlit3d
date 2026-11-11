# 示例、命令与源码位置

## 跑起来、调起来

通用约定（`cargo nextest run`、快照目录）见根 AGENTS.md；`cargo xtask check` 的检查顺序见 `xtask/README.md`。这里只列与场景强相关的命令。

| 目的                       | 命令                                                                                                        |
| -------------------------- | ----------------------------------------------------------------------------------------------------------- |
| 单 world 的窗口示例        | `cargo run -p unlit3d_examples --bin unlit3d-examples -- --scene spin_cube`（`--list-scenes` 看全部，`--mcp` 开调试宿主） |
| 多 world 的示例            | `cargo run -p unlit3dx_examples --bin unlit3dx-examples`（另有 `--mcp` / `--stepped` / `--headless`；`--headless --mcp` 是无窗口、按帧驱动的 MCP 宿主） |
| glTF 出图                  | `cargo run -p unlit3d_cli --bin unlit3d-cli -- --output frame.png --document unlit3d_asset_files/assets/Fox.glb --animation Walk --time 0.2`（`--output` 是唯一必填项，不带就是用法错误） |
| 整个工作区测试             | `cargo xtask test`（内部是 `cargo nextest run` 加 `cargo test --doc`）                                     |
| 单 crate                   | `cargo nextest run -p unlit3d --all-features`                                                               |
| 重存快照                   | `SNAPSHOT_UPDATE=1 cargo nextest run -p unlit3d_examples`，再审快照的 diff                                 |
| 多 world 的端到端测试      | `cargo nextest run -p unlit3dx_examples`：离屏渲染帧、混音，以及起真实进程走 MCP 的四步交互（三张图 + 一段音频） |

快照不匹配的帧写在 `target/snapshot-mismatches`（webgl2 档 `target/snapshot-mismatches-webgl2`，wasm 档 `target/snapshot-mismatches-wasm`；单独跑时由 `UNLIT3D_SNAPSHOT_MISMATCH_DIR` 指定）。

调试**运行中**的示例用本技能的 [MCP.md](MCP.md)，不要用截屏软件。

## 示例索引

| 场景                                                        | 演示什么                                                       |
| ----------------------------------------------------------- | -------------------------------------------------------------- |
| `spin_cube`                                                 | 纹理立方体 + 两块 egui 面板 + 键盘与拖拽；默认场景，也是窄宽高比快照的取样对象 |
| `ui_only`                                                   | 没有 mesh source、没有相机也能出一帧 UI                          |
| `mesh_and_ui`                                               | mesh source 与 UI source 同帧的记录顺序、scissor、load ops       |
| `transparent`                                               | 透明 + `ZSortedDrawing` + blend                                  |
| `animated`                                                  | 八帧内 mesh 的分配、回收、平移旋转                               |
| `skinned` / `morphed` / `instanced_skinned_morph`           | 蒙皮、morph，以及共享一个 mesh 的实例合并                        |
| `gltf`                                                      | `LitGltf` 加载、动画采样、`spawn_default_scene`                  |
| `mesh_topologies`                                           | 五种 primitive topology，索引与非索引各一遍                      |
| `custom_pipeline`                                           | 自己注册 family（程序化天空 + 粒子）                             |
| `lit`                                                       | Lambert 光照：共 8 盏（2 方向 + 3 点 + 1 聚光 + 1 矩形 + 1 三角面光），`unlit: false` 的 key |
| `lit_clusters`                                              | 光照分簇，改 `ClusterConfig` 的切片距离与远平面                  |
| `outline` / `outline_msaa`                                  | inverted-hull 描边壳；后者跑在 4x MSAA 目标上                    |
| `unlit3dx_examples`（另一个二进制，不是场景 id）              | 多 world：render / physics / audio / gameplay 分工，跨 world UI，MIDI |

## 在源码里的位置

### 核心库（单 world）

- 单 world 骨架与四步示例：`crates/unlit3d/README.md` 的「Example」一节（doctest，可直接抄）。
- frame 模型与 source/pass 语义：`crates/unlit3d/src/source.rs`、`crates/unlit3d/src/pass.rs`、`crates/unlit3d/src/renderer.rs`。
- 内建 lit family 与实例组件：`crates/unlit3d/src/lit.rs`；注册与分配在 `crates/unlit3d/src/lit/allocate.rs`。
- mesh 分配与资源注册：`crates/unlit3d/src/mesh_source/`（目录，入口 `mod.rs`，family 在 `families.rs`）。
- 管线策略与 `LitOptions`：`crates/unlit_wgpu/src/pipeline/options.rs`（聚合重导出在 `pipeline.rs`）。
- 附件与读回：`crates/unlit3d/src/attachments.rs`（`FrameAttachments`）、`crates/unlit_wgpu/src/render_attachments.rs`（`create_*_target` / `default_depth_stencil_format`）、`crates/unlit_wgpu/src/readback.rs`（`readback_texture`）。
- 目标特化：`crates/unlit_wgpu/src/specialize.rs`（`SurfaceKey` / `SurfaceTarget::set_surface`）。
- 帧行为与帧时钟：`crates/unlit3d/src/frame.rs`。
- 输入：`crates/unlit3d/src/input/`（事件词汇在 `event.rs`，状态在 `state.rs`，帧循环的接线在 `mod.rs`）。
- UI：`crates/unlit3d/src/ui/mod.rs`。
- winit 桥与内置行为：`crates/unlit3d/src/winit/` 下的 `event.rs` / `builtin.rs` / `surface.rs`。

### 多 world

- `crates/unlit3dx/README.md`；代码在 `crates/unlit3dx/src/world.rs`、`host.rs`、`job.rs`、`channel.rs`、`request.rs`。
- 展示路径与 GPU 请求：`crates/unlit3dx/src/present.rs`、`crates/unlit3dx/src/gpu.rs`。
- 参考装配（真正的骨架长什么样）：`unlit3dx_examples/src/lib.rs`、`scene/mod.rs`、`render.rs`、`headless.rs`。

### MCP 与调试宿主

- `crates/unlit_ecs/src/mcp/server.rs` —— `McpServer`、`ToolRouter`、`serve_stdio_with` 与 `serve_with`，以及 `tool()` / `ToolSource` 这套注册机制。
- `crates/unlit_ecs/src/mcp/host.rs` —— 传输契约：`McpCommand`、`Output`、`WorldRef`、`WorldEntry`、`Job`，以及落盘助手。
- `crates/unlit_ecs/src/mcp/tools.rs` —— world 与实体工具，末尾的 `register` 是它们的注册表。
- `crates/unlit3d/src/mcp/` —— 渲染工具（`host.rs`）与它们的 `register`（`mod.rs`）。
- `crates/unlit3dx/src/mcp/mod.rs` —— 音频录音工具、`request_exit` 与它们的 `register`；录制实现在 `crates/unlit3dx/src/record.rs`。
- `crates/unlit3dx/src/mcp/bridge.rs` —— `serve_mcp(world)` 起名为 `unlit3dx-mcp-transport` 的线程，宿主侧挂 `McpInbox` + `drain_mcp()` 的 `OnAboutToWait`；`pump_mcp(world)` 是无 winit 宿主用的同一套 drain + apply + 跑 stepped world 队列；`reachable_worlds` 决定 `list_worlds` 看到什么。
- `crates/unlit3dx/src/world.rs` —— `WorldSender`（留在代理实体上、让 bridge 够得着线程/步进 world 的命令通道）、`WorldName` 与 `drain_all_stepped_worlds`。
- `crates/unlit_ecs/README.md` —— 设计说明（无特权路径、传输、各 crate 注册工具）。

### 示例与测试

- 单 world 的完整可跑实现：`unlit3d_cli/src/lib.rs` 的 `build_world` / `offscreen_world`。
- 无窗口的交互式 MCP 宿主（按帧驱动、离屏渲染、自有工具）：`unlit3dx_examples/src/mcp.rs`。
- 端到端快照测试的写法（起子进程 + 手写 JSON-RPC 客户端）：`unlit3dx_examples/tests/mcp_input/native.rs`。
- `unlit3d_examples/src/lib.rs` —— 走 winit 用户事件；`unlit3d_cli/src/lib.rs` —— 走 mpsc channel。
