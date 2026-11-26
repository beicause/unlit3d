# 装配多个 world

**用途**：为已选用 `unlit3dx` 的 agent 选择装配、驱动和通信方式。先读[技能入口](SKILL.md)，单个渲染 world 的结构仍看[场景教程](SCENE.md)。不复制当前 API 签名或 MCP schema。

## 两种骨架

- **用 `App`**：把渲染、游戏、物理和音频的初始化交给 runner，窗口、无头、传输和驱动力由 `RunShape` 决定。实现 `App::Args`，无额外参数用 `NoArgs`；如果 helper 返回 `(renderer, pass)`，解构后返回 `RenderSetup { renderer, pass }`。
- **自己拥有驱动循环**：根据执行位置选子 world、threaded world 或 stepped world。初始化、推进和关闭是三件事；host-owned 不等于能在另一个线程访问，stepped 也不等于队列自动运行。

**可编译骨架**：[Demo](../../../unlit3dx_examples/src/demo.rs) 的 `impl App for Demo` 及[无头测试](../../../unlit3dx_examples/tests/headless.rs)的 `frames_match_the_baselines` 可复制相应装配顺序，依据 `cargo check -p unlit3dx_examples --all-targets --all-features` 和该 crate 的 Nextest 验证。Demo 有自定义参数和场景数据，不是无依赖程序。

## 装配规则

`World` 留在创建它的线程；向外传 `WorldEndpoint` 与拥有所有权的数据，不传 scope、组件借用或裸 `Entity`。跨 world 的实体从生产它的 world 用 `World::remote` 包装成 `RemoteEntity`，然后在目标操作对应的 world 调 `World::resolve`；只有成功才绑定 pass 或查询 renderer。按角色加 newtype 可以辅助选对字段，但不能替代来源与存活校验。实际拒绝行为见 [ECS 测试](../../../crates/unlit_ecs/src/world/tests.rs)中的 `a_handle_from_another_world_is_turned_down`。

## 通信协议

| 需要什么 | 使用 | 存储与结果 | 谁推进 |
| --- | --- | --- | --- |
| 安排操作 | `JobSender` | 队列满 `Full` 或关闭 `Closed` 时返还未运行的 job；`Ok` 只代表入队 | 目标 world 的驱动 |
| 观察最新状态 | `watch` / `SnapshotReader` | 只保留最新；`Uninitialized`、`Unchanged`、`Updated`、`Closed`、`Failed` 可区分，终态后 `latest` 仍可读 | 目标发布，宿主轮询 |
| 顺序观察事件 | `subscribe` / `StreamReader` | 每订阅独立有界；慢 reader 溢出后先取已接纳批，再看到 `Overflow` | 目标发布，宿主及时 `pump` |
| 一次请求一个结果 | `Pending` | `Answered`、`TimedOut`、`WorldStopped`、`WorldFailed`、确认的 `Cancelled` | 目标执行，发起方轮询 |

- `watch` / `subscribe` 先拿到 `Subscription`，还不是已安装的 reader。驱动目标后再 `poll`；安装仍在排队时没有结果，world 提前结束或 outlet 不可达会报告错误。已安装但 collector 未发布，与安装仍在排队，是不同情况。
- `Pending` 绑定一个 `WorldEndpoint`。同 key 的后续请求 `Coalesced`，不运行第二个闭包，也不延长首请求截止时间。超时按墙钟，并不撤销 job；只有协作任务在检查点确认才是 `Cancelled`。重试可能重复副作用，先确认幂等或添加业务去重。不同 key 的结算顺序不保证。
- 入队拒绝可以用返还的 job 自行决定何时重投；已入队后丢失不能据此推定任务执行过。上述机制不是严格无锁协议，不可同步阻塞等待由自己驱动的 stepped world。

## 展示路径与离线取帧

只在 `StepOutcome::Ran` 后记成功帧、消费当轮混音或做捕获。`Finished` 和 `Stopped` 停止循环；`Failed(error)` 向上保留原因，不继续补成功帧。图像读回需要等待 future，再检查 `Result`；只有成功的像素可进入输出。

要录音时从 audio world 取得 `AudioFormat`，按采样率和声道消费 `AudioPull` 交出的交错样本；步进本身不是消费。内置无头驱动通过 `SteppedRun` 拉取并累积完整声道帧，[无头驱动](../../../crates/unlit3dx/src/app/headless.rs)的 `headless` 分开处理终止、音频和像素；[无头集成测试](../../../unlit3dx_examples/tests/headless.rs)中的 `an_ended_or_failed_turn_does_not_capture_a_frame`、`an_undrawn_frame_matches_a_drawn_one` 和 `audio_matches_the_baseline` 分别验证终止行为、跳过绘制和实际消费的音频样本。仅需要图像时不必自建音频混音器。

继续阅读：[示例与测试](EXAMPLES.md) · [通信异常](TROUBLESHOOTING.md#多-world-与通信) · [现场取图](MCP.md#绘制)。
