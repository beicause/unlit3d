# 排查表

**用途**：为 agent 按症状缩小范围。前提是知道当前 world、pass 或宿主；不替代专题契约和编译验证。

## 提交前自检清单

核对[可运行测试来源](EXAMPLES.md#示例索引)，使用当前版本与 feature 的[结构化索引](REFERENCE.md#查当前版本的-api) 核查调用；至少区分编译通过、实际绘制、现场宿主验证。

## 症状 → 检查项

### 画面与渲染

| 症状 | 先看 |
| --- | --- |
| 清屏正常但没有物体 | [pass 附件与 source 可达性](SCENE.md#最小骨架)，再查 mesh 资源与可见性 |
| 某个视图拉伸或用了另一只相机 | [目标、viewport 与相机](SCENE.md#相机与-viewport) |
| 多个 pass 下缓存互相污染 | [pass、目标、特化键的区别](REFERENCE.md#pass-与-source) |
| 面板不可见 | [egui 层级](SCENE.md#egui)：先核对 pass 绑定附件且能到达 source，再核对面板位于 source 子树 |

### 帧、输入、egui

- 按键不触发或指针状态不符：核对[输入装配](SCENE.md#输入)、事件批次与 feature，再区分 [Blitz 输入限制](REFERENCE.md#blitz)。
- 每帧似乎推进了多次：检查是否把逻辑放到了[可重复执行的 egui 回调](SCENE.md#egui)。

### 多 world 与通信

- Pass/renderer 句柄被拒：检查[实体的来源校验](MULTI_WORLD.md#装配规则)，不要只比实体位模式。
- 值迟迟不来、排队拒绝或溢出：按[通信表](MULTI_WORLD.md#通信协议) 区分未安装、未首次发布、队列积压、超时和 world 终态。
- 失败后仍有「成功帧」、文件中没有样本：检查[无头循环的终止、读回与消费](MULTI_WORLD.md#展示路径与离线取帧)。

### MCP

- 工具找不到或连接不 ready：从[连接与权限](MCP.md#服务器与驱动) 开始，确认实际宿主、feature 与工具发现结果。
- 图片空白、读错帧或输出太大：核对 [world 与资源定位](MCP.md#worldref-与世界名) 以及[重绘／输出方式](MCP.md#绘制)。

继续阅读：[示例索引](EXAMPLES.md) · [技能入口](SKILL.md)。
