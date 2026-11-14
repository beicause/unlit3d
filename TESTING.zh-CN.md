# 测试

[English](TESTING.md) | 简体中文

本工作区如何检查自己：测试分哪几层、怎么运行、各 crate 覆盖什么，以及运行它们的
CI workflow。基准测试也放在这里，因为它与测试走的是同一条帧路径。

## 测试分层

测试按「离被测代码有多近」分三层：

- **单元测试**：在各 crate 的 `src/` 内（`#[cfg(test)]`），只测私有纯逻辑，不碰 GPU，
  随 `cargo nextest run` 直接运行。
- **库集成测试**：在各 crate 的 `tests/` 下，只经公开 API 使用它。`unlit_ecs` 的是纯
  ECS 行为；两个渲染 crate 的是 GPU 测试——用
  [`unlit_wgpu_test_util`](crates/unlit_wgpu_test_util/README.zh-CN.md) 建无头设备，
  离屏渲染后回读像素断言，但不与存储图像比较。
- **快照测试**：把一帧（或多帧序列）与存储图像做 SSIMULACRA2 感知比较。低层 API 的
  快照留在 `unlit_wgpu` 的测试里；高层 ECS 场景的快照由
  [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) 的 `tests/gpu_suite.rs` 运行
  （`SNAPSHOT_UPDATE=1` 重新生成），因为它既是示例也是 CI 的渲染回归检查；而
  [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) 的 `tests/gpu_cli.rs` 通过命令行工具的库
  渲染同一个参考 glTF 场景，与同一批快照比较。

单元层里还有一些性质是用生成而非手写的输入来检查的：`proptest` 驱动 offset 分配器
（偏移始终对齐且互不重叠，装得进最大空闲区的请求必定获批）、依赖图（它接受的每条边
都落在依赖顺序上，且一轮清扫后存活节点仍有序）与网格压缩（位置与方向在一个量化步长
内往返，索引恰在装得下时收窄）。失败的输入会写进该 crate 的 `proptest-regressions/`，
使同一用例重跑；这些文件要提交，因为种子本身就是用例。

每个 crate 的集成测试都是**一个二进制**，而不是每个源文件一个：`tests/` 下的文件是它们
旁边那个 suite 文件（`gpu_suite.rs`，纯 `libtest` 的测试则为 `suite.rs`）的模块，用
`#[path]` 引入。它们共用该 crate 的代码，GPU 那侧还共用同一套骨架，因此把它们拆成多个
target 就意味着同一份代码按文件各链接一次——此前整个工作区有三十个测试二进制，每一个
都带着同一套运行时。二进制的名字就是 crate 的名字，其 `required-features` 是它的用例
所需的功能，因此若某个 suite 用到了可选功能，该功能打开时它才会运行。

