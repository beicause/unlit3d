# 绕过做法

本仓库不得不绕开的驱动与后端 bug：为什么显而易见的写法是错的，以及绕法要付什么代价。
每条都点名绕过代码所在的位置，后来的人不必先从症状搜起。

绕过它的那段代码旁留有一行注释指回这里。

[English](WORKAROUND.md) | 简体中文

## DX12 上对向量做动态下标，读出的 lane 全是 0

**位置：**[`crates/unlit_wgpu/shaders/array_access.wesl`](crates/unlit_wgpu/shaders/array_access.wesl)
的 `load_cell_word`，也就是设备没有 storage buffer 时（WebGL2）走的那条 texel 路径。

**症状：**在 DX12 上跑 WebGL2 档时，lit 场景里的点光源与面光源全部消失，只剩聚光灯。
片元读到的簇格是对的，格里的灯数也是对的，但格里每一个灯的索引都读成 0，于是它们全部指向索引 0 那盏灯——在示例场景里恰好是一盏聚光灯。全程没有任何警告：画面只是画错了。

**成因：**格记录把四个 `u32` 字存进一个 `Rgba32Float` texel，取其中一个字就是从 texel 里挑一个 lane：

```wgsl
let texel = textureLoad(light_cells_tex, texel_of(light_cells_tex, first), 0);
return bitcast<u32>(texel[word % 4u]); // DX12 上每个 lane 都读成 0
```

