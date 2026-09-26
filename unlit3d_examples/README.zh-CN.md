[English](README.md) | 简体中文

# unlit3d_examples

一个带 egui 叠加层、可切换场景的窗口化示例，通过
[`unlit3d::winit::WindowSurface`](../crates/unlit3d/README.zh-CN.md) 渲染；也可以
无头地渲染到一个离屏目标，回读后与存储的快照比较。它既是本工作区的示例程序，也是 CI 中的
渲染回归检查，同时还是 Android 示例，会打包成 APK。

与本示例所用到的那些 crate 一样，示例也处于**早期阶段**，并且会用到其中仍在建设中
的部分。

## 场景

示例的场景就是原来 `crates/unlit3d/tests` 里 GPU 测试所画的那些快照场景：现在每一个
都是可选择的场景，而示例的无头路径正是取代那些测试的检查。窗口化循环显示命令行选中的
场景，并在面板里列出所有场景，运行时即可切换。`--list-scenes` 打印场景表：

```text
Scenes:
  cube                   the example's own scene: a textured cube with two panels
  ui_only                a rich egui panel, no mesh source and no camera
  mesh_and_ui            a cube with the rich egui panel over it, one pass
  ecs_animated           a grid of cubes filling, moving and recycling over eight frames
  ecs_skinned            a cube bent by a two-joint skin over six frames
  ecs_morphed            a cube blended by two morph targets over six frames
  instanced_skinned_morph three cubes sharing one mesh, deformed per instance
  transparent_zsorted    translucent panes composited back to front over opaque cubes
```

每个场景都精确重现其测试冻结下来的内容：同样的世界、相机与帧序列，因此
[`unlit3d_asset_files`](../unlit3d_asset_files/README.md) 中存储
的快照仍然能验证它。`transparent_zsorted` 是个例外：它并非从测试移植而来，而是补上移植
场景所缺的覆盖——重叠的半透明绘制，其合成结果同时取决于 z 排序与混合状态，这两者都是
不透明场景不会触及的。场景全部用公开的 `unlit3d` API 构建——示例没有任何一处触碰到
crate 的内部实现。

## 在工作区中的位置

这是工作区中唯一带二进制的 crate，也是唯一一个把所有其他 crate 组合起来的消费者：它用
[`unlit3d`](../crates/unlit3d/README.zh-CN.md) 处理帧循环、ECS 组件、输入与 UI，用
[`unlit_wgpu`](../crates/unlit_wgpu/README.zh-CN.md) 获取管线选项与
资源图。它的无头路径在 `snapshot` feature 之后借用
[`unlit_wgpu_test_util`](../crates/unlit_wgpu_test_util/README.zh-CN.md) 做帧回读与
评分。

它既是库也是二进制，因为 Android 既不启动进程也不提供命令行：activity 加载动态库并
调用它的 `android_main`，而命令行入口是那个二进制。两者最终进入同一个窗口化循环，
因此示例只需要维护一个帧循环。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `snapshot` | 否 | 无头捕获路径：`--headless`、`--output` 与 `--snapshot`。它会引入测试骨架的帧回读与感知比较，因此窗口化示例两者都不需要，wasm 与 Android 构建也永远不会看到它 |

## 运行

```text
cargo run -p unlit3d_examples
```

会打开一个窗口，里面是默认场景——一个旋转的贴图立方体和两个 egui 面板——外加一个列出
所有场景的面板，运行时即可切换。`Esc` 关闭窗口；立方体面板上的按钮、复选框与滑块驱动
旋转，按住左键拖动可以环绕相机。也可以从命令行选场景：

```text
cargo run -p unlit3d_examples -- --scene ecs_skinned
```

要在浏览器中运行同一个示例——WebGPU 需要安全上下文，因此要用 `localhost` 而不是
`file://`——请用任务执行器：

```text
cargo xtask run-wasm
```

## Android

Android 启动的是一个 *activity*，而不是进程：activity 加载动态库，并在线程中调用它的
`android_main`——这正是本 crate 既是库也是二进制的原因。`android_main` 用该平台要求
的 activity 构建事件循环，然后交给二进制所驱动的那个同样的窗口化循环，从默认场景开始。

构建 APK：先交叉编译动态库，把它放进 Gradle 工程的 `jniLibs` 目录，再在那里运行
Gradle wrapper：

```text
cargo xtask build-android
```

