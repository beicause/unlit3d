[English](README.md) | 简体中文

# unlit3d

面向 WebGPU 的紧凑、可拓展的、有主见的 3D 渲染器，以及构建在其上的
ECS 层。内置**无光照（unlit）**渲染管线，移动端优先；不支持 WebGL 与 GLES。
它直接使用并暴露 `wgpu` 资源，允许底层控制和拓展，带有很少的CPU端的高级封装。

项目处于**极早期开发阶段**。API 会自由变动，设计文档中仍列有未完成的工作；请把
每个 crate 都视为进行中的产物。

## 功能一览

- **移动端优先，且跨平台。** 每帧一个 pass，瞬态深度与多重采样纹理、压缩顶点，没有
  prepass、计算着色器、光照与阴影。同一个帧循环既驱动窗口，也驱动无头离屏目标、
  Web 示例与 Android APK。
- **由调用者组合的场景。** 一帧由若干帧源拼成，各自贡献绘制并声明自己在帧中的位置。
  内置网格渲染、egui 叠加层与调用者自己的绘制趟次权限完全相同。
- **精简的 ECS。** 可渲染实体把网格、材质与管线作为组件携带；借鉴 OPP 重视对象状态，
  行为是持有闭包的组件而非 system，游戏逻辑与绘制共用同一套模型。
- **按变体组合的 unlit 管线。** 着色器变体只含网格实际使用的通道——位置、UV、顶点色、
  逐实例变换与颜色、基础色纹理、蒙皮、形变目标——并针对本帧的渲染目标特化。
- **常驻且池化的 GPU 资源。** 直接使用 `wgpu` 资源；跨帧保留，按需重建，并共享复用
  上传所经过的缓冲。
- **CPU 视锥剔除与自动实例化。** 屏幕外的网格不产生开销，状态相同的绘制折叠为一次
  实例化绘制。
- **egui 与可移植输入。** egui 可叠加于渲染之上，也可渲染到单独纹理；输入统一，由
  类似 OOP 风格的回调驱动。
- **确定性渲染，CI 快照测试。** 同一场景离屏渲染的结果每次都完全相同。三个桌面平台上
  各自 lint、构建与测试，再把示例的每个场景渲染出来并与存储图像做 SSIMULACRA2 比对；
  wasm 与 Android 构建是另外两个独立 job。

## Crate 一览

| Crate | 职责 |
|-------|------|
| [`wgpu_unlit_render`](crates/wgpu_unlit_render/README.zh-CN.md) | 底层渲染器：资源图、顶点压缩、缓冲池、staging、声明式 `Scene`、内置 unlit 管线与 egui 后端。不认识 ECS。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 高层渲染 API：ECS 组件、帧源、输入、UI 与 winit 呈现。 |
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | 高层所用的精简 archetype ECS：没有变化检测、事件、关系或调度器。 |
| [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.zh-CN.md) | 无头 GPU 测试骨架：设备初始化、缓冲与纹理回读、SSIMULACRA2 快照。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 可切换场景的窗口化示例及其无头快照模式；也是打包成 APK 的 Android 示例。 |
| [`xtask`](xtask/README.zh-CN.md) | `cargo xtask` 背后的任务执行器。不是工作区成员。 |

## 各部分的配合方式

`wgpu_unlit_render` 是基础，不依赖工作区中的任何其他 crate。`unlit3d` 构建在它和
`unlit_ecs` 之上，并把两者保留为直接依赖：`prelude` 重导出大多数调用者需要的条目，
其余条目仍可通过各自的 crate 路径访问。

两条原则决定了这种分层：**内置管线不享有特权**，它由调用者自建管线所用的同一批公开
设施组合而成；**帧源彼此平等**，内置网格源与调用者自己的绘制趟次没有任何差别。

设计取舍、架构与实施计划见 [`docs/DESIGN.md`](docs/DESIGN.md)。

## 环境要求

