# 测试与基准

[English](TESTING.md) | 简体中文

本仓库如何被检查，以及每类检查位于何处。命令本身列在根 README 的
[常用命令](../README.zh-CN.md#常用命令)一节。

## 测试划分

测试按「离被测代码有多近」分三层：

- **单元测试**：在各 crate 的 `src/` 内（`#[cfg(test)]`），只测私有纯逻辑，不碰 GPU，
  随 `cargo nextest run` 直接运行。
- **库集成测试**：在各 crate 的 `tests/` 下，只经公开 API 使用它。`unlit_ecs` 的是纯
  ECS 行为；两个渲染 crate 的是 GPU 测试——用
  [`unlit_wgpu_test_util`](../crates/unlit_wgpu_test_util/README.zh-CN.md) 建无头设备，
  离屏渲染后回读像素断言，但不与存储图像比较。
- **快照测试**：集成测试的子类，把一帧（或多帧序列）与存储图像做 SSIMULACRA2 感知
  比较。低层 API 的快照留在 `unlit_wgpu` 的测试里（`SNAPSHOT_UPDATE=1`
  重新生成）；高层 ECS 场景的快照由
  [`unlit3d_examples`](../unlit3d_examples/README.zh-CN.md) 的无头模式运行
  （`--scene all` 验证、`--update` 重新生成），因为它既是示例也是 CI 的渲染回归检查。

快照基线都在 [`unlit3d_asset_files`](../unlit3d_asset_files/README.md) 这个 git submodule
中，用 `git submodule update --init` 拉取；改动渲染结果后重新生成对应快照，并审查
图像差异再提交。

### 运行

```text
cargo xtask test                 # 整个工作区：先 nextest，再 doctest
cargo nextest run -p unlit_wgpu  # 单个 crate
cargo nextest run -E 'test(name)'  # 单个测试
```

`nextest` 不跑 doctest，因此 `cargo xtask test` 会在其后追加 `cargo test --doc`。
用 `cargo nextest run` 筛选只适合迭代时用，它不能替代该任务；CI 跑的就是该任务，
所以两者不会脱节。

各 crate 的测试覆盖：

| Crate | 测试覆盖的内容 |
|-------|----------------|
| [`unlit_ecs`](../crates/unlit_ecs/README.zh-CN.md) | world 与查询行为、延迟命令，以及 `SendWorld` 能跨线程共享。不需要 GPU，因此在任何环境都能运行。 |
| [`unlit_wgpu`](../crates/unlit_wgpu/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把网格渲染到离屏纹理，与 `tests/snapshots` 下的快照比较（该目录是指向 asset submodule 的软链接）。 |
| [`unlit3d`](../crates/unlit3d/README.zh-CN.md) | 单元测试，以及 GPU 集成测试：把场景渲染到离屏目标并检查回读的像素。其多帧快照覆盖位于 `unlit3d_examples`。 |
| [`unlit_wgpu_test_util`](../crates/unlit_wgpu_test_util/README.zh-CN.md) | 自身没有测试：它是其他 crate 的 GPU 测试所使用的骨架。 |
| [`unlit3d_examples`](../unlit3d_examples/README.zh-CN.md) | 命令行（`src/cli.rs`）与固定步长播放时钟（`src/lib.rs`）。其渲染输出由 CI 的快照任务检查，而不是由 `cargo test` 目标检查。 |

渲染 crate 的集成测试与示例都需要可用的 GPU。在无头 CI runner 上，用 Mesa 的
`lavapipe` 作为软件 Vulkan 实现。

## 基准测试

[`unlit3d_benchmarks`](../unlit3d_benchmarks/Cargo.toml) 下有两个目标。它不属于测试运行：
基准二进制没有测试 harness，因此被排除在 `cargo xtask test` 之外，改由 `cargo bench`
驱动。

### `frame` —— 有多快

用 [Criterion](https://docs.rs/criterion) 跑帧路径，以每秒实体数报告。它有两个组：

- **`frame`** —— 构建场景的一帧，覆盖 100 到 100 000 个实体，各有 `visible` 与
  `culled` 两个变体，因此绘制路径与剔除路径的变化会各自体现出来。
- **`spawn world`** —— 构建 world 本身。它属于初始化而非每帧工作，因此单独测量。

```text
cargo bench -p unlit3d_benchmarks
cargo bench -p unlit3d_benchmarks --bench frame -- 'frame/visible/100000'
```

筛选时要指明目标（`--bench frame`）：过滤参数只会传给被指定的目标，而 `profile` 目标
自己接收位置参数，因此 `cargo bench -p unlit3d_benchmarks -- <过滤串>` 会把两个目标
都跑起来。

Criterion 把报告写在 `target/criterion` 下，并把每次运行与上一次比较。请把几个百分点的
差异视为噪声：这是共享机器上的帧基准，数值会随机器负载浮动。请在负载相近时对比，并
优先看重复测量的结果而不是单次采样。

### `profile` —— 时间花在哪

同样的 world，跑在 `profiling` 跨度下，每帧每个阶段打印一行。当基准说某次改动变慢了、
而问题变成「慢在哪个阶段」时，就用这个目标。

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- [实体数] [帧数]
```

两个参数都可选且按位置传入：给出实体数则只跑该用例，帧数覆盖默认的 2。不加
`--features profile-tracing` 时跨度会编译消失——没有后端时 `profiling::scope!` 是
空操作——该次运行不会报告任何耗时。

每个阶段命名为 `模块.阶段`，由外到内，因此打印出的跨度路径读起来就是它们描述的树。
`renderer.frame` 是根；实体很多的一帧大致分解如下：

```text
renderer.frame
├── renderer.frame.build_sources
│   └── mesh_source.build
│       ├── mesh_source.metadata.upload
│       ├── mesh_source.uniforms.upload
│       ├── mesh_source.global_groups.rebuild
│       ├── scene.cull
│       ├── scene.resolve
│       │   └── scene.resolve.family
│       ├── scene.sort
│       ├── mesh_source.poses.pack
│       ├── mesh_source.poses.upload
│       ├── mesh_source.instances.upload
│       └── mesh_source.assemble
│           ├── mesh_source.assemble.handles
│           └── mesh_source.assemble.draws
├── renderer.frame.resolve_order
├── renderer.frame.record_passes
└── renderer.frame.submit
```

请把它们当作分解说明，而不是承诺：这些跨度覆盖的是帧自身的 CPU 工作，花在 GPU 驱动
内部或等待设备的时间不会计入其中。

只取出某次运行中 `visible` 用例的各阶段：

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- 100000 3 \
  | awk '/^== visible/{f=1} /^== culled/{f=0} f' \
  | grep time.busy
```

## 持续集成

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) 在每次推送到 `main` 和每个
pull request 上运行上述检查以及各 lint 关卡：

- **lint** —— 对全部文件跑 `typos`；对 TOML 跑 `tombi lint --error-on-warnings` 与
  `tombi format --check`；以及 `cargo fmt --all -- --check`。
- **build**（Linux、macOS、Windows）—— 带与不带 `unlit` feature 的 clippy、
  `cargo build --workspace --all-targets`、`cargo xtask test`、带 `-D warnings` 的
  `cargo doc`，以及示例的无头快照比较。由于 runner 没有 GPU，Linux 会安装 Mesa 以
  提供 `lavapipe`。
- **build-wasm** —— clippy 与 `wasm32-unknown-unknown` 构建。
- **build-android** —— `aarch64-linux-android` 交叉构建与 Gradle APK。

快照比较正是让渲染回归会失败而不是悄然通过的那道检查，这也是示例的场景承担它的原因。
