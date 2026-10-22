[English](README.md) | 简体中文

# unlit3d

[![Build](https://github.com/beicause/unlit3d/actions/workflows/ci.yml/badge.svg)](https://github.com/beicause/unlit3d/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](https://github.com/beicause/unlit3d)
[![Cargo](https://img.shields.io/crates/v/unlit3d.svg)](https://crates.io/crates/unlit3d)
[![Documentation](https://docs.rs/unlit3d/badge.svg)](https://docs.rs/unlit3d)

面向 WebGPU 的紧凑、可拓展的、有主见的 3D 渲染器，以及构建在其上的
ECS 层。内置**无光照（unlit）**渲染管线，移动端优先；不支持 WebGL 与 GLES。
它直接使用并暴露 `wgpu` 资源，允许底层控制和拓展，带有很少的CPU端的高级封装。

它主要面向拥有图形渲染知识的开发者和编程智能体。与主流的面向大众用户的游戏引擎不同，
它不做高级别的封装，你需要有 WebGPU 知识才能较好地运用它。

项目处于**极早期开发阶段**。API 会自由变动；请把每个 crate 都视为进行中的产物。

<details>
<summary>本项目优化什么，以及有意舍弃什么</summary>

- **轻量、可自定义。** 没有繁重的依赖，较快的编译时间，对 AI Agent 友好。精简 ECS
  范式并借鉴 OOP，更多地使用行为组件替代系统，将关注点放在对象上。
- **移动和 Web 优化。** 默认瞬态 MSAA 纹理、瞬态深度纹理，顶点属性压缩，默认无光照
  材质，以及少量显式的渲染 pass。没有对移动端较为昂贵的 prepass、PBR 光照、阴影，
  没有使用计算着色器。
- **底层。** 直接使用 wgpu 资源，允许直接操作缓冲。渲染资源跨帧保留，资源图管理使得
  资源只在必要时重建。
- **确定性的渲染。** 一流的无头渲染、CI 自动化快照测试。

不支持：光照与阴影，以及后处理。

</details>

## 功能一览

- **移动端优先，且跨平台。** 渲染 pass 是显式的——通常一个，需要合成时更多——配以
  瞬态深度与多重采样纹理、压缩顶点，没有 prepass、计算着色器、光照与阴影。同一个
  帧循环既驱动窗口，也驱动无头离屏目标、Web 示例与 Android APK。
- **跑在 WebGL2 与 WebGPU 基线设备上。** 适配器在运行时挑选：优先 WebGPU，平台只有
  WebGL2 时回退过去。WebGL2 缺少 WebGPU 基线的若干部分——没有 storage buffer、没有
  `base_vertex`——于是着色器用 `textureLoad` 读取本帧的数组，网格的顶点偏移被烘焙进
  索引；而具备完整基线的设备仍走 storage buffer 与整缓冲绑定，不额外增加 pass 状态
  切换。走哪条路由适配器自身的 downlevel 能力决定，而非编译期 feature。
- **由调用者组合的场景。** 一帧是一串 pass，每个 pass 都是普通实体，携带自己绘入的
  view；每个 pass 录制其下的帧源，这些帧源各自贡献绘制并声明自己在该 pass 中的位置。
  内置网格渲染、egui 叠加层与调用者自己的源权限完全相同。
- **精简的 ECS。** 借鉴 OPP 重视对象状态，实体原型不可变，系统由外部驱动而非内置，
  行为是持有闭包的组件而非 system，参见[unlit_ecs](./crates/unlit_ecs/README.zh-CN.md)。
  可渲染实体把网格、材质与管线作为组件携带，游戏逻辑与绘制共用同一套模型。
- **按变体组合的 unlit 管线。** 着色器变体只含网格实际使用的通道——位置、UV、顶点色、
  逐实例变换与颜色、基础色纹理、蒙皮、形变目标——并针对本帧的渲染目标特化。
- **自定义着色器与逐实例数据。** 调用者自己的家族通过与内置家族同一个调用注册，用自己的
  管线绘制——手写的 `wgpu` 管线，或用本库公开着色器包组合出的 WESL 模块——并拥有
  自己的逐实例顶点流，由内置 unlit 家族所实现的同一个 `InstanceData` trait 描述。
- **可选的 glTF 加载。** `unlit3d` 的 `gltf` feature 把一个 glTF 2.0 文档加载为纯粹的
  模型记录——没有 `World`，没有 GPU 状态——并把它的图像、材质与网格修补进你已在渲染
  的世界的资源图，再卸载它们，并生成绘制文档默认场景的实体。节点层级、蒙皮与形变
  primitive、混合或裁剪材质，以及文档中的动画——在你选定的时间采样——都会一并带过来。
  相机不会：渲染器用世界自己的 `Camera` 绘制，由调用方摆放。
- **常驻且池化的 GPU 资源。** 直接使用 `wgpu` 资源；跨帧保留，按需重建，并共享复用
  上传所经过的缓冲。
- **CPU 视锥剔除与自动实例化。** 屏幕外的网格不产生开销，状态相同的绘制折叠为一次
  实例化绘制。
- **egui 与可移植输入。** egui 可叠加于渲染之上，也可渲染到单独纹理；输入统一，由
  类似 OOP 风格的回调驱动。
- **确定性渲染，CI 快照测试。** 同一场景离屏渲染的结果每次都完全相同。三个桌面平台上
  各自 lint、构建与测试，再把示例的每个场景渲染出来并与存储图像做 SSIMULACRA2 比对；
  wasm 与 Android 构建是另外两个独立 job。
- **内置MCP支持。** 可将MCP服务器嵌入应用程序，让AI Agent读写组件、截图。服务器不携带任何
  引擎知识：每个 crate 各自注册作用于它所定义类型的工具。

## Crate 一览

| Crate | 职责 |
|-------|------|
| [`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) | 底层渲染器：资源图、顶点压缩、缓冲池、staging、声明式 `Scene`、内置 unlit 管线与 egui 后端。不认识 ECS。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 高层渲染 API：ECS 组件、帧源、输入、UI 与 winit 呈现，以及可选的 glTF 加载器，把模型修补进另一个世界的 `MeshSource`。 |
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | 高层所用的精简 archetype ECS：没有变化检测、事件、关系或调度器。它还带有把运行中的 world 经 stdio 暴露出去的 MCP 服务器，各 crate 各自为它扩展工具。 |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.zh-CN.md) | 无头 GPU 测试骨架：设备初始化、缓冲与纹理回读、SSIMULACRA2 快照。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 可切换场景的窗口化示例，其 `tests/gpu_scenes.rs` 把场景与快照比较；也是 Android APK 所打包的两个示例之一。 |
| [`unlit3d_asset_bytes`](unlit3d_examples/asset_bytes/README.zh-CN.md) | `unlit3d_examples` 所绘制的模型，随身带在二进制里：`include_bytes!` 写在 `#[inline(never)]` 的函数体内，这些字节因此不进任何下游的元数据。 |
| [`unlit3dx_asset_bytes`](unlit3dx_examples/asset_bytes/README.zh-CN.md) | `unlit3dx_examples` 所演奏的音色库，同样随身携带；文件缺失时其 `build.rs` 换上一个空的 stub。 |
| [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) | 把 glTF 文档渲染成图片的命令行工具，由 JSON 配置文件驱动、并可被自身选项覆盖；其 `tests/gpu_cli.rs` 是端到端快照检查。 |
| [`unlit3dx`](crates/unlit3dx/README.zh-CN.md) | 多 world 运行时：以 job 与快照信箱连接渲染子 world 与物理、音频、玩法 world，各占一条线程或由宿主的帧步进。 |
| [`xtask`](xtask/README.zh-CN.md) | `cargo xtask` 背后的任务执行器。 |
| [`unlit3d_benchmarks`](unlit3d_benchmarks/README.zh-CN.md) | 帧路径的基准测试：Criterion 吞吐量数字，以及 `profiling` 跨度报告的阶段耗时。 |

## 各部分的配合方式

`unlit_wgpu` 是基础，不依赖工作区中的任何其他 crate。`unlit3d` 构建在它和
`unlit_ecs` 之上，并把两者保留为直接依赖：`prelude` 重导出大多数调用者需要的条目，
其余条目仍可通过各自的 crate 路径访问。

两条原则决定了这种分层：**内置管线不享有特权**，它由调用者自建管线所用的同一批公开
设施组合而成；**帧源彼此平等**，内置网格源与调用者自己的绘制趟次没有任何差别。

各 crate 的 README 记述它自己那部分的设计取舍：
[`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) 讲资源图、声明式场景、管线特化与
逐帧上传；[`unlit3d`](crates/unlit3d/README.zh-CN.md) 讲帧模型、unlit 管线、UI 与
输入；[`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) 讲 ECS 为何如此精简。

## 编码原则

- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行
  自定义和拓展。本库的一些内置实现（如 unlit 渲染）不应该拥有特权和内部专用实现，
  内部实现应该挪到外部以保证本库的可自定义性和可拓展性。

## 常用命令

常用命令遵循`cargo xtask`约定：

- **`cargo xtask check`** — 先以默认 features、再以全 feature 对全工作区跑 clippy，
  另外每个包各自以默认 features 单独跑（`-D warnings`）；随后 `cargo fmt --check`。
  提交前的门槛。逐包那一趟能抓出某个 crate 自身代码已不再使用的可选 feature——
  全工作区一起跑会因 feature 统一而掩盖它。加 `--release` 走 release profile。
- **`cargo xtask test`** — 用 `cargo nextest run` 覆盖单元与集成测试，随后
  `cargo test --doc` 补上 nextest 不跑的 doctest。加 `--release` 走 release profile。
- **`cargo xtask test-wasm`** — 把 GPU 测试构建到 `wasm32-unknown-unknown`、做
  bindgen，再经由 WebGL2 在真实浏览器里逐个测试各开一个页面运行；随后加载每个示例并
  检查它确实画出了东西。需要与工作区解析版本一致的 `wasm-bindgen-cli`、`cargo nextest`
  与 Node；它会自行安装 runner 的依赖与一个浏览器，除非用 `CHROME_PATH` 指定现成的。
  `--show` 打开可见窗口以观察失败。
- **`cargo xtask run-wasm`** — 构建 web 示例并用内置静态服务器提供（WebGPU 需要 secure
  context，`file://` 不行）。`--app` 选择构建哪些——`unlit3d`、`unlit3dx` 或默认的
  `all`——根目录的首页会链接已经构建出来的那些；`--no-serve` 只构建。这里和
  `test-wasm` 的每一次 wasm
  构建都用工作区的 `wasm-dev` cargo profile：沿用 release 的设定，但 `opt-level`
  以体积为目标，因为二进制是要被浏览器下载的。
- **`cargo xtask build-android`** — 先 `cargo ndk` 交叉编译两个示例的动态库
  （`unlit3d_examples_android` 与 `unlit3dx_examples_android`，两者只放 Android
  入口）放进 `android/app/src/main/jniLibs`，再调 Gradle 构建 APK。默认 debug，
  `--release` 构建未签名的 release APK。需 JDK 17+ 与 `ANDROID_HOME`
  （NDK 由 cargo-ndk 自动探测）。
- **`cargo xtask publish`** — 按依赖顺序把可发布的 crate（`unlit_ecs`、`unlit_wgpu`、
  `unlit3d`）上传到 crates.io，每个都等到它出现在索引里再打包下一个。
  `--` 之后的参数会透传给每次 `cargo publish`，因此 `-- --dry-run` 只做全部检查、
  不上传。
- **`cargo nextest run`** — 需要按名筛选或重跑单个测试时直接用（`-p <crate>`、
  `-E 'test(<name>)'`）。nextest 不跑 doctest，也别用它代替 `cargo xtask test`。
- **`cargo bench -p unlit3d_benchmarks`** — 把渲染路径的一帧按每秒实体数计时。
  用 `--bench ecs` 则单独测量 ECS 的组件访问路径；加 `--features profile-tracing`
  并以 `--bench profile -- <实体数> <帧数>` 运行时，改为每帧每个阶段打印一行。各目标
  覆盖什么见 [`unlit3d_benchmarks`](unlit3d_benchmarks/README.zh-CN.md)。
- **`typos`** — 拼写检查，扫全仓库。
- **`tombi lint --error-on-warnings`** 与 **`tombi format`** — TOML 的 lint 与格式检查。
  改过任何 `Cargo.toml` 后务必跑。

## 测试与基准

测试按「离被测代码有多近」分三层：

- **单元测试**：在各 crate 的 `src/` 内（`#[cfg(test)]`），只测私有纯逻辑，不碰 GPU，
  随 `cargo nextest run` 直接运行。
- **库集成测试**：在各 crate 的 `tests/` 下，只经公开 API 使用它。`unlit_ecs` 的是纯
  ECS 行为；两个渲染 crate 的是 GPU 测试——用
  [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.zh-CN.md) 建无头设备，
  离屏渲染后回读像素断言，但不与存储图像比较。
- **快照测试**：把一帧（或多帧序列）与存储图像做 SSIMULACRA2 感知比较。低层 API 的
  快照留在 `unlit_wgpu` 的测试里；高层 ECS 场景的快照由
  [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) 的 `tests/gpu_scenes.rs` 运行
  （`SNAPSHOT_UPDATE=1` 重新生成），因为它既是示例也是 CI 的渲染回归检查；而
  [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) 的 `tests/gpu_cli.rs` 通过命令行工具的库
  渲染同一个参考 glTF 场景，与同一批快照比较。

快照基线都在 [`unlit3d_asset_files`](unlit3d_asset_files/README.md) 这个 git submodule
中。它被标记为 `update = none`：把本库当作 git 依赖的项目不会去拉取用不到的快照；
而这里测试确实要用，所以在本仓库用 `git submodule update --init --checkout` 拉取。
改动渲染结果后重新生成对应快照，并审查图像差异再提交。由于基线来自同一套 GPU 栈，
换一个平台的驱动就可能让某个场景低于阈值
而并没有出错；因此不匹配的帧会在测试失败前被写下（每一趟各写进 `target/` 下的一个
目录），CI 再把这些目录作为 artifact 上传。

### 运行

```text
cargo xtask test                 # 整个工作区：先 nextest，再 doctest
cargo nextest run -p unlit_wgpu  # 单个 crate
cargo nextest run -E 'test(name)'  # 单个测试
cargo xtask test-wasm            # 同一批 GPU 测试，再在浏览器里跑一遍
```

`nextest` 不跑 doctest，因此 `cargo xtask test` 会在其后追加 `cargo test --doc`。
用 `cargo nextest run` 筛选只适合迭代时用，它不能替代该任务；CI 跑的就是该任务，
所以两者不会脱节。

`cargo xtask test-wasm` 把渲染 crate 的 GPU 测试再跑一遍：构建到
`wasm32-unknown-unknown`，经由 WebGL2 在真实浏览器中运行。测试文件是同一批；
`harness = false` 的测试 target 本身就是那个 wasm 模块，骨架的宏给它生成页面要调用的
导出。浏览器提供的是无头设备给不了的东西——一个真实的 WebGL2 实现，带着它自己的
limits、自己的着色器翻译和自己的光栅化器。由于该光栅化器并不是采集快照时所用的那个，
`Tolerance` 可以指定一帧允许相差多少；立方体快照正是这么用的，并附有其阈值的实测依据。

它还会在浏览器里启动每个示例，并检查画布上留下的是一幅有明暗的画面、而不是一片空白。
GPU 测试从不会启动示例，因此没有这一步的话，某个「能编过、但页面仍是空的」改动会照样
通过。

各 crate 的测试覆盖：

| Crate | 测试覆盖的内容 |
|-------|----------------|
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | world 与查询行为、延迟命令，以及 MCP 服务器的 ECS 工具。 |
| [`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把网格渲染到离屏纹理，与 `tests/snapshots` 下的快照比较（该目录是指向 asset submodule 的软链接）。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把场景渲染到离屏目标并检查回读的像素。一个端到端 MCP 测试在一条线程上建起真实的 GPU world，用一对流把它服务出去，再用 JSON-RPC 与它对话——列出工具、读取 world、生成网格，再逐像素检查截图。其多帧快照覆盖位于 `unlit3d_examples`。 |
| [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.zh-CN.md) | 骨架自身的行为——快照比较、以及差异像素如何计数。它支撑的 GPU 测试位于上述 crate 中。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 命令行（`src/cli.rs`）、固定步长播放时钟（`src/lib.rs`），以及 `tests/gpu_scenes.rs` 的渲染快照。 |
| [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) | 参数解析与配置合并，以及端到端渲染测试：复现示例的参考 glTF 场景并与存储快照比较。 |
| [`unlit3dx`](crates/unlit3dx/README.zh-CN.md) | 多 world 核心：job 往返、请求回复、快照替换、线程停止与 panic 报告、render 桥的失败路径，以及物理、音频与 MCP 各桥的端到端测试。 |
| [`unlit3dx_examples`](unlit3dx_examples/README.zh-CN.md) | 示例场景逻辑的单元测试，以及一次端到端的离屏运行：捕获的帧与混音同资产子模块下的快照基准比较；同一批 world 也会在浏览器中步进运行，并处在一条 Android activity 之后。 |

渲染 crate 的集成测试与示例都需要可用的 GPU。在无头 CI runner 上，用 Mesa 的
`lavapipe` 作为软件 Vulkan 实现。wasm 那一遍不需要 GPU：浏览器会回退到软件 WebGL2。

## 基准测试

[`unlit3d_benchmarks`](unlit3d_benchmarks/README.zh-CN.md) 下有三个目标：`frame` 测
吞吐量，`ecs` 测一次组件读取的成本，`profile` 给出帧内各阶段的分解。它不属于测试运行：
基准二进制没有测试 harness，
因此被排除在 `cargo xtask test` 之外，改由 `cargo bench` 驱动。各目标覆盖什么、如何
读它的输出，见[该 crate 的 README](unlit3d_benchmarks/README.zh-CN.md)。

## 持续集成

上述检查以及各 lint 关卡分成两档，并拆到两个 workflow 里，好让两档共有的任务只写一遍：
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) 是轻量档，
[`.github/workflows/ci-main.yml`](.github/workflows/ci-main.yml) 是整份，它带上 `full` 调用
轻量档的任务，再补上只有发行才需要的部分。跑哪一档由这一次运行所在的 ref 决定，
而不由是谁触发的决定：`main` 与合并队列（不论 ref 写成什么，那都是一次 `main` 的运行）
跑整份，其余分支跑轻量档。因此，从分支开出的 pull request 算在内，没开 pull request、
只是推上去的分支也算在内——推上去就够了，开 PR 并不是启动构建的那个动作。

轻量档跑的是改动自己就能弄坏的东西，并且要落在审阅者读这一遍代码的时间里：只跑一个平台，
web 与 Android 只跑到 lint 而不是它外围的浏览器或 APK。剩下的由 `main` 跑，那是证明一份
发行物能出得去的那部分。

`main` 以外的每个分支，无论是自己推上去的还是开成 pull request 的：

- **lint** —— 对全部文件跑 `typos`；对 TOML 跑 `tombi lint --error-on-warnings` 与
  `tombi format --check`；以及 `cargo fmt --all -- --check`。
- **build**（Linux）—— 带与不带 `unlit` feature 的 clippy、
  `cargo build --workspace --all-targets`、`cargo xtask test`、带 `-D warnings` 的
  `cargo doc`，以及工作区的快照比较——该比较跑两遍：一遍用 WebGPU 基线的 limits，
  一遍把所有离屏设备收窄到 WebGL2 的形态。由于 runner 没有 GPU，Linux 会安装 Mesa
  以提供 `lavapipe`。
- **check-wasm** —— 为 `wasm32-unknown-unknown` 对工作区与测试二进制各跑一遍
  clippy。它排除 `xtask`：那是只在宿主机上运行的任务运行器，其 HTTP 服务器
  无法为该目标编译。
- **check-android** —— 为 `aarch64-linux-android` 跑 clippy，这是唯一会编译
  `cfg(target_os = "android")` 后面代码的检查；`build` 里的宿主 clippy 到不了那里。

`main` 上——每次推送到它、合并队列的一次运行，以及从它手动触发的运行，都是整份：

- **build** 除 Linux 外还在 macOS 与 Windows 上跑。
- **build-wasm** —— 对 `wasm32-unknown-unknown` 构建跑同样的两遍 clippy，然后把
  每个 wasm 测试二进制 bindgen 成页面，在真实浏览器中驱动。既然这里已经 lint 了这两次
  构建，`main` 上就不必再跑 `check-wasm`。
- **build-android** —— 先跑 `aarch64-linux-android` 的 clippy，再交叉构建动态库，并在
  其外套上 Gradle 得到 APK。它自己就跑了那道 clippy，所以 `main` 上同样不必再跑
  `check-android`。

两个 `check-*` 任务各自与 `main` 上做同一件事的任务共用一个缓存
（Swatinem/rust-cache 的 `shared-key`），因此轻量档编译出来的东西，合并后是
被还原而不是被重编。每个任务即便失败也会保存它的缓存（`cache-on-failure`），
因此在失败的分支上迭代时，下一次运行仍比冷启动要暖和。

快照比较正是让渲染回归会失败而不是悄然通过的那道检查，这也是示例的场景承担它的原因。
它的第二遍通过设置 `UNLIT3D_DEVICE_TIER=webgl2`（而不是为 Web 构建）来触达 WebGL2
路径，即便 runner 自身的适配器有 storage buffer 与 `base_vertex`。`build-wasm` 则用
同一批测试体，在真实浏览器中跑同样的比较。

比较失败的帧会在测试失败前被写下，因此 CI 中失败的运行会把它留在 `target/` 下各自
的目录里，由任务作为 artifact 上传。不匹配往往反映的是平台差异而非回归，所以拿到这
一帧，才能让人直接查看差异，而不是从分数去推断。

[`.github/workflows/pages.yml`](.github/workflows/pages.yml) 在每次推送到 `main` 时
把站点发布到 GitHub Pages：根目录是一个首页，链接两个 web 示例与 API 文档，每个示例
各占旁边的一个目录，工作区的 rustdoc 放在 `docs/` 下；`unlit3d` 示例的页角也链接到这些
文档。文档用 nightly 构建，因为列出各 crate 的索引页与 `doc(cfg(...))`
标签都只存在于 nightly；`ci.yml` 仍在 stable 上检查文档，好让问题在分支上
就被发现，而不是等到发布时。让文档警告弄挂构建的正是这道检查——发布这一侧只要它产出的
页面，因为一个链接坏掉的页面也比被它挡下、什么都没发出去的站点有价值。

## 工作区结构

```text
crates/unlit_wgpu/           渲染器、它的 WESL 着色器与 GPU 测试
crates/unlit3d/              与 ECS 集成的渲染 API
crates/unlit_ecs/            archetype ECS
crates/unlit_wgpu_test_util/ 共享的 GPU 测试骨架
crates/unlit3dx/             多 world 运行时：job、快照、线程化与步进的 world
unlit3d_examples/            窗口化示例、它的场景与场景的快照测试
  asset_bytes/               它嵌入的 glTF 模型，单独成 crate
unlit3d_examples_android/    Android activity 所加载的动态库
unlit3dx_examples/           多 world 示例：五个 world、一个窗口、一个 web 页面与一条 Android activity
  asset_bytes/               它嵌入的音色库，单独成 crate
unlit3dx_examples_android/   该示例供自己的 activity 加载的动态库
unlit3d_cli/                 glTF 转图片的命令行工具与端到端测试
unlit3d_benchmarks/          帧路径基准测试与剖析运行
android/                     把两个示例打包成 APK 的 Gradle 工程
xtask/                       cargo xtask 任务执行器
```

## 许可证

双许可：MIT 或 Apache-2.0，任选其一，如工作区 manifest 所声明。
