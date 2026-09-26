[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.md) | 简体中文

# unlit_wgpu_test_util

工作区中渲染相关 crate 共用的 GPU 测试骨架。每个测试都通过真实的 `wgpu::Device`
驱动 wgpu，并回读缓冲或纹理数据来做断言；本 crate 负责初始化该设备、回读数据，以及
把一个渲染出的帧变成感知快照断言。

它刻意保持自包含：除 `wgpu`、`glam`、`log` 与 `pollster`，加上平台的日志后端，以及
`snapshot` feature 之后的 `fast-ssim2` 与 `image` 之外，它不依赖任何东西。

本 crate 处于**极早期开发阶段**；API 会自由变动。它不属于对外的渲染 API：实际发布的
渲染路径不依赖它。它是
[`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md)
与 [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
的 **dev-dependency**，也是
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)
在 `snapshot` feature 之后的可选依赖。

## 提供的内容

- `Ctx::headless()` —— 开箱可用的无头 `wgpu::Device` 与 `wgpu::Queue`，adapter 与
  device 请求由 `pollster` 同步驱动。首次使用时会安装日志后端。
- `init_logging()` —— 单独的日志后端，供从不创建 `Ctx` 的测试使用。原生平台上是读取
  `RUST_LOG` 的 `env_logger`；web 上是转发到浏览器控制台的 `console_log`。默认级别
  为 `warn`。
- 缓冲与纹理回读：`readback_buffer` 与 `read_texture_bytes`（后者会去掉纹理拷贝所
  要求的行填充），以及 `ColorTarget`、`Frame`、`texel_bytes`、`bg_entry`、`rgb`、
  `srgb_to_linear_u8` 与 `count_pixels_off_background`。
- 开启 `snapshot` feature 时：SSIMULACRA2 快照断言。

`log` 被重导出，因此测试 crate 无需自备 `log` 依赖。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `snapshot` | 否 | 基于 SSIMULACRA2 的感知快照断言，以及用于 WebP 编解码的 `image` |

## 快照

开启 `snapshot` 后，测试可以存储渲染出的帧，或与已存储的帧做断言：

- `assert_image_snapshot(name, rgba, width, height)` 把帧与名为 `name` 的快照比较，
  当分数低于 `DEFAULT_MIN_SCORE`（85.0）时失败。`assert_image_snapshot_with_threshold`
  接受显式阈值。
- `score_frame_webp` 与 `store_frame_webp` 是底层的组成部件，供想要报告分数而非断言的
  调用者使用。
- 快照以 `name` 在 `tests/snapshots` 下查找（相对于进程的工作目录），并以无损 WebP
  存储。该目录是指向
  [`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
  submodule 的软链接；用 `git submodule update --init` 拉取。

快照缺失时会被*存储*而不是比较；设置 `SNAPSHOT_UPDATE=1` 会重新存储它触及的每个
快照。因此一个从未提交的快照会通过自我写入悄悄让 CI 变绿——这正是 CI 的快照任务要
检出 submodule、而示例的无头路径拒绝与不存在的快照比较的原因。

确有意改动渲染结果后，重新生成快照：

```text
SNAPSHOT_UPDATE=1 cargo nextest run -p unlit_wgpu
git -C unlit3d_asset_files diff   # 提交前先审查
```

## 用法

```rust,no_run
use unlit_wgpu_test_util::{Ctx, read_texture_bytes};

let ctx = Ctx::headless();
let target = unlit_wgpu_test_util::ColorTarget::new(&ctx.device, "example", 64, 64);
// ... render into target.view ...
let bytes = read_texture_bytes(&ctx, &target.texture, 64, 64, 4);

#[cfg(feature = "snapshot")]
unlit_wgpu_test_util::assert_image_snapshot("example.webp", &bytes, 64, 64);
```

## 测试

本 crate 自身没有测试；它是其他 crate 的 GPU 测试所使用的骨架。用 `cargo xtask test`
运行整个工作区的测试套件，并注意它需要一个可用的 WebGPU adapter。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