- 较新的 stable Rust 工具链（edition 2024）。
- 一个支持 WebGPU 的设备，用于运行 GPU 测试与示例。在无 GPU 的 CI runner 上，
  Mesa 的 `lavapipe` 充当软件 Vulkan 实现。
- [`cargo-nextest`](https://nexte.st)，用于测试套件。
- [`typos`](https://github.com/crate-ci/typos) 与
  [`tombi`](https://github.com/tombi-toml/tombi)，用于复现 CI 的 lint 步骤。

## 常用命令

常用命令遵循`cargo xtask`约定：

- **`cargo xtask check`** — 对全工作区、全 target、全 feature 跑 clippy
  （`-D warnings`），随后 `cargo fmt --check`。提交前的门槛。加 `--release` 走 release
  profile。
- **`cargo xtask test`** — 用 `cargo nextest run` 覆盖单元与集成测试，随后
  `cargo test --doc` 补上 nextest 不跑的 doctest。加 `--release` 走 release profile。
- **`cargo xtask run-wasm`** — 构建 web 示例并用内置静态服务器提供（WebGPU 需要 secure
  context，`file://` 不行）。`--no-serve` 只构建，`--release` 走 release。
- **`cargo xtask build-android`** — 先 `cargo ndk` 交叉编译示例的动态库放进
  `android/app/src/main/jniLibs`，再调 Gradle 构建 APK。默认 debug，`--release` 构建
  未签名的 release APK。需 JDK 17+ 与 `ANDROID_HOME`（NDK 由 cargo-ndk 自动探测）。
- **`cargo nextest run`** — 需要按名筛选或重跑单个测试时直接用（`-p <crate>`、
  `-E 'test(<name>)'`）。nextest 不跑 doctest，也别用它代替 `cargo xtask test`。
- **`typos`** — 拼写检查，扫全仓库。
- **`tombi lint --error-on-warnings`** 与 **`tombi format`** — TOML 的 lint 与格式检查。
  改过任何 `Cargo.toml` 后务必跑。

## 测试划分

测试按「离被测代码有多近」分三层：

- **单元测试**：在各 crate 的 `src/` 内（`#[cfg(test)]`），只测私有纯逻辑，不碰 GPU，
  随 `cargo nextest run` 直接运行。
- **库集成测试**：在各 crate 的 `tests/` 下，只经公开 API 使用它。`unlit_ecs` 的是纯
  ECS 行为；两个渲染 crate 的是 GPU 测试——用
  [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.zh-CN.md) 建无头设备，
  离屏渲染后回读像素断言，但不与存储图像比较。
- **快照测试**：集成测试的子类，把一帧（或多帧序列）与存储图像做 SSIMULACRA2 感知
  比较。低层 API 的快照留在 `wgpu_unlit_render` 的测试里（`SNAPSHOT_UPDATE=1`
  重新生成）；高层 ECS 场景的快照由
  [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) 的无头模式运行
  （`--scene all` 验证、`--update` 重新生成），因为它既是示例也是 CI 的渲染回归检查。

快照基线都在 [`unlit3d_asset_files`](unlit3d_asset_files/README.md) 这个 git submodule
中，用 `git submodule update --init` 拉取；改动渲染结果后重新生成对应快照，并审查
图像差异再提交。

## 工作区结构

```text
crates/wgpu_unlit_render/   渲染器、它的 WESL 着色器与 GPU 测试
crates/unlit3d/             与 ECS 集成的渲染 API
crates/unlit_ecs/           archetype ECS
crates/wgpu_unlit_test_util/ 共享的 GPU 测试骨架
unlit3d_examples/           窗口化示例、它的场景与快照运行器
android/                    把示例打包成 APK 的 Gradle 工程
xtask/                      cargo xtask 任务执行器（不在工作区内）
docs/DESIGN.md              设计文档
```

## 许可证

双许可：MIT 或 Apache-2.0，任选其一，如工作区 manifest 所声明。
