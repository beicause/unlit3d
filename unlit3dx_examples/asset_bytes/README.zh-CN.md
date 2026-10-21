[English](https://github.com/beicause/unlit3d/blob/main/unlit3dx_examples/asset_bytes/README.md) | 简体中文

# unlit3dx_asset_bytes

[`unlit3dx_examples`](../README.zh-CN.md) 演奏的音色库，被带进二进制，因此示例运行时不需要
任何文件，同一份字节在窗口、浏览器与 Android activity 里都一样。

这些字节单独成为一个 crate，只有一个原因：编译时间。

## 为什么音色库不放在示例里

`include_bytes!` 对读取它的那一方并非免费。用一个 `pub const` 或 `static` 装着音色库，
十四兆字节会被拷进声明它的那个 crate 的元数据里，而每个编译依赖该 crate 的 crate 都要把
这份元数据读回来——示例自身的重新构建也不例外。拆分前在示例上实测：不带音色库时产物是
0.5 MB 元数据与 14 MB rlib，带上之后变成 57 MB 与 100 MB。改在这里声明，元数据则停在
几 KB。

把 `include_bytes!` 写在函数的函数体里，交出去的是 `&'static [u8]` 而不是字节本身，所以
音色库是一个函数，绝不是常量。但光有函数还不够：优化档位（`opt-level` 1 及以上，本工作区
用的就是这个区间）会把函数体序列化出去，好让别的 crate 能内联它，而函数体随身带着它嵌入
的东西——在这里实测，不带 `#[inline(never)]` 的 `soundfont` 仍然写出 14 MB 元数据。加上
该属性后函数体不进元数据，降到 4.8 KB。因此本 crate 的每个函数都带 `#[inline(never)]`，
示例读到的是一份 14 MB、却没有任何 crate 去解析的 rlib。

## 用法

```rust,ignore
let install = install_audio_job_with(unlit3dx_asset_bytes::soundfont().into(), sample_rate);
```

## 音色库缺失时

`assets/FluidR3Mono_GM.sf3` 不进版本控制，因此新克隆下来的仓库里没有它。`build.rs` 在工作区
根目录旁解析它，并把路径导出为 `SOUNDFONT_FILE`；文件不存在时它改写一个空桩，示例静音
启动，而不是编译失败。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一，如工作区 manifest 所声明。
