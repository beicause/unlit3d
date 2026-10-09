[English](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md) | 简体中文

# unlit3d_examples

一个带 egui 叠加层、可切换场景的窗口化示例，通过
[`unlit3d::winit::WindowSurface`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
渲染。它的场景也会由 `tests/gpu_scenes.rs` 离屏绘制，逐帧与存储的快照比较，因此它既是
本工作区的示例程序，也是一项渲染回归检查。它同时还是 Android 示例，会打包成 APK。

与本示例所用到的那些 crate 一样，示例也处于**早期阶段**，并且会用到其中仍在建设中
的部分。

## 场景

示例的场景就是原来 `crates/unlit3d/tests` 里 GPU 测试所画的那些快照场景：现在每一个
都是可选择的场景，而 `tests/gpu_scenes.rs` 正是取代那些测试的检查。窗口化循环显示
命令行选中的场景，并在面板里列出所有场景，运行时即可切换。`--list-scenes` 打印场景表：

```text
Scenes:
  spin_cube              the example's own scene: a textured cube with two panels
  ui_only                a rich egui panel, no mesh source and no camera
  mesh_and_ui            a cube with the rich egui panel over it, one pass
  ecs_animated           a grid of cubes filling, moving and recycling over eight frames
  ecs_skinned            a cube bent by a two-joint skin over six frames
  ecs_morphed            a cube blended by two morph targets over six frames
  instanced_skinned_morph three cubes sharing one mesh, deformed per instance
  gltf                   a skinned fox and a morph-target cube playing their glTF clips
  mesh_topologies        every primitive topology, indexed and non-indexed
  transparent_zsorted    translucent panes composited back to front over opaque cubes
```

每个场景都精确重现其测试冻结下来的内容：同样的世界、相机与帧序列，因此存储的快照仍然
能验证它。`mesh_topologies` 与 `transparent_zsorted` 是两个例外：它们并非从测试移植而
来，而是补上移植场景所缺的覆盖——每种图元拓扑各画一次索引与非索引，以及重叠的半透明
绘制，其合成结果同时取决于 z 排序与混合状态。`gltf` 是另一个例外：它通过 `UnlitGltf`
绘制 `unlit3d_asset_files/assets` 下的 glTF 文档，因此加载器的蒙皮与形变目标动画是拿
真实资产来检验的，而不是单元测试里手写的那些。场景全部用公开的 `unlit3d` API 构建——
示例没有任何一处触碰到 crate 的内部实现。

## 运行方式

窗口化路径就是一个窗口应用所需的完整帧循环。渲染器作为资源实体只生成一次，场景的网格
与材质通过它的 mesh source 分配；每次 `RedrawRequested` 都获取交换链的下一个图像，把
ECS world 渲染进去并呈现。窗口尺寸变化交给 surface 处理，它会重新配置交换链，并重建
渲染器绘制所用的深度与多重采样附件。

绘制 3D 内容的场景会声明一个**基准宽高比**——960×720，即移植场景被捕获时的形状。
窗口化路径会把这类场景画进渲染目标内该宽高比所能容纳的最大矩形，并把该矩形自身的尺寸
交给场景，因此每个场景都按它真正被绘制进的那块区域来取相机宽高比。渲染目标只会给画面
加上黑边：宽窗口与窄窗口看到的是同一个画面，只是更大，而不是看到更多，且任何情况下都
不会拉伸变形。UI 仍然铺满整个渲染目标而不是那个矩形，因为面板本身没有宽高比。快照测试
会关掉这个 letterbox，直接按场景自身尺寸绘制，因此捕获结果不变。

窗口顶部居中的读数显示平滑后的帧率与帧耗时。场景选择器在右下角打开，其右下角距屏幕
边缘留出一个边距，并可从那里拖到任意位置；离开时留下的矩形会在选择下一个场景时带过去。
在浏览器中，右上角有一个加大到便于手指点按的全屏按钮：点击是浏览器唯一认可的授权手势，
因此可以随时按需进入或退出全屏，按钮的文案也随当前是否全屏而改变。
随后被竖持的设备会锁定为横屏，使画面铺满手机屏幕。该锁定依附于全屏：浏览器在退出全屏时
会释放它，而对本来就横持的设备不会加锁。若浏览器拒绝该页面进入全屏——例如位于缺少
`fullscreen` 权限的 `iframe` 中——则不绘制任何按钮，而不是给出一个只会失败的控件。

GPU 上下文是异步请求的，因为 adapter 与 device 请求本身就是异步：在 web 上它们由浏览器
的任务队列完成，所以帧循环不能阻塞等待。窗口在主线程创建——winit 只从拥有窗口的线程
交出它的原始句柄——而上下文通过事件循环的 proxy 返回，场景就在拥有 ECS world 的线程上
构建。

挂起（suspend）不会重置上述任何状态。平台会使渲染 surface 失效，在 Android 上还会销毁
它底下的原生窗口，但窗口句柄、GPU 上下文与整个 ECS world 都会保留：只有交换链被释放，
并在下次恢复时重建，因此应用会回到离开时的状态——相同的自转角、相机环绕与面板值。

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

它提供的页面会把 canvas 按视口减去一圈自己的边距来设置尺寸，而不是按示例请求的窗口
尺寸，因此示例能铺满手机屏幕，也会跟随被缩放的窗口。但 canvas 并不等于画面：每个 3D
场景都画在它的基准宽高比区域内，所以页面给 canvas 什么形状，同一幅画面都能填满它。

它既是库也是二进制，因为 Android 既不启动进程也不提供命令行：activity 加载动态库并
调用它的 `android_main`，而命令行入口是那个二进制。两者最终进入同一个窗口化循环，
因此示例只需要维护一个帧循环。

## Model Context Protocol

`--mcp` 会把示例原本要渲染的那个 world 经 stdio 服务出去，作为
[Model Context Protocol](https://modelcontextprotocol.io) 服务器。所选场景会先被构建——
`--scene` 仍然生效——随后客户端通过各工具读取并驱动这个 world。场景在独立线程上离屏渲染，
窗口照旧显示画面：第二个 world 把这张离屏纹理 blit 进交换链，因此画面随客户端更新，
而无需任何输入事件进入场景。工具与传输见
[`unlit3d_mcp`](../crates/unlit3d_mcp/README.zh-CN.md)。

```text
cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube
```

## Android

Android 启动的是一个 *activity*，而不是进程：activity 加载动态库，并在线程中调用它的
`android_main`。`android_main` 用该平台要求的 activity 构建事件循环，然后交给二进制
所驱动的那个同样的窗口化循环，从默认场景开始。

构建 APK：先交叉编译动态库，把它放进 Gradle 工程的 `jniLibs` 目录，再在那里运行
Gradle wrapper：

```text
cargo xtask build-android
```

产物是 `android/app/build/outputs/apk/debug/app-debug.apk`，可用 `adb install` 安装。
`--release` 改为构建 release APK，本工程让它保持未签名。工程的 Gradle 配置必须与任务
中的常量一致，参见
[任务执行器的 README](https://github.com/beicause/unlit3d/blob/main/xtask/README.zh-CN.md#cargo-xtask-build-android)。

activity 位于
[`MainActivity.kt`](https://github.com/beicause/unlit3d/blob/main/android/app/src/main/java/org/unlit3d/example/MainActivity.kt)。
它继承 `GameActivity`，后者会加载清单项 `android.app.lib_name` 指定的动态库并调用进去，
因此 activity 本身只负责接管屏幕。整个应用——窗口、GPU 上下文、帧循环——都在本 crate 中。

## 快照测试

`tests/gpu_scenes.rs` 是检查本示例渲染结果的地方。它不打开窗口、不跑事件循环，把每个
场景离屏画出来，以固定步长推进（因此同一份代码产生同样的画面），并把场景冻结的每一帧与
该场景存储的快照比较：

```text
cargo nextest run -p unlit3d_examples
```

它是库测试而不是命令行：同一份测试体既在原生上运行，也在浏览器中运行——因为测试二进制
正是 wasm 测试页面所加载的那个模块。`cargo xtask test` 会跑它两遍，每个设备档位一遍；
`cargo xtask test-wasm` 则让它跑在真实的 WebGL2 实现上。

### 选项

选项用 [`argh`](https://docs.rs/argh) 声明与解析。每个选项都是 `--name value` 或裸
开关；值必须作为下一个参数传入，不支持 `--name=value`。

| 选项 | 默认值 | 含义 |
|------|--------|------|
| `--scene <ID>` | `spin_cube` | 起始场景 |
| `--list-scenes` | 关 | 打印场景表并退出 |
| `--size <WxH>` | 场景自己的 | 窗口初始尺寸（像素） |
| `--mcp` | 关 | 经 stdio 服务这个 world，并在窗口中显示它的画面 |

### 播放节奏

快照测试把多帧场景的每一帧紧挨着画一遍，所以一次运行逐帧匹配快照。窗口化运行则按时间
播放：把动画冻结成若干帧的场景，每帧停留 `SEQUENCE_STEP`（当前为 0.5 秒），因此肉眼
可看，而不是随刷新率一闪而过。窗口循环每次最多推进一帧序列，卡顿不会跳帧。

自带时序的动画则不声明步长，而是按帧间隔推进。`gltf` 是这里唯一的这种场景：狐狸的
`Walk` 与形变立方体的 `Pulse` 各自按自身时长实时循环播放，因此窗口展示的是文档在
应用中的样子，而不是六帧序列。快照捕获仍然走那个序列，因为存下的帧必须可复现。

### 选择设备档位

`UNLIT3D_DEVICE_TIER` 决定测试创建的每个离屏设备请求多少 WebGPU 基线能力。不设置
时为 `webgpu`：请求有保证的基线 limits，于是设备不会超出该 API 的承诺，帧在任何支持
WebGPU 之处都能跑；只有纹理分辨率取自适配器，设备因此不会比机器所能提供的更小。
`native` 改为请求适配器自身的 limits，在桌面上即其完整的 Vulkan、Metal 或 DX12 能力。

`webgl2` 请求 WebGL2 的 limits——没有 storage buffer——并按 WebGL2 的方式录制帧：
着色器的数组用 `textureLoad` 读取，每个网格的顶点偏移被烘焙进索引。其他取值会被拒绝
而不是被猜测，因为跑错档位会「测试通过」而实际上什么都没测。

```text
UNLIT3D_DEVICE_TIER=webgl2 cargo nextest run -p unlit3d_examples
```

`WGPU_BACKEND` 按 `wgpu` 的惯例选择后端，与档位相互独立；`WGPU_BACKEND=gles`
配合该档位是桌面机器最接近浏览器 WebGL2 的组合——即便 GL 上下文并非 WebGL2，其
limits 与能力标志也是 WebGL2 的。

### 快照

每个测试会把场景声明了快照的每一帧与快照目录比较。比较用 SSIMULACRA2，分数低于容差时
失败，容差由测试为它绘制的那个场景给出。若快照不存在，它会用该帧写出一份而不是失败，
因此新测试首次运行即记录基线，`SNAPSHOT_UPDATE=1` 则重写已存在的那份。

默认容差适合整片表面的画面：回归会改变大片区域，分数因此远低于门槛。而细图元的画面
不同——它的像素就是轮廓，而某个轮廓像素属于相邻两个三角形中的哪一个，由各自实现的光栅化
tie-break 规则决定，API 把这件事留给实现。这样零星几个像素在这种画面上值几十分，所以
`mesh_topologies` 有自己的一份容差，并与那几项实测数据一同写明：它们让该界限高于所有
正确帧、低于所有出错帧。

场景自己的快照只描述「精确重现该场景存储设置」的那一次运行，因此测试按场景自身的尺寸与帧
数绘制。窗口化路径所做的 letterbox 适配由它自己的一个测试检查：在比基准更窄的渲染目标上
绘制默认场景，把适配后的画面与专门为它捕获的一份快照比较。

不匹配往往反映的是平台差异而非回归：存储图像来自某一套 GPU 栈，另一个驱动的舍入就足以
让某个场景低于门槛，而并没有任何东西出错。因此失败的帧会在测试失败前被留下——原生下写在
`UNLIT3D_SNAPSHOT_MISMATCH_DIR`，浏览器中则交给 runner（它才是拥有文件系统的进程）——
供人直接查看而不是只凭日志里的分数去猜。测试失败时 CI 会把这些目录作为 artifact 上传。

确有意改动渲染结果后，要重新生成快照，用 `SNAPSHOT_UPDATE=1` 运行，然后在提交前审查
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
中的图像差异。该 submodule 用 `git submodule update --init --checkout` 检出，
`--checkout` 是为了越过它的 `update = none`。

## 测试

本 crate 自己的测试覆盖命令行（`src/cli.rs`）与固定步长播放时钟（`src/lib.rs`），
`tests/gpu_scenes.rs` 则覆盖渲染。测试各层在整个工作区中的位置，以及 CI 所跑的内容，见
[根 README](https://github.com/beicause/unlit3d/blob/main/README.zh-CN.md#测试与基准)。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
