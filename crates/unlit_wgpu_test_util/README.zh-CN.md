[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu_test_util/README.md) | 简体中文

# unlit_wgpu_test_util

工作区中渲染相关 crate 共用的 GPU 测试骨架。每个测试都通过真实的 `wgpu::Device`
驱动 wgpu，并回读缓冲或纹理数据来做断言；本 crate 负责初始化该设备、回读数据、
把一个渲染出的帧变成感知快照断言，以及让同一个测试既能在宿主机上跑，也能在浏览器
里跑。

本 crate 处于**极早期开发阶段**；API 会自由变动。它不属于对外的渲染 API：实际发布的
渲染路径不依赖它。它是
[`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.zh-CN.md)
与 [`unlit3d`](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)
的 **dev-dependency**，也是
[`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.zh-CN.md)
在 `snapshot` feature 之后的可选依赖。

## 一个测试，两个运行器

一个测试是不带参数、不返回值的 `async fn`；本 crate 的宏把它注册两份：原生平台上它
在进程里运行，而在 `wasm32-unknown-unknown` 上同一个文件*本身*就变成一个 wasm 模块，
由浏览器驱动。

由于 wgpu 的 adapter 与 device 请求是异步的，骨架也是异步的。在宿主机上这一点看不
出来——`block_on` 会把它们驱动到完成。而在浏览器里没有任何东西可以阻塞等待：页面只
有一个线程，请求要在它必须让出的微任务上兑现，所以测试本身被投递到浏览器自己的任务
队列上，其结果再通过 `sessionStorage` 报回。

正是这一差异决定了测试文件末尾要有两次宏调用：

```rust,ignore
gpu_tests! {
    renders_a_cube_over_the_clear_color,
    #[should_panic(expected = "morphing without targets")]
    morphing_without_targets_panics,
}

gpu_test_main!(all_tests());
```

`gpu_tests!` 列出文件中的测试，并把名字旁边的函数体交给它；`gpu_test_main!` 生成运行
它们的 `main`。原生测试 target 在 `Cargo.toml` 中声明为 `harness = false`，因为要跑的
是这个列表，而不是 rustc 生成的那个。

`#[should_panic]` 在两个运行器上都可用，但浏览器上到达它的方式不同：宿主机捕获
unwind，而 wasm 的 panic 会让其实例 trap、根本无法捕获——所以骨架安装一个 panic
hook，把消息与期望做比较，并通过与通过时相同的通道上报。

还有第三种模式。环境变量中有 `UNLIT3D_WASM_TEST` 时，原生运行会变成*代理*而不是运行
器：它列出每个测试以便 `cargo nextest` 驱动它，而每个 trial 请求本地的 Node runner
在真实浏览器里跑那个测试，并把浏览器的判定当作自己的判定上报。`cargo xtask test-wasm`
会设置它。

## 提供的内容

- `Ctx::headless()` —— 开箱可用的无头 `wgpu::Device` 与 `wgpu::Queue`。首次使用时会
  安装日志后端。
- `Ctx::headless_for()` —— 同上，但用于指定的 `DeviceTier`。档位把设备收窄到能力
  较弱的平台所能提供的样子，于是测试可以在并非该平台的硬件上触达该平台所走的路径。
  `Ctx::headless()` 从 `UNLIT3D_DEVICE_TIER` 读取档位，测试套件正是借此再跑一遍
  WebGL2 的形态；不设置时请求的是 WebGPU 基线，而不是适配器自身的 limits。在 web
  上设备由 canvas 与 `GL` 后端创建，因为这是浏览器用来代替原生 API 的东西。
- `init_logging()` —— 单独的日志后端，供从不创建 `Ctx` 的测试使用。原生平台上是读取
  `RUST_LOG` 的 `env_logger`；web 上是转发到浏览器控制台的 `console_log`。默认级别
  为 `warn`。
- 缓冲与纹理回读：`readback_buffer` 与 `read_texture_bytes`（后者会去掉纹理拷贝所
  要求的行填充），以及 `ColorTarget`、`Frame`、`texel_bytes`、`bg_entry`、`rgb`、
  `srgb_to_linear_u8` 与 `count_pixels_off_background`。
