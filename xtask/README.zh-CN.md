[English](README.md) | 简体中文

# xtask

仓库的任务执行器，由 `cargo xtask` 别名驱动。它包装了贡献者在提交前会运行的命令，
使 CI 与本地运行不会彼此脱节，同时负责构建并服务 web 示例。

这只是本仓库的开发工具。它不是工作区成员——工作区 manifest 把它列在 `exclude` 下，
`.cargo/config.toml` 以 `xtask = "run --manifest-path xtask/Cargo.toml --"` 把它接进来
——因此任务执行器自身的依赖永远不会压到任务所作用的工作区上。它设置了
`publish = false`。

## 任务

```text
cargo xtask check      # 对全工作区跑 clippy，随后 cargo fmt --check
cargo xtask test       # 用 nextest 跑单元与集成测试，随后跑 doctest
cargo xtask run-wasm   # 构建 web 示例并提供给浏览器
```

### `cargo xtask check`

依次运行 `cargo clippy --workspace --all-targets --all-features` 与
`cargo fmt --all -- --check`。clippy 排在前面，因为它的诊断可能让代码树处于
`rustfmt` 会重写的状态，因此格式化拥有最后发言权。接受 `--release`。

### `cargo xtask test`

依次运行 `cargo nextest run --workspace --all-targets --all-features` 与
`cargo test --workspace --all-features --doc`。nextest 让每个测试跑在独立进程中，
因此一个测试的设备、日志后端或 panic 不会影响到别的测试；它不跑 doctest，所以需要
第二趟。这两条命令合起来就是 CI 所跑的内容。接受 `--release`。

### `cargo xtask run-wasm`

浏览器运行的 `wasm32-unknown-unknown` 二进制所需的标准流水线：为该 target 构建示例，
把 wasm 交给 `wasm-bindgen` 以得到 JS 加载器，然后用内置的静态文件服务器提供产物——
WebGPU 只在*安全上下文*中可用，`localhost` 是安全上下文而 `file://` URL 不是。服务器
内置于任务执行器中，因此无需安装任何东西。

它绑定到回环地址的 8000 端口；若该端口被占用，则尝试其后的端口，并打印要打开的 URL。
`--no-serve` 只构建并跑 bindgen、不提供服务；`--release` 走 release 构建；尾部的
位置参数会透传给 `cargo build`。

## 结构

```text
src/main.rs      参数解析（argh）与任务分发
src/check.rs     cargo xtask check
src/test.rs      cargo xtask test
src/run_wasm.rs  cargo xtask run-wasm：wasm 构建、bindgen 与页面
src/http.rs      run-wasm 用来提供服务的静态文件服务器
src/step.rs      运行单个子命令并报告是哪一步失败
```

本 crate 禁止 `missing_docs`，因此每个条目都带有文档注释。

## 构建它

```text
cargo run --manifest-path xtask/Cargo.toml -- check
```

由于它被排除在工作区之外，它有自己的 `Cargo.lock` 和自己的 `target/` 目录。在仓库根
目录执行 `cargo xtask <task>` 是通常的入口。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
