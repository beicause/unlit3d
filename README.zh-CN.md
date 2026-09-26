[English](README.md) | 简体中文

# unlit3d

面向 WebGPU 的**无光照（unlit）**绘制的紧凑、有主见的 3D 渲染器，以及构建在其上的
ECS 层。本工作区以 WebGPU（及其背后的原生后端）为目标，并且移动端优先；不支持
WebGL 与 GLES。它在一个 render pass 内把整个场景——不透明与透明实例一视同仁——
绘制到一个 `wgpu::TextureView`，带有 CPU 视锥剔除，以及为 CI 快照测试准备的一流
无头渲染路径。

项目处于**极早期开发阶段**。API 会自由变动，设计文档中仍列有未完成的工作；请把
每个 crate 都视为进行中的产物。

## Crate 一览

| Crate | 职责 |
|-------|------|
| [`wgpu_unlit_render`](crates/wgpu_unlit_render/README.zh-CN.md) | 底层渲染器：资源图、网格顶点压缩、缓冲池、staging、声明式 `Scene`、内置 unlit 管线，以及 egui 后端。它不认识 ECS。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 高层渲染 API：ECS 组件、帧源、带有管线家族（family）的 mesh 源、输入、UI 叠加层，以及 winit 呈现。 |
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | 高层所针对的 archetype ECS。刻意精简：没有变化检测、钩子、事件、实体关系或调度器。 |
| [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.zh-CN.md) | 无头 GPU 测试骨架：设备初始化、缓冲与纹理回读，以及可选的 SSIMULACRA2 图像快照。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 可切换场景的窗口化示例——以及它自己的无头快照模式。同时也是 Android 示例，会打包成 APK。 |
| [`xtask`](xtask/README.zh-CN.md) | `cargo xtask` 背后的仓库任务执行器。不是工作区成员。 |

## 各部分的配合方式

`wgpu_unlit_render` 是基础，不依赖工作区中的任何其他 crate。`unlit3d` 构建在它和
`unlit_ecs` 之上，并把两者保留为直接依赖而非整体重导出：它的 `prelude` 重导出
大多数调用者需要的条目，其余条目仍可通过各自的 crate 路径访问。
`wgpu_unlit_test_util` 是两个渲染 crate 的 dev-dependency；`unlit3d_examples`
仅在开启 `snapshot` feature 时使用它。

两条原则决定了这种分层：

- **内置管线不享有特权。** unlit 管线用到的一切——绑定槽位、顶点压缩、资源追踪、
  变体缓存——都是公开的，并且它由调用者自建管线时所用的同一批设施组合而成。
- **帧源彼此平等。** 一帧由若干 `unlit3d::source::FrameSource` 实现拼成，每个实现
  各自产出 `wgpu_unlit_render::scene::Scene`；内置网格渲染是其中一个源，调用者
  自己的绘制趟次是另一个，两者权限完全相同。

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

测试按「离被测代码有多近」分三层，各层的职责与规则如下：

- **单元测试**：写在各 crate 的 `src/` 内（`#[cfg(test)]`），只测 crate 私有的纯逻辑，
  不碰 GPU、不建 `LocalWorld`。它们随 `cargo nextest run` 直接运行。
- **库集成测试**：放在各 crate 的 `tests/` 下，只通过该 crate 的公开 API 使用它。
  `unlit_ecs` 的集成测试是纯 ECS 行为；两个渲染 crate 的集成测试是 GPU 测试——用
  [`wgpu_unlit_test_util`](crates/wgpu_unlit_test_util/README.zh-CN.md) 建无头设备，
  把场景离屏渲染后回读像素并断言，但不与任何存储图像比较。
- **快照测试**：是集成测试的一个子类，把一帧（或多帧序列）与仓库中存储的图像做
  SSIMULACRA2 感知比较，用来冻结渲染结果。判定规则是：**低层 API 的快照留在
  `wgpu_unlit_render` 的测试里**（`tests/snapshots` 软链接接入 submodule，用
  `SNAPSHOT_UPDATE=1` 重新生成）；**高层 ECS 场景的快照由
  [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) 的无头模式运行**
  （`--scene all` 验证、`--update` 重新生成），因为它既是示例也是 CI 的渲染回归检查，
  不再为这些场景维护两处代码。

因此，快照基线都在
[`unlit3d_asset_files`](unlit3d_asset_files/README.md) 这个 git
submodule 中；用 `git submodule update --init` 拉取。改动了渲染结果时，按上面各自的方式
重新生成对应快照，并审查图像差异后再提交。

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