- 开启 `snapshot` feature 时：快照断言，见下文。

`log` 被重导出，因此测试 crate 无需自备 `log` 依赖。

## Feature

| Feature | 默认 | 提供的内容 |
|---------|------|-----------|
| `snapshot` | 否 | 基于 SSIMULACRA2 的快照断言，以及用于 WebP 编解码的 `image` |

## 快照

开启 `snapshot` 后，测试可以存储渲染出的帧，或与已存储的帧做断言。
`assert_image_snapshot` 把帧与名为 `name` 的快照在 `DEFAULT_TOLERANCE` 之内比较；
`assert_image_snapshot_with_tolerance` 接受显式的 `Tolerance`。

`Tolerance` 最多指定两道闸门，两者都必须通过：

- **分数下限**，取 SSIMULACRA2 的 0–100 标度，用于发现弥散在全帧、让每个像素都偏移
  一点的变化；
- **离群像素额度**，即允许有多少比例的像素与基线在某个通道上相差超过 `channel_delta`
  （默认 8/255），用于发现集中在少数像素上、被全帧平均淹没的变化。

两者都可以不设。需要放宽的快照应当带上支撑该阈值的实测数据，因为没有实测依据的阈值
与「为了让失败消失而挑的阈值」无法区分。

`score_frame_webp` 与 `store_frame_webp` 是底层的组成部件，供想要报告分数而非断言的
调用者使用。

基线从哪里来取决于目标平台，由 `snapshot!` 宏负责安排：

- **原生平台**按名字在 `tests/snapshots` 下查找（相对于进程的工作目录），并在比较时
  读取，因此可以改写。该目录是指向
  [`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
  submodule 的软链接；用 `git submodule update --init` 拉取。
- **web 上**没有文件系统，所以字节在编译期由 `include_bytes!` 内嵌进 wasm 二进制，
  相对于宏被调用的那个文件解析。除此之外比较完全相同——帧在浏览器里真的被解码、真的
  被打分。

快照缺失时会被*存储*而不是比较；设置 `SNAPSHOT_UPDATE=1` 会重新存储它触及的每个
快照。因此一个从未提交的快照会通过自我写入悄悄让 CI 变绿——这正是 CI 的快照任务要
检出 submodule 的原因。在 web 上快照缺失根本不会发生：那样编译就通不过。

比较*失败*的帧会在断言失败前被留下，因此 CI 中失败的运行会把该帧留给任务去上传：
原生下写在 `UNLIT3D_SNAPSHOT_MISMATCH_DIR`（默认为 `target/snapshot-mismatches`），
web 上则在浏览器中编码后交给测试 runner——它才是拥有文件系统的那个进程。不匹配往往
反映的是平台差异而非回归：存储的帧来自某一套 GPU 栈，另一个驱动的舍入就足以让某帧低于
门槛——所以拿到这一帧，才能让人直接查看差异，而不是从分数去推断。

确有意改动渲染结果后，重新生成快照：

```text
SNAPSHOT_UPDATE=1 cargo nextest run -p unlit_wgpu
git -C unlit3d_asset_files diff   # 提交前先审查
```

## 用法

```rust,no_run
use unlit_wgpu_test_util::{Ctx, read_texture_bytes};

let ctx = unlit_wgpu_test_util::block_on(Ctx::headless());
let target = unlit_wgpu_test_util::ColorTarget::new(&ctx.device, "example", 64, 64);
// ... render into target.view ...
let bytes = read_texture_bytes(&ctx, &target.texture, 64, 64, 4);

#[cfg(feature = "snapshot")]
unlit_wgpu_test_util::assert_image_snapshot(
    unlit_wgpu_test_util::snapshot!("example.webp"),
    &bytes,
    64,
    64,
);
```

## 测试

本 crate 自身的测试覆盖骨架的内部实现，例如离群像素如何计数。工作区的 GPU 测试放在
它们所验证的 crate 旁边，`cargo xtask test` 在宿主机上运行它们，`cargo xtask test-wasm`
则在浏览器里运行同一批测试。两者都见
[根 README](https://github.com/beicause/unlit3d/blob/main/README.zh-CN.md#测试与基准)。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