WGSL 允许这样写，naga 也如实翻成 HLSL（`v[min(uint(lane), 3u)]`）。FXC 照收——这是合法的右值下标，不会报 `X3500`——然后生成无论下标是多少都读 lane 0 的 DXBC。naga 早就知道 FXC 会编错值指针背后的向量下标（[naga#2095](https://github.com/gfx-rs/naga/issues/2095)，[naga#2096](https://github.com/gfx-rs/naga/pull/2096) 只修了**常量**下标），但由变量给出的下标仍然原样透传（[wgpu#4460](https://github.com/gfx-rs/wgpu/issues/4460) 只覆盖左值那种写法）。

对**数组**做下标没问题：naga 会把值落进临时数组，FXC 能原生寻址（[wgpu#4337](https://github.com/gfx-rs/wgpu/issues/4337)）。向量 lane 没有这种寻址方式，所以那个数组修法延伸不到这里。

**绕法：**把 lane 逐个命名，再选出要的那个，让动态下标根本不进编译器：

```wgsl
let lane = word % 4u;
let lanes = vec4<u32>(bitcast<u32>(texel.x), bitcast<u32>(texel.y),
                      bitcast<u32>(texel.z), bitcast<u32>(texel.w));
return lanes.x * select(0u, 1u, lane == 0u)
     + lanes.y * select(0u, 1u, lane == 1u)
     + lanes.z * select(0u, 1u, lane == 2u)
     + lanes.w * select(0u, 1u, lane == 3u);
```

**代价：**每读一个字从一次带下标的读取变成四次乘法、三次加法、四次 select，落在每个受光片元的灯循环里。另一个选择是先把格解包进一个 `var` 数组，代价相当，而 WebGPU 路径的 storage buffer 本来就免掉了这一步。

**规矩：**着色器里每一处 texel 解包都按名字挑 lane。着色器树里已经没有任何动态向量下标，保持下去。

## 以带裁剪的绘制收尾，会让 pass 的多重采样解析在 GL 上被裁掉

**位置：**[`crates/unlit3d/src/renderer.rs`](crates/unlit3d/src/renderer.rs) 的 `record_pass` 末尾，所有源都录完之后。

**症状：**在 GL 后端开着 MSAA 时，以带裁剪的绘制收尾的场景（比如 UI 窗口）只解析了那一小块矩形，画面其余部分留着上一帧的像素。

**成因：**裁剪矩形是 pass 状态，而视口不同，wgpu 没有把它重置的调用。GL 后端上结束一个 pass 的多重采样解析是一次 blit，而 blit 受 pass 结束时生效的裁剪矩形限制。于是最后一次绘制的裁剪范围决定了整帧解析多少。

**绕法：**所有源录完之后用整个目标盖掉它：

```rust
render_pass.set_scissor_rect(0, 0, attachments.width(), attachments.height());
```

这同时让 [`DrawEntry::scissor`](crates/unlit_wgpu/src/scene.rs) 名副其实——`None` 表示「保留 pass 现有的裁剪矩形」，而不是暗示整个目标——因为 pass 不再依赖某个绘制设过它。

**代价：**每个 pass 多一次冗余的状态设置。

**规矩：**只要一个 pass 里画过带裁剪的东西，结束前就用整个目标盖掉裁剪矩形。回归测试是 [`crates/unlit3d/tests/gpu_ui_source.rs`](crates/unlit3d/tests/gpu_ui_source.rs) 里的 `a_clipped_panel_does_not_clip_the_resolve`。

## 导数写在逐片元分支里，在 WebGPU 上编不过

**位置：**[`crates/unlit_wgpu/shaders/lit_lighting.wesl`](crates/unlit_wgpu/shaders/lit_lighting.wesl)
的 `fragment_normal`，它在后面那个分支之前先取好两个导数。

**症状：**浏览器 WebGPU 后端上所有 lit 测试全挂，且只在这个后端上挂。页面日志是

```text
Error while parsing WGSL: :162:43 error: 'dpdy' must only be called from uniform control flow
```

随后 `ShaderModule`、`RenderPipeline`、`CommandBuffer` 接连无效，这一帧什么都没画出来，断言在一片 0 的画布上失败。

**成因：**WGSL 要求导数内建函数——`dpdx`、`dpdy`、`fwidth`——只能从 uniform 控制流里调用。原来的着色器先定下法线，然后才取几何法线：

```wgsl
if length_squared > 1.0e-8 { return in.normal * inverseSqrt(length_squared); }
var geometric = cross(dpdx(in.world), dpdy(in.world)); // 非 uniform 控制流
```

`length_squared` 是逐片元插值出来的，于是这个分支是非 uniform 的，它之后的代码也一样。只有浏览器 WebGPU 背后的校验器 Tint 会做 uniform 性分析，naga 不做，所以宿主上一个后端都没拒过这个模块，这个错误直到浏览器编译它才现身。这个缺口记在 [wgpu#4369](https://github.com/gfx-rs/wgpu/issues/4369) 里：naga 对片元阶段的 uniform 性要求被 `naga/src/valid/analyzer.rs` 里的一个 `DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE` 常量编译掉了，`DERIVATIVE` 与 `IMPLICIT_LEVEL` 两位因此归零，导数在宿主上从不做 uniform 性检查。那个分支在 `@if(VERTEX_NORMAL)` 里，因此只有带顶点法线的着色器受影响。

**绕法：**先取导数，再分支：

```wgsl
let world_dx = dpdx(in.world);
let world_dy = dpdy(in.world);
@if(VERTEX_NORMAL) { … }
var geometric = cross(world_dx, world_dy);
```

这也正是规范要求的形状，所以没有剩下任何需要检测的东西：模块在每个后端上都合法。

**代价：**每个受光片元多取两次导数，即使答案由顶点法线定了、它们被丢弃。导数是对已在寄存器里的值做一次跨四像素的减法，比它挪到前面的那个分支还便宜。

**规矩：**导数内建函数要在函数里任何分支之前调用，否则就别调用。着色器树里只有这两处调用，保持下去。没有任何测试看住这一条：各套件把每个变体都组合出来、用 naga 校验，而 naga 恰恰是不问这个问题的那个校验器。只有浏览器的 WebGPU 后端会用 Tint 编译着色器，而只有 lit 的那些测试才走得够远，能把「编译失败」和「设备没了」区分开。

## headed 浏览器的 GPU watchdog 会在负载下杀掉 GPU 进程

**位置：**[`tests/wasm/runner/index.js`](tests/wasm/runner/index.js) 的 `BACKEND_ARGS.webgpu` 里那个 `--disable-gpu-watchdog`。

**症状：**WebGPU 那趟每次跑都有几个示例快照测试失败，而且每次都不一样——118 个里四到七个，每个都在十九到二十八秒之后报 `the staging buffer could not be mapped: A valid external Instance reference no longer exists`。这些用例单独跑都过。失败开始的那一刻，Chrome 自己的 stderr 是：

```text
ERROR:command_buffer_proxy_impl.cc:476] GPU state invalid after WaitForGetOffsetInRange.
ERROR:command_buffer_proxy_impl.cc:277] ContextResult::kTransientFailure: Failed to send GpuControl.CreateCommandBuffer.
ERROR:gpu_process_host.cc:1042] GPU process exited unexpectedly: exit_code=512
ERROR:… Message N rejected by interface blink.mojom.WidgetHost
```

**成因：**这里的 `exit_code` 是 `base::Process::GetTerminationStatus` 交回的原始 wait status——`base/process/kill_posix.cc` 把 `waitpid` 的状态原样存下、不做右移——所以 `512 >> 8` 得 2，即 `RESULT_CODE_HUNG`。GPU 进程不是崩了，是被 Chrome 的 GPU watchdog 因为长时间没有进展而杀掉的：

```text
gpu/ipc/common/gpu_watchdog_timeout.h: kGpuWatchdogTimeout = Seconds(15), kSoftwareRenderingFactor = 2
gpu/ipc/service/gpu_init.cc: bool enable_watchdog = !gpu_preferences_.disable_gpu_watchdog
                                                    && !command_line->HasSwitch(switches::kHeadless);
```

SwiftShader 的 Vulkan 设备慢到这个程度：同时画十六个页面——浏览器套件的默认并行度——足以让 GPU 主线程在两次出帧之间停过软件渲染拿到的三十秒。进程一死，浏览器里所有设备跟着死，于是正在画的那些测试都在下一次映射时报错，作决定的并不是浏览器自己。

headless 豁免正是只有这一趟中招的原因：WebGL2 那趟是 headless，从来没有 watchdog。

**绕法：**对需要窗口的那一趟关掉 watchdog：

```js
const BACKEND_ARGS = {
  webgpu: [ …, "--disable-gpu-watchdog" ],
  webgl2: [],
};
```

**代价：**GPU 进程真的卡死时，浏览器不再在它自己的超时里杀掉并重启它，改由 harness 的每测试超时（五分钟，`TIMEOUT_MS`）来判这一页失败。除此之外没有别的东西看住 GPU 进程，靠超时发现卡死的代价是整页失败，而不是那一个测试失败。

**规矩：**headed 那趟都要传 `--disable-gpu-watchdog`。headless 的不需要，headless 浏览器根本不会起 watchdog。

## 没有窗口、系统又没有 GL 驱动时，WebGPU 画布读出来全是透明

**位置：**[`tests/wasm/runner/index.js`](tests/wasm/runner/index.js) 的 `BACKENDS_NEEDING_A_WINDOW`，以及 runner 为这个集合里的后端起的那台私有 Xvfb；还有 [`.github/workflows/ci-main.yml`](.github/workflows/ci-main.yml)，它在一张裸镜像上装好画布所需的 OpenGL 实现。

**症状：**WebGPU 那趟的示例检查——唯一一处读画布的检查——一直等到超时，而页面报告这一帧已经画出来了。画布仍是透明的：`drawImage`、`toDataURL`、`page.screenshot` 一律返回 `rgba(0, 0, 0, 0)`。页面还报 `A valid external Instance reference no longer exists`，设备丢失。

**成因：**帧确实画了，只是没送到合成器：WebGPU 画布要靠一块 shared image 呈现，而 Chrome 找不到能为它造出 backing 的工厂。涉及的几个名词分属三层：

| 名字 | 所处层 | 是什么 |
| --- | --- | --- |
| WebGL2、WebGPU | 网页调用的接口 | 浏览器的两代 GPU 接口 |
| ANGLE、Dawn | 把接口调用落到实处的实现 | ANGLE 把 WebGL2 翻给 Vulkan、桌面 GL 或某个软件后端；Dawn 实现 WebGPU |
| SwiftShader | Chrome 自带的软件设备 | 一台软件 Vulkan 设备，外加一个 ANGLE 可以建上下文的软件 GL 实现 |
| Mesa | Linux 系统可能带的软件设备 | 软件 Vulkan 叫 `lavapipe`，软件 OpenGL 叫 `llvmpipe` |
| `libvulkan1`、ICD 文件 | Vulkan 实现的查找方式 | loader，以及登记「可以加载哪些实现」的 JSON 文件 |

让画布没有 backing 的是两个彼此独立的条件，任一条件单独成立都会得到同一片透明。

*没有窗口。*无头 Chrome 根本不合成 WebGPU 画布，加什么参数都一样：

```text
ERROR:shared_image_factory.cc: Could not find SharedImageBackingFactory with params:
  usage: …|WebgpuSwapChainTexture|…, format: RGBA_8888, gmb_type: empty
ERROR:shared_image_stub.cc: SharedImageStub: Unable to create shared image
```

接下来把画布纹理当拷贝源就会报错，Chrome 于是把设备判为失效，页面余下的时间都不可用。没有任何参数能救：在 Chrome 154 与 157 上，无论加不加 `--enable-unsafe-webgpu`、SwiftShader 那三个 Vulkan 参数、`--use-webgpu-adapter`、`--ozone-platform=x11`、ANGLE、Skia Graphite 与那组合成开关，试过的组合都是透明的。

*系统没有自己的 GL 驱动。*headed 浏览器若其 OpenGL 上下文落在 SwiftShader 上，回读出来依旧是同一片透明，而 CI runner 正是这个状态：镜像里既没有 OpenGL 也没有 Vulkan 实现，浏览器只能用它自带的 SwiftShader。于是 Chromium 拒绝画布交换链所依赖的那条 interop，判断条件就卡在「OpenGL 上下文不是 SwiftShader 的」上：

```text
if (use_swift_shader || gpu_preferences.gr_context_type != GrContextType::kGL)
  return kGpuFeatureStatusDisabled;              // GetWebGPUOnVulkanViaGLInterop()
```

interop 一关，`ExternalVkImageBackingFactory` 就不会注册——Linux 上只有它接受 `WEBGPU_SWAP_CHAIN_TEXTURE` 用途，而它只在 `gr_context_type_ == GrContextType::kVulkan || enable_webgpu_on_vk_via_gl_interop` 时才被加进工厂表（chromium 的 `gpu/command_buffer/service/shared_image/shared_image_factory.cc`）。画布于是没有 backing，理由与 headless 时相同，报的也是那两条同样的消息。

**绕法：**把浏览器按下面几样凑齐。参数是 [agent-browser 的 WebGPU 说明](https://agent-browser.dev/webgpu)为 Linux 列出的那套，外加一个本仓库自己的 watchdog 开关：

| 需要 | 由什么提供 | 为什么 |
| --- | --- | --- |
| 没有显卡也能跑 WebGPU | `--enable-unsafe-webgpu`，加上 `--use-angle=vulkan --use-vulkan=swiftshader --use-webgpu-adapter=swiftshader --disable-vulkan-surface` | 这四个把 ANGLE 与 WebGPU 都钉在 Chrome 自带的 SwiftShader Vulkan 上，于是不需要显卡，也不会悄悄换成别的 adapter |
| 画布会被合成 | headed，加上 `--enable-features=Vulkan` | headed 且开着这个 feature 才会合成；headed 但不开、以及 headless 无论开不开，读出来都是透明，而 adapter 与测试看着都正常 |
| 一块显示 | 私有 Xvfb，`DISPLAY` 指过去，删掉 `WAYLAND_DISPLAY` | 机器没有显示服务时由 Xvfb 提供；刻意自起而不借现成的，因为现成的那台可能是某个 Wayland 会话的 Xwayland——唯一例外是 `--show` 且已设 `DISPLAY`，那时窗口要落在开发者自己屏幕上 |
| X 而不是 Wayland | `--ozone-platform=x11` | Wayland 通往合成器的那条路要 DRM 设备，软件 Vulkan 设备不是 |
| 不被 watchdog 杀掉 | `--disable-gpu-watchdog` | headed 浏览器在软件渲染卡住 GPU 线程时会杀掉 GPU 进程，见上一条 |
| 一个不是 SwiftShader 的 OpenGL 上下文 | `sudo apt-get install --yes xvfb libvulkan1 mesa-vulkan-drivers` | `mesa-vulkan-drivers` 给 ANGLE 一个可以建的 OpenGL 上下文，`libvulkan1` 是它找到该 ICD 所经的 loader；底下给 WebGPU 用的仍是 Chrome 自带的 SwiftShader Vulkan，所以这些包只决定画布会不会被合成，不改变测试跑在什么设备上 |
| 容器的环境 | `--no-sandbox`、`--disable-dev-shm-usage`、`--enable-unsafe-swiftshader` | 容器的沙箱与过小的 `/dev/shm`，以及两个后端的软件渲染 |

```js
const server = spawn("Xvfb",
  ["-displayfd", "1", "-screen", "0", "1280x1024x24", "-nolisten", "tcp"],
  { stdio: ["ignore", "pipe", "pipe"] });
```

`-displayfd` 让 Xvfb 自己挑一个空闲的 display 号并报到 stdout，于是同一台机器上两趟运行不会撞号；它自己的 stderr 在转发时加上 `xvfb:` 前缀，免得服务器端的抱怨被当成浏览器的。

**代价：**WebGPU 那趟变成 headed，读画布要经合成器，不再是一次纹理拷贝；WebGL2 那趟保持 headless 启动，因为它的画布确实能到得了 headless 的合成器。本来就在跑桌面的机器自带 OpenGL 实现，所以只有裸的 CI 镜像才需要装。

**规矩：**只有读画布的检查才需要这一套，而示例检查是唯一一处。GPU 测试从离屏纹理读它那一帧——若有测试改成读画布，它在任何没有显示器的机器上都会挂。
