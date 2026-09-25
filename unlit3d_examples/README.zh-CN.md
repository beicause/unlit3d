[English](README.md) | 简体中文

# unlit3d_examples

一个带 egui 叠加层的窗口化 unlit 立方体，通过
[`unlit3d::winit::WindowSurface`](../crates/unlit3d/README.zh-CN.md) 渲染；也可以
无头地渲染到一个离屏目标，回读后与快照比较。它既是本工作区的示例程序，也是 CI 中的
渲染回归检查。

与本示例所用到的那些 crate 一样，示例也处于**早期阶段**，并且会用到其中仍在建设中
的部分。

## 在工作区中的位置

这是工作区中唯一的二进制 crate，也是唯一一个把所有其他 crate 组合起来的消费者：它用
[`unlit3d`](../crates/unlit3d/README.zh-CN.md) 处理帧循环、ECS 组件、输入与 UI，用
[`wgpu_unlit_render`](../crates/wgpu_unlit_render/README.zh-CN.md) 获取管线选项与
资源图。它的无头路径在 `snapshot` feature 之后借用
[`wgpu_unlit_test_util`](../crates/wgpu_unlit_test_util/README.zh-CN.md) 做帧回读与
评分。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `snapshot` | 否 | 无头捕获路径：`--headless`、`--output` 与 `--snapshot`。它会引入测试骨架的帧回读与感知比较，因此窗口化示例两者都不需要，wasm 与 Android 构建也永远不会看到它 |

## 运行

```text
cargo run -p unlit3d_examples
```

会打开一个窗口，里面是一个旋转的贴图立方体和两个 egui 面板。`Esc` 关闭窗口；面板上
的按钮、复选框与滑块驱动旋转，按住左键拖动可以环绕相机。

要在浏览器中运行同一个示例——WebGPU 需要安全上下文，因此要用 `localhost` 而不是
`file://`——请用任务执行器：

```text
cargo xtask run-wasm
```

## 无头捕获

开启 `snapshot` feature 后，示例本身就是它自己的捕获工具。它不打开窗口、不跑事件
循环，而是以固定步长推进场景（因此同一条命令产生同样的画面），然后回读帧：

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --snapshot frame.webp
```

不带该 feature 时使用 `--headless` 会提示需要该 feature，并以退出码 2 结束。

### 选项

每个选项都是 `--name value` 或裸开关；`--name=value` 也接受。

| 选项 | 默认值 | 含义 |
|------|--------|------|
| `--headless` | 关 | 离屏渲染，回读帧后退出，不打开窗口 |
| `--size <WxH>` | `960x720` | 渲染目标尺寸（像素）；同时作为窗口的初始尺寸 |
| `--frames <N>` | `2` | 捕获前绘制的帧数。第二帧是 egui 首次拿到字体度量之后的帧，因此要显示排好版的文字至少需要两帧 |
| `--output <PATH>` | 无 | 把捕获到的帧以无损 WebP 写入 `PATH` |
| `--snapshot <PATH>` | 无 | 把捕获到的帧与 `PATH` 处的快照比较 |
| `--update` | 关 | 存储 `PATH`，而不是与之比较 |
| `--no-ui` | 关 | 只画立方体，不画 UI 叠加层 |
| `--min-score <S>` | `85.0` | 视为匹配的最低 SSIMULACRA2 分数 |
| `-h`、`--help` | | 打印用法说明 |

`--output`、`--snapshot` 与 `--update` 在窗口循环中没有意义，因此不带 `--headless`
使用其中任何一个都算错误，而不是被静默忽略。`--update` 还需要 `--snapshot <PATH>`。
`--no-ui` 只在无头路径生效；窗口化路径始终挂载 UI。

### 快照

`--snapshot` 用 SSIMULACRA2 比较，分数低于 `--min-score` 时以非零码退出。若快照不
存在，它会拒绝比较，并提示用 `--update` 生成一个——写入一个缺失的快照会让回归通过
CI，因为它创建的正是这项检查要读取的文件。若只想检查 3D 场景本身，配合 `--no-ui`
在窗口之外比较快照。

这正是 CI 快照任务所跑的、针对 submodule 中已提交图像的命令：

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --frames 30 \
    --snapshot wgpu_unlit_render_asset_files/snapshots/example.webp
```

确有意改动渲染结果后要重新生成它，加上 `--update`，然后在提交前审查
[`wgpu_unlit_render_asset_files`](../wgpu_unlit_render_asset_files/README.md) 中的
图像差异。该 submodule 用 `git submodule update --init` 检出。

## 测试

本 crate 唯一的测试针对手写的命令行解析器（`src/cli.rs`），它自身没有参数解析依赖：

```text
cargo nextest run -p unlit3d_examples
```

示例的渲染输出由上面那个 CI 快照任务检查，而不是由 `cargo test` 目标检查。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
