# 搭建单 world 场景

**用途**：为 agent 装配一个可绘制的 world；前置阅读是[技能入口](SKILL.md)。不负责跨 world 调度或 MCP 操作。

## 最小骨架

1. 在渲染 world 安装 `RenderContext`、`Renderer` 和至少一个 `Pass`。给需要绘制的 pass 绑定目标；离屏优先用 `PersistentTarget`，窗口由 presenter 管理附件。只有创建 pass 不会得到像素。
2. 在目标 pass 的作用域下放 `Source`，在 source 作用域下放它收集的实例或 UI。确实需要多个 pass 读取同一个 source 才放入 shared 子树；无 pass 可达的普通根实体不会绘制。
3. 按宿主节奏分发输入和帧行为，再由 renderer 画当前状态；需要读回时使用可读的离屏目标并等待读回。

**可编译骨架**：[单 world README Example](../../../crates/unlit3d/README.md#example) 是完整的 mesh 装配与绘制顺序，由 `cargo test -p unlit3d --doc --all-features` 验证。[pass 测试](../../../crates/unlit3d/tests/passes.rs)中的 `a_source_no_pass_reaches_draws_nowhere` 另行检查实际 GPU 行为。

## 状态怎么放

对象状态随实体，行为从同一个作用域取兄弟组件；只把需要跨子树访问的数据放 shared。mesh 先上传一次，再把资源句柄用于多个实例；自定义顶点格式、管线、光照或外部资源先按[当前 API 查询步骤](SKILL.md#查当前版本的-api)定位扩展点。对需要比较绘制顺序的透明实例，按实际材质选择 blend/depth 策略并明确是否需要 `ZSortedDrawing`，不要用 source 的顺序替代逐实例排序。

## 相机与 viewport

帧行为收到的 `Frame` 携带步长和序号，不提供窗口或 pass 尺寸。宿主为每个 pass 维护目标和 viewport；相机行为从该 pass 的 `FramePass::target_size` 获取目标像素尺寸，有 viewport 时使用其**内容矩形**的宽高算投影 aspect。自定义 source 用本次 `SceneBuild.info` 的尺寸与 viewport，不从窗口或其他 pass 借尺寸。目标暂不可读时不要计算新投影。

给不同视图的 `FramePass` 显式绑定各自的相机。`MeshSource` 在未指定相机时才使用它能到达的首个活动 `Camera`；不要依赖多个活动相机的行序。egui 等屏幕 overlay 可以使用自己的映射。

**可编译骨架**：[pass 测试](../../../crates/unlit3d/tests/passes.rs)中的 `a_pass_uses_its_own_camera_and_content_size` 展示独立目标、内容比例与相机选择；由 `cargo nextest run -p unlit3d --all-features --test unlit3d a_pass_uses_its_own_camera_and_content_size` 验证。

## 绘制源缓存键

按缓存内容的归属从 `SceneBuild.info` 选键：相机或排序等 pass 私有状态用 `pass`；整套颜色、深度和 MSAA 附件共享的状态用 `target`；`frame` 用于判断数据是否属于本帧。`surface` 只按格式和采样数特化管线，不能识别 pass、目标或尺寸。多个 pass 可共用目标而不共用 pass 私态；resize 可以保留目标身份，按尺寸派生的缓存仍要核对实际尺寸。

## egui

在已有目标 pass 下建 `EguiSource`，面板放在该 source 子树。**可编译骨架**：[egui 模块 rustdoc](../../../crates/unlit3d/src/egui.rs)中的 `Mount the source under the pass` 示例只演示装配，由 unlit3d 的 all-feature doctest 验证；[egui GPU 测试](../../../crates/unlit3d/tests/gpu_egui_source.rs)中的 `egui_only_with_two_panels` 实际读回两块面板的像素。

面板构建可因 `request_discard` 在同一帧重复执行；不要把构建次数当 tick 次数。每帧只做一次的模拟、生成实体、发 job 应由有独立节拍的行为推进，而不是由面板构建次数驱动。

## 输入

把可达的 `InputEvents` 和 `InputState` 成对安装；宿主每批先 refresh，再 dispatch。Rust 注入按键时手动填写 `KeyEvent.location`，反射层默认值不替代结构体字段。**可编译骨架**：[输入模块 rustdoc](../../../crates/unlit3d/src/input.rs)中包含 `KeyEvent` 的示例由 unlit3d 的 all-feature doctest 验证。

## Blitz

`BlitzSource` 读取其作用域内的 `BlitzDocument`；资源由宿主按路径提供，不联网。`OnBlitzEvent` 可按事件名筛选；一个物理动作可能生成多种兼容事件，不要把回调次数当作动作次数。

输入从批初状态逐事件重放，同一文档由多个 pass 绘制时每 tick 只消费一次。鼠标、笔和各触点的按键独立，失焦会取消活动指针并清除修饰键。字符按下与对应文本合成一次事件，保留可用的 code、location 与 repeat；独立文本（如粘贴）没有物理键。文档接收预编辑和提交，但不接管平台输入法启停、剪贴板或光标形状。见 [Blitz 输入 GPU 测试](../../../crates/unlit3d/tests/blitz.rs)。

`fonts` 提供 crate 自带的字体（blitz 页面与 egui 叠加层共用同一份），`sc_fonts` 添加中文子集，`blitz_system_fonts` 才加载系统字体。跨平台比较像素时核对 feature、系统字体与页面资源；不要假定它们一致。页面通过 `@font-face` 注册的字体仅在该文档内匹配，见[字体隔离测试](../../../crates/unlit3d/src/blitz/fonts.rs)。

继续阅读：[运行示例](EXAMPLES.md) · [画面排查](TROUBLESHOOTING.md#画面与渲染)。
