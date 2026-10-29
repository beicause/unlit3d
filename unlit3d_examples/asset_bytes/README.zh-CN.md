[English](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/asset_bytes/README.md) | 简体中文

# unlit3d_asset_bytes

[`unlit3d_examples`](../README.zh-CN.md) 的 `gltf` 场景所绘制的 glTF 文档——一只带蒙皮的
狐狸和一份形变目标压力测试——被带进二进制，因此场景运行时不需要任何文件。两者都来自
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md)
子模块。

这些字节单独成为一个 crate，只有一个原因：编译时间。

## 为什么这些资产不放在示例里

`include_bytes!` 对读取它的那一方并非免费。用一个 `pub const` 或 `static` 装着文档，字节
会被拷进声明它的那个 crate 的元数据里，而每个编译依赖该 crate 的 crate 都要把这份元数据
读回来——示例自身的重新构建也不例外。若声明在 `unlit3d_examples` 里，一兆字节的文档就是
一兆字节的元数据，下游每个 crate 每次增量构建都得重新解析一遍。

把 `include_bytes!` 写在函数的函数体里，交出去的是 `&'static [u8]` 而不是字节本身，所以
这里的每个资产都是一个函数，绝不是常量。但光有函数还不够：优化档位（`opt-level` 1 及
以上，本工作区用的就是这个区间）会把函数体序列化出去，好让别的 crate 能内联它，而函数体
随身带着它嵌入的东西——在这里实测，不带 `#[inline(never)]` 的 `fox` 仍然写出 0.6 MB
元数据，每个资产一份。加上该属性后它们不进元数据，降到 5 KB。因此本 crate 的每个函数都带
`#[inline(never)]`，这个 crate 也因此可以任意长大，而不会拖慢读取它的任何东西。

## 用法

```rust,ignore
let fox = unlit3d_asset_bytes::fox();
let document = LitGltf::from_bytes(fox).expect("the fox asset is a valid glTF document");
```

| 函数 | 资产 |
|------|------|
| `fox` | `Fox.glb`，`gltf` 场景里走 Walk 动画的蒙皮狐狸 |
| `morph_stress_test` | `MorphStressTest.glb`，八个形变目标一起脉动的立方体 |

## 许可证

双许可：MIT 或 Apache-2.0，任选其一，如工作区 manifest 所声明。
