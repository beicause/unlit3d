[English](https://github.com/beicause/unlit3d/blob/main/unlit3d_cli/README.md) | 简体中文

# unlit3d_cli

一个把 glTF 文档渲染成图片的命令行工具。包名为 `unlit3d_cli`，构建出的可执行程序名为
`unlit3d-cli`。与它所用的那些 crate 一样，它也处于 **早期阶段**。

## 它做什么

它加载一个或多个 glTF 文档，为每个文档设置摆放，在给定时间采样一段动画，通过相机把场景
画出来，再把这一帧写到文件。以上每一部分都可配置：JSON 配置文件描述整次渲染，命令行选项
覆盖其中同名的字段。

命令行用 [`argh`](https://docs.rs/argh) 解析，因此每个选项都是 `--name value` 或裸开关，
值必须作为下一个参数传入，不支持 `--name=value`。`--output` 是唯一必需的选项，它的扩展名
决定图片格式：支持 `png`、`webp` 与 `jpeg`。

```text
cargo run -p unlit3d_cli --bin unlit3d-cli -- --output frame.png --document unlit3d_asset_files/assets/Fox.glb --animation Walk --time 0.2
```

## 输出大小与渲染大小

一次渲染有两个尺寸。**输出大小**是最终写出的图片。**渲染大小**是相机与视口所依据的尺寸：
它决定相机的宽高比，以及场景被画进的那块区域的形状。视口等于渲染大小乘以 `scale`，并在
输出中居中。`scale` 是一对值 `[scale_x, scale_y]`，两个轴各自独立缩放：因此 512x384 的
输出配上 256x192 的渲染大小与 `scale [2.0, 2.0]`，会把场景放大一倍并填满画面；而更大的
输出若保持同样的渲染大小与 `scale [1.0, 1.0]`，就会加黑边。

只有一张纹理，尺寸为输出大小；场景直接画进去，因此画面从不重采样。`render_size` 默认取
输出大小，输出大小默认取渲染大小，所以两者至少要给出一个。

## 相机

相机既可以由 `eye`、`target`、`up`、`fov_y`（角度）与 `z_near` 构建，也可以直接给出
一对列主序 4x4 矩阵：视图（world-to-view）矩阵与投影（view-to-clip）矩阵，在配置文件中为
`view` 与 `projection`，在命令行上为 `--camera-view-matrix` 与
`--camera-projection-matrix`。两者都给时以矩阵为准：相机把视图与投影分开保存，单个合并矩阵
无法拆回它们；命名字段构建的是右手系 look-at 视图与 DirectX 风格无限远反向投影，与
`unlit3d_examples` 绘制其场景所用的相机一致。

## 配置文件

`--config` 会在应用覆盖之前读取一个 JSON 文件。所有字段都可选，未知字段会被拒绝。结构如下：

```json
{
  "output": {
    "size": [512, 384],
    "render_size": [256, 192],
    "scale": [2.0, 2.0],
    "samples": 1,
    "depth": true,
    "clear": [0.0, 0.0, 0.0, 1.0]
  },
  "camera": {
    "eye": [160.5, 109.0, 49.5],
    "target": [4.5, 38.0, 26.5],
    "up": [0.0, 1.0, 0.0],
    "fov_y": 60.0,
    "z_near": 0.1
  },
  "documents": [
    {
      "path": "unlit3d_asset_files/assets/Fox.glb",
      "placement": {
        "translation": [-4.54, 0.0, -1.01],
        "rotation": [0.0, -0.21643962, 0.0, 0.976296],
        "scale": [1.0, 1.0, 1.0]
      },
      "animation": "Walk",
      "time": 0.0
    }
  ]
}
```

`animation` 指定文档中的某段动画，可用索引或名字；`time` 是动画时间（秒）。文档以自身场景
绘制，它生成的每个节点在给出 `placement` 时都会采用该变换。

在命令行上，`--document` 可重复，并替换配置中的文档列表，而 `--animation` 与 `--time` 作用
于每个文档。

## 库

二进制只负责解析参数与写文件。渲染本身是库的 `render` 函数：它构建无头 GPU 设备，把场景画进
离屏渲染目标并读回像素；`save` 则把一帧编码到指定路径。端到端测试驱动的正是这两者。

## 测试

`tests/gpu_cli.rs` 是端到端检查。它通过库在六个动画相位分别渲染
`unlit3d_examples/src/scenes/gltf.rs` 的参考 glTF 场景，把每一帧与
`unlit3d_asset_files/snapshots/gltf/` 中存储的快照比较。它还会用一份自己的快照检查窄幅渲染
大小——192x288 的输出配上 256x192 的渲染大小、两个轴都按 `288/192` 缩放——并检查 `save`
能写出 png、webp 与 jpeg。

```text
cargo nextest run -p unlit3d_cli
```

快照比较使用 SSIMULACRA2。快照不存在时，它会用该帧写出一份而不是失败，`SNAPSHOT_UPDATE=1`
则重写已存在的那份。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