产物是 `android/app/build/outputs/apk/debug/app-debug.apk`，可用 `adb install` 安装。
`--release` 改为构建 release APK，本工程让它保持未签名。工程的 Gradle 配置——它的
`compileSdk`、`minSdk` 和唯一的 ABI——必须与任务中的常量一致，参见
[`xtask/README.zh-CN.md`](../xtask/README.zh-CN.md#cargo-xtask-build-android)。

activity 位于
[`android/app/src/main/java/org/unlit3d/example/MainActivity.kt`](../android/app/src/main/java/org/unlit3d/example/MainActivity.kt)。
它继承 `GameActivity`，后者会加载清单项 `android.app.lib_name` 指定的动态库并调用进去，
因此 activity 本身只负责接管屏幕。整个应用——窗口、GPU 上下文、帧循环——都在本 crate 中。

## 无头捕获

开启 `snapshot` feature 后，示例本身就是它自己的快照运行器。它不打开窗口、不跑事件
循环，而是以固定步长推进场景（因此同一条命令产生同样的画面），并把每一帧与该场景存储
的快照比较：

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --scene ecs_skinned
```

不带该 feature 时使用 `--headless` 会提示需要该 feature，并以退出码 2 结束。
`--scene all` 运行所有场景，CI 就是这么跑的。

### 选项

选项用 [`argh`](https://docs.rs/argh) 声明与解析（与 `xtask` 同一个库），每个选项都是
`--name value` 或裸开关；值必须作为下一个参数传入，不支持 `--name=value`。

| 选项 | 默认值 | 含义 |
|------|--------|------|
| `--headless` | 关 | 离屏渲染，回读帧后退出，不打开窗口 |
| `--scene <ID>` | `cube` | 要运行的场景；`all` 运行全部（仅无头模式） |
| `--list-scenes` | 关 | 打印场景表并退出 |
| `--size <WxH>` | 场景自己的 | 渲染目标尺寸（像素）；同时作为窗口的初始尺寸 |
| `--frames <N>` | 场景自己的 | 捕获前绘制的帧数。场景自己的帧数就是快照存储时的帧数；`--frames` 覆盖它 |
| `--output <PATH>` | 无 | 把捕获到的帧以无损 WebP 写入 `PATH` |
| `--snapshot <PATH>` | 无 | 把捕获到的帧与 `PATH` 处的快照比较，而不是与场景自己的快照比较 |
| `--update` | 关 | 存储正在比较的快照，而不是与之比较 |
| `--no-ui` | 关 | 只画场景的 3D 内容，不画它的 UI 叠加层 |
| `--min-score <S>` | `85.0` | 视为匹配的最低 SSIMULACRA2 分数 |
| `--snapshot-dir <D>` | 资产 submodule 的 `snapshots` | 场景自己的快照按名解析的目录 |
| `-h`、`--help` | | 打印用法说明与场景表 |

`--output`、`--snapshot`、`--update` 与 `--scene all` 在窗口循环中没有意义，因此不带
`--headless` 使用其中任何一个都算错误，而不是被静默忽略。`--scene all` 不能与
`--output` 或 `--snapshot` 组合。`--no-ui` 只在无头路径生效；窗口化路径始终挂载 UI。

### 播放节奏

无头捕获把多帧场景的每一帧紧挨着画一遍，所以一次运行逐帧匹配快照。窗口化运行则按时间
播放：把动画冻结成若干帧的场景，每帧停留 `SEQUENCE_STEP`（当前为 0.5 秒），因此肉眼
可看，而不是随刷新率一闪而过。窗口循环每次最多推进一帧序列，卡顿不会跳帧。连续动画的
场景（例如自转的立方体）不受影响，它们本来就按帧间隔推进。

### 快照

不带 `--snapshot` 时，无头运行会把场景声明了快照的每一帧——多帧场景的整个序列——与
快照目录比较。比较用 SSIMULACRA2，分数低于 `--min-score` 时以非零码退出。若快照不
存在，它会拒绝比较，并提示用 `--update` 生成一个——写入一个缺失的快照会让回归通过
CI，因为它创建的正是这项检查要读取的文件。

场景自己的快照只描述“精确重现该场景存储设置”的那一次运行。因此覆盖 `--size`、
`--frames` 或 `--no-ui` 得到的是一次自定义捕获：它可以用 `--output` 写出，但不会与
场景自己的快照比较；要比较请显式用 `--snapshot <PATH>`。

```text
cargo run -p unlit3d_examples --features snapshot -- --headless --scene all
```

这正是 CI 快照任务所跑的、针对 submodule 中已提交图像的命令。确有意改动渲染结果后，
要重新生成其中一份或全部，加上 `--update`，然后在提交前审查
[`unlit3d_asset_files`](../unlit3d_asset_files/README.md) 中的
图像差异。该 submodule 用 `git submodule update --init` 检出。

## 测试

本 crate 自己的测试覆盖命令行（`src/cli.rs`）与固定步长播放时钟（`src/lib.rs`）：

```text
cargo nextest run -p unlit3d_examples
```

示例的渲染输出由上面那个 CI 快照任务检查，而不是由 `cargo test` 目标检查。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
