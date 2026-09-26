[English](README.md) | 简体中文

# xtask

仓库的任务执行器，由 `cargo xtask` 别名驱动。它包装了贡献者在提交前会运行的命令，
使 CI 与本地运行不会彼此脱节，同时负责构建示例的 web 与 Android 产物。

这只是本仓库的开发工具。它不是工作区成员——工作区 manifest 把它列在 `exclude` 下，
`.cargo/config.toml` 以 `xtask = "run --manifest-path xtask/Cargo.toml --"` 把它接进来
——因此任务执行器自身的依赖永远不会压到任务所作用的工作区上。它设置了
`publish = false`。

## 任务

```text
cargo xtask check          # 对全工作区跑 clippy，随后 cargo fmt --check
cargo xtask test           # 用 nextest 跑单元与集成测试，随后跑 doctest
cargo xtask run-wasm       # 构建 web 示例并提供给浏览器
cargo xtask build-android  # 构建 Android 动态库及其 APK
cargo xtask publish        # 按依赖顺序把工作区的 crate 发布到 crates.io
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

这里显式指定了二进制 target，而不是交给默认的 target 选择：示例 crate 同时是
`cdylib` 和二进制，在 `wasm32-unknown-unknown` 上两者都想写出
`unlit3d_examples.wasm`，cargo 会就此报告输出文件名冲突。浏览器运行的是二进制。

### `cargo xtask build-android`

两个工具，沿语言边界分工。`cargo ndk` 为 Android 交叉编译示例并把 `.so` 放进 app 的
`jniLibs` 目录——Gradle 正是在那里寻找原生库并打包它找到的一切；随后 Gradle 编译
activity 并把 APK 组装起来。先构建动态库，因为缺少它的 APK 就是一个无法启动的 activity。

两个工具都从环境变量自我配置：`cargo ndk` 从 `ANDROID_NDK_HOME` 读取 NDK（否则取
`ANDROID_HOME/ndk` 下最新的一版），Gradle 从 `ANDROID_HOME` 读取 SDK（否则看
`android/local.properties`）。`PATH` 上需要有 JDK 17 或更新版本，这是 AGP 的要求。

只编译一个 ABI（`arm64-v8a`）和一个 API 级别（26），与
`android/app/build.gradle.kts` 中的 `abiFilters` 和 `minSdk` 一致——这两个文件就是
构建中 Rust 一半与 Gradle 一半之间的全部契约。默认是 debug 构建类型；`--release`
构建 release APK，由于本工程不提供签名配置，它保持未签名。

### `cargo xtask publish`

把会发布到 crates.io 的 crate——`unlit_ecs`、`unlit_wgpu`、`unlit3d`——逐个、按上述
顺序发布。这个顺序是硬性要求而非偏好：一个 crate 在它所依赖的 crate 进入 registry 之前
无法打包，所以先发布 `unlit3d` 会报 `no matching package named unlit_ecs found`。
`cargo publish` 会等待每个已上传的 crate 出现在索引里，这正是下一个能解析成功的原因。

`cargo publish --workspace` 本来会自行排序，但它的 `--dry-run` 无法验证包
（rust-lang/cargo#16525）；逐个发布则保留了发布前值得做的验证步骤。`--dry-run`
只做全部检查、不上传。

测试骨架、示例与任务执行器都设置了 `publish = false`，因此永远不会被上传。真正发布是
不可重复的——cargo 会拒绝已在 registry 上的版本——所以中途失败的运行必须从失败的那个
crate 继续。

## 结构

```text
src/main.rs          参数解析（argh）与任务分发
src/check.rs         cargo xtask check
src/test.rs          cargo xtask test
src/run_wasm.rs      cargo xtask run-wasm：wasm 构建、bindgen 与页面
src/build_android.rs cargo xtask build-android：交叉构建与 APK
src/publish.rs       cargo xtask publish：要发布的 crate 及其顺序
src/http.rs          run-wasm 用来提供服务的静态文件服务器
src/step.rs          运行单个子命令并报告是哪一步失败
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
