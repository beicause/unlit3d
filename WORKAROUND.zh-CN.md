# 绕过做法

本仓库不得不绕开的驱动与后端 bug：为什么显而易见的写法是错的，以及绕法要付什么代价。
每条都点名绕过代码所在的位置，后来的人不必先从症状搜起。

绕过它的那段代码旁留有一行注释指回这里。

[English version](WORKAROUND.md)

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
