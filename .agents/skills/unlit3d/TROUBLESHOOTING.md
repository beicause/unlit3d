# 排查

先过自检清单，再按症状查。心智模型见 [SKILL.md](SKILL.md)；写场景的细节见 [SCENE.md](SCENE.md)，多 world 见 [MULTI_WORLD.md](MULTI_WORLD.md)，MCP 宿主见 [MCP.md](MCP.md)。

## 提交前自检清单

1. 渲染实体与相机 spawn 在**画它的那个 source** 之下？pass 的 `set_attachments` 调了？
2. reverse-z 三件套齐了（`Greater` + 清 0.0 + `perspective_infinite_reverse`）？自定义 family 在自己的管线描述符上调了 `set_surface`？
3. 相机每帧按 `frame.size` 重建 aspect；viewport 宽高比与相机一致？
4. 一个 mesh 多份实例用 `InstanceColor` / `InstanceCutoff`，没有为每种颜色重建 mesh？
5. 透明物件用了第二个 key（blend + 不写深度 + 仍测深度 + 背面剔除）+ `ZSortedDrawing`？
6. UI source 在场景构建之前挂到 pass 下，面板在它的子树里？可复现截图把 egui style 的 `animation_time` 设 0 了（`context_mut().all_styles_mut(..)`）？
7. 行为回调里没有再取自己这个组件类型；结构变更走 `queue` + `apply`；帧行为有依赖时两端都声明了 `with_order`？
8. `spawn_context` 每个 world 只调一次；同一 key 类型只注册一次 family？
9. 跨 world 通信只用 job + `watch` / `subscribe` / `Pending`；`send` 的 `Full` / `Closed` 没被 unwrap 掉；跨 world 实体用 newtype 包了？
10. 要截图 / 快照 / MCP 就开了 offscreen 展示路径？
11. 指针用 `OnPointer`，面板先问 `egui_wants_pointer_input()`？
12. 要明暗就用 `unlit: false` 的 key，且灯 spawn 在画 mesh 的那个 source 子树里、朝向对（沿 `-Z`）？
13. MCP 大结果先落盘或挑字段读，不整表内联；所有 id 与坐标从 `list_worlds` / `scene_status` / `list_resources` 取？

## 症状 → 检查项

### 画面与渲染

| 现象                                                        | 检查什么                                                                                                                                                                             |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 什么都不画                                                  | 渲染实体或相机不在 source 的子树里；`Renderer` 只画 source 自己的子树                                                                                                                   |
| pass 完全不画                                               | 没 `set_attachments`：`FramePass` 在拿到颜色附件之前不画任何东西                                                                                                                         |
| 整片被裁掉 / 深度反了                                       | forward-z 投影配 `Greater`，或 reverse-z 投影配 `Less`                                                                                                                                   |
| pipeline 创建失败                                           | 自定义 family 没在自己的管线描述符上 `set_surface`，`SurfaceKey` 的 `sample_count` / `color_format` / `depth_stencil_format` 与这一帧的附件不一致（内建 lit family 已自动覆盖）                   |
| panic: `a pipeline family is already registered for this key type` | 同一个 world 里 `register_lit_family` 调了两次                                                                                                                                      |
| `allocate_lit_mesh` panic                                   | slice 为空、通道长度不一致、joints/weights 只给一边、morph 数量不对                                                                                                                      |
| 透明面发黑 / 错位                                           | 没开 blend、`depth_write_enabled` 没关，或没加 `ZSortedDrawing`                                                                                                                          |
| 画面拉伸                                                    | viewport 的宽高比与相机的不一致                                                                                                                                                       |
| 面板看不见                                                  | `UiPanel` 不在 `UiSource` 子树里，或 UI source 挂在场景构建之后                                                                                                                         |
| 有画面但整块纯色、没有明暗                                   | key 的 `unlit` 还是 `true`（示例的 `default_options()` 就是），或灯没 spawn 在画它的 source 子树里，或 `intensity` 为 0、朝向背了                                                  |

### 帧、输入、UI

| 现象                             | 检查什么                                                                                                                                                     |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 借用 panic                       | 行为回调里取自己这个组件类型；或同一实体同时 `query_one` 两次                                                                                                  |
| 结构变更没生效                   | 回调里 spawn/despawn 没走 `queue`，或没调 `apply`                                                                                                              |
| 面板有 draw 但一个材质都没有     | 发布方比绘制方快，携带纹理增量的那一帧被下一帧顶掉了。`ExternalFrames::publish` 会把被顶掉帧的 `textures` 交给继任者；自己绕过它换帧就会丢图集               |
| 点了按钮但场景没变               | 坐标不是物理像素、world 指错，或同一帧里连点多次（egui 每帧只结算一次）                                                                                       |

### 多 world 与通信

| 现象                                                     | 检查什么                                                                                                                         |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| job 丢了                                                 | `send` 的 `Full` / `Closed` 被忽略                                                                                              |
| 想把 `World` 装进 `Arc<Mutex<..>>` 或传去别的线程        | `World` 是 `!Send`，编不过；一个关注点一个 world，通信走 job + snapshot / stream                                            |
| 跨 world 数据时序怪、偶发竞争                           | 自造 mpsc / 锁 / 回答式 job 绕过内置时机：job 在 tick 前 drain，发布在 tick 后，单槽快照本就免锁                               |
| 处处借用冲突、状态要手动到处同步                         | 状态堆在一个巨大组件或大 enum 状态机里；按数据归属拆实体与组件，行为放行为组件，跨事件标志用兄弟组件                           |
| snapshot 一直是 `None`                                   | 目标 world 还没跑完一次 tick（发布在 tick 之后），或 reader 已 drop（drop 即退订）                                            |
| 集成测试没跑                                             | 本仓库用 `cargo nextest run`；带 required-features 的套件要 `--all-features` 或点名 feature                                  |

### MCP

| 现象                                                     | 检查什么                                                                                                                     |
| -------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `failed to deserialize parameters: missing field 'directory'` | `save_*` 系列必须给 `directory`                                                                                              |
| `the world has no renderer` / `the world has no render context` | 指名的 world 不带 `Renderer`。`unlit3dx_examples --mcp` 下要指名渲染子 world，不是 `Host`                             |
| `entity N carries no world`                              | 该实体既不带 `World` 也不带 `WorldSender`，不是可调度的 world                                                             |
| 某个 world 不在 `list_worlds` 里                         | 它没带 `WorldName`，桥接层无法给它命名，于是不报告                                                                          |
| `the world on entity N has stopped`                      | 那个 world 已经结束，job 不会再跑                                                                                            |
| 无响应                                                   | 宿主没起来（缺 DISPLAY 会 panic），或宿主没带 `--mcp`，或宿主没泵命令（无窗口宿主必须每轮调 `pump_mcp`）                 |
| 发给 stepped world 的命令一直不回                        | 宿主没在泵，或没跑 `pump_mcp` 的 job 阶段；stepped world 的队列只在推进帧时被 drain                                        |
| `the host has no exit request`                           | 该宿主没挂 `ExitRequest` 组件，`request_exit` 对它无效                                                                      |
| `the host is not an interactive headless scene`          | `step_frames` / `scene_status` 只存在于 `unlit3dx_examples --headless --mcp`                                              |
| `the world has no input writer`                          | 指名的 world 没挂 `InputWriter`；给玩法 world 发输入，不是 `Host`                                                          |
| `the world has more than one readable texture` / `the world has no readable texture to screenshot` | 不传 `id` / `target` 时服务器会猜，离屏宿主里猜不出来；用 `scene_status` 的 `target`                                    |