快照基线都在 [`unlit3d_asset_files`](https://github.com/beicause/unlit3d_asset_files/blob/main/README.md)
这个 git submodule 中。它被标记为 `update = none`：把本库当作 git 依赖的项目不会去拉取
用不到的快照；而这里测试确实要用，所以在本仓库用
`git submodule update --init --checkout` 拉取。改动渲染结果后重新生成对应快照，并审查
图像差异再提交。由于基线来自同一套 GPU 栈，换一个平台的驱动就可能让某个场景低于阈值
而并没有出错；因此不匹配的帧会在测试失败前被写下（每一趟各写进 `target/` 下的一个
目录），CI 再把这些目录作为 artifact 上传。默认档失败后，WebGL2 档仍会跑完。

## 运行

```text
cargo xtask test                 # 整个工作区：先 nextest，再 doctest
cargo nextest run -p unlit_wgpu --all-features  # 单个 crate
cargo nextest run -E 'test(name)'  # 单个测试
cargo xtask test-wasm            # 同一批 GPU 测试，再在浏览器里跑一遍
```

suite 二进制要求它的用例所需的功能，因此不带这些功能单跑一个 crate 会把它整个编掉：
只跑 `cargo nextest run -p unlit3d` 跑的是该 crate 的单元测试，跳过 GPU suite，因为后者
需要 `gltf`、`mcp` 与 `ui`。

`nextest` 不跑 doctest，因此 `cargo xtask test` 会在其后追加 `cargo test --doc`。
用 `cargo nextest run` 筛选只适合迭代时用，它不能替代该任务；CI 跑的就是该任务，
所以两者不会脱节。

`cargo xtask test-wasm` 把渲染 crate 的 GPU 测试再跑一遍：构建到
`wasm32-unknown-unknown`，在真实浏览器中运行，浏览器的两个 GPU 后端各跑一趟。测试
文件是同一批：suite 那个 target 本身就是 wasm 模块，suite 根部唯一的一次
`gpu_test_main!` 给它生成页面要调用的导出。浏览器提供的是无头设备给不了的东西——
一个带着自己的 limits、自己的着色器翻译和自己的光栅化器的实现。由于该光栅化器并不是
采集快照时所用的那个，`Tolerance` 可以指定一帧允许相差多少；立方体快照正是这么用的，
并附有其阈值的实测依据。

它还会在浏览器里启动每个示例，并检查画布上留下的是一幅有明暗的画面、而不是一片空白。
GPU 测试从不会启动示例，因此没有这一步的话，某个「能编过、但页面仍是空的」改动会照样
通过。

渲染 crate 的集成测试与示例都需要可用的 GPU。在无头 CI runner 上，用 Mesa 的
`lavapipe` 作为软件 Vulkan 实现。wasm 那几趟不需要 GPU：浏览器用自带的 SwiftShader，
两个后端都是。凡是需要读画布的地方，WebGPU 那一趟要的还不止这些——得有一个窗口，以及一个
不是 SwiftShader 的 OpenGL 实现，裸机器上还得把它装上。哪项检查需要哪一样，见
[`WORKAROUND.zh-CN.md`](WORKAROUND.zh-CN.md) 的「没有窗口、系统又没有 GL 驱动时，
WebGPU 画布读出来全是透明」。

## 各 crate 的覆盖

| Crate | 测试覆盖的内容 |
|-------|----------------|
| [`unlit_ecs`](crates/unlit_ecs/README.zh-CN.md) | world 与查询行为、延迟命令，以及 MCP 服务器的 ECS 工具。 |
| [`unlit_wgpu`](crates/unlit_wgpu/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把网格渲染到离屏纹理，与 `tests/snapshots` 下的快照比较（该目录是指向 asset submodule 的软链接）。 |
| [`unlit3d`](crates/unlit3d/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把场景渲染到离屏目标并检查回读的像素，其中有一个端到端 MCP 测试，用一对流把 world 服务出去并用 JSON-RPC 与它对话。其多帧快照覆盖位于 `unlit3d_examples`。 |
| [`unlit3d_examples`](unlit3d_examples/README.zh-CN.md) | 命令行（`src/cli.rs`）、场景切换与固定步长播放时钟（`src/tests.rs`）、窗口浮层（`src/overlay/tests.rs`），以及 `tests/gpu_suite.rs` 的渲染快照；`tests/selector_position.rs` 覆盖窗口外壳跨场景切换保留的面板。 |
| [`unlit3d_cli`](unlit3d_cli/README.zh-CN.md) | 参数解析与配置合并，以及端到端渲染测试：复现示例的参考 glTF 场景并与存储快照比较。 |
| [`unlit3dx`](crates/unlit3dx/README.zh-CN.md) | 多 world 核心：job 抵达目标 world、快照与流订阅、world 对自己状态的发布、render 桥的失败路径，以及物理、音频与 MCP 各桥的端到端测试。 |
| [`unlit3dx_examples`](unlit3dx_examples/README.zh-CN.md) | 示例场景逻辑的单元测试，以及一次端到端的离屏运行：捕获的帧与混音同资产子模块下的快照基准比较；同一批 world 也会在浏览器中步进运行，并处在一条 Android activity 之后。 |

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
轻量档的任务，再补上只有发行才需要的部分。分档跟着提交落到哪里走：`main` 以外任何分支
上的推送、以及任何 pull request，都跑轻量档；同一分支的推送与由它开出的 pull request 共用
一个并发槽位，pull request 不会再起一次。推送到 `main`、合并队列的运行与手动运行跑整份。

轻量档跑的是改动自己就能弄坏的东西，并且要落在审阅者读这一遍代码的时间里：只跑一个平台，
web 与 Android 只跑到 lint 而不是它外围的浏览器或 APK。剩下的由 `main` 跑，那是证明一份
发行物能出得去的那部分。

`main` 以外的每个分支，无论是自己推上去的还是开成 pull request 的：

- **lint** —— 对全部文件跑 `typos`；对 TOML 跑 `tombi lint --error-on-warnings` 与
  `tombi format --check`；以及 `cargo fmt --all -- --check`。
- **build**（Linux）—— 带默认 features、完全不带 features 与带全部 features 的
  clippy，然后 `cargo xtask test` 与带 `-D warnings` 的 `cargo doc`。快照比较在
  `cargo xtask test` 里跑两遍：一遍用 WebGPU 基线的 limits，一遍把所有离屏设备收窄到
  WebGL2 的形态。由于 runner 没有 GPU，Linux 会安装 Mesa 以提供 `lavapipe`，并取来示例
  音色库，好让音频基线比较的是真实声音。
- **miri** —— `cargo xtask miri`：把 `unlit_ecs` 的测试放到 Miri 下解释执行，这是
  唯一能看见「经 `&World` 发出引用的那套存储」里未定义行为的检查。其他 crate 都不跑。
- **check-wasm** —— 为 `wasm32-unknown-unknown` 对工作区、四个库（关闭默认
  features）与测试二进制各跑一遍 clippy。它排除 `xtask`：那是只在宿主机上运行的
  任务运行器，其 HTTP 服务器无法为该目标编译。
- **check-android** —— 为 `aarch64-linux-android` 跑带全部 features 与完全不带
  features 的两遍 clippy，这是唯一会编译 `cfg(target_os = "android")` 后面代码的
  检查；`build` 里的宿主 clippy 到不了那里。

`main` 上——每次推送到它、合并队列的一次运行，以及从它手动触发的运行，都是整份：

- **build** 除 Linux 外还在 macOS 与 Windows 上跑。
- **build-wasm** —— 先跑 `check-wasm` 的那三遍 clippy，然后把每个 wasm 测试二进制
  bindgen 成页面，在真实浏览器中驱动：WebGPU 一趟、WebGL2 一趟，两趟都是软件设备。
  既然这里已经 lint 了这些构建，`main` 上就不必再跑 `check-wasm`。
- **build-android** —— 先跑 `check-android` 的两遍 clippy，再交叉构建动态库，并在其外套上
  Gradle 得到 APK。它自己就跑了这两遍，所以 `main` 上同样不必再跑 `check-android`。

两个 `check-*` 任务各自与 `main` 上做同一件事的任务共用一个缓存
（Swatinem/rust-cache 的 `shared-key`），因此轻量档编译出来的东西，合并后是
被还原而不是被重编。两个 workflow 为此设置同一份 `env`：rust-cache 会把每个
`CARGO*` 与 `RUST*` 变量都算进 key，一边设了、另一边没设，就会把它本要共享的
缓存拆成两份。每个任务即便失败也会保存它的缓存（`cache-on-failure`），因此在
失败的分支上迭代时，下一次运行仍比冷启动要暖和。

失败运行留下的帧按各趟分别上传，各来自自己的目录：`build` 的
`target/snapshot-mismatches` 与 `target/snapshot-mismatches-webgl2`，`build-wasm` 的
`target/snapshot-mismatches-wasm` 与 `target/snapshot-mismatches-wasm-webgl2`。不匹配
往往反映的是平台差异而非回归，所以拿到这一帧，才能让人直接查看差异，而不是从分数去
推断。

[`.github/workflows/pages.yml`](.github/workflows/pages.yml) 在每次推送到 `main` 时
把站点发布到 GitHub Pages：根目录是一个首页，链接两个 web 示例与 API 文档，每个示例
各占旁边的一个目录，工作区的 rustdoc 放在 `docs/` 下，`unlit3d` 示例的页角也链接到这些
文档。发布这一侧用 nightly 构建文档，因为列出各 crate 的索引页与 `doc(cfg(...))` 标签都
只存在于 nightly，并且容忍那里的文档警告：一个链接坏掉的页面，胜过被它挡下、什么都没
发出去的站点。让文档警告弄挂构建的是 `ci.yml`，那是在分支上，而不是等到发布时。
