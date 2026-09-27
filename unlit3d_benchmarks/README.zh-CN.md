[English](README.md) | 简体中文

# unlit3d_benchmarks

帧路径的基准测试：一帧有多快，以及时间花在了哪。两个目标都用
[`unlit3d`](../crates/unlit3d/README.zh-CN.md) 驱动同一批场景。

本 crate 不属于测试运行：基准二进制没有测试 harness，因此 `cargo xtask test` 把它
排除在外，改由 `cargo bench` 驱动。工作区的测试覆盖什么见
[根 README](../README.zh-CN.md#测试与基准)。

## `frame` —— 有多快

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

## `profile` —— 时间花在哪

同样的 world，跑在 `profiling` 跨度下，每帧每个阶段打印一行。当基准说某次改动变慢了、
而问题变成「慢在哪个阶段」时，就用这个目标。

```text
cargo bench -p unlit3d_benchmarks --features profile-tracing \
    --bench profile -- [实体数] [帧数]
```

两个参数都可选且按位置传入：给出实体数则只跑该用例，帧数覆盖默认的 2。不加
`--features profile-tracing` 时跨度会编译消失——没有后端时 `profiling::scope!` 是
空操作——该次运行不会报告任何耗时。

<details>
<summary>打印出的阶段树长什么样</summary>

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

</details>

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
