[English](https://github.com/beicause/unlit3d/blob/main/xtask/README.md) | 简体中文

# xtask

仓库的任务执行器，由 `cargo xtask` 别名驱动。它包装了贡献者在提交前会运行的命令，
使 CI 与本地运行不会彼此脱节，同时负责构建示例的 web 与 Android 产物。

这只是本仓库的开发工具，并设置了 `publish = false`。它是工作区成员，因此
`cargo clippy --workspace` 与 `cargo fmt --all` 会连同它所作用的各个 crate 一起覆盖
它；`.cargo/config.toml` 以 `xtask = "run -p xtask --"` 把它接进来。

## 任务

```text
cargo xtask check          # 对全工作区跑 clippy，随后 cargo fmt --check
cargo xtask test           # 用 nextest 跑单元与集成测试，随后跑 doctest
cargo xtask test-wasm      # 经由 wasm 与 WebGL2 在浏览器里跑 GPU 测试
cargo xtask run-wasm       # 构建 web 示例并提供给浏览器
cargo xtask build-android  # 构建 Android 动态库及其 APK
cargo xtask publish        # 按依赖顺序把工作区的 crate 发布到 crates.io
```

### `cargo xtask check`

依次运行 `cargo clippy --workspace --all-targets --all-features` 与
`cargo fmt --all -- --check`。clippy 排在前面，因为它的诊断可能让代码树处于
`rustfmt` 会重写的状态，因此格式化拥有最后发言权。接受 `--release`。

### `cargo xtask test`

依次运行 `cargo nextest run --workspace --lib --bins --tests --all-features` 与
`cargo test --workspace --all-features --doc`。nextest 让每个测试跑在独立进程中，
因此一个测试的设备、日志后端或 panic 不会影响到别的测试；它不跑 doctest，所以需要
第二趟。目标是一个个点名的，而不是用 `--all-targets` 一把扫进来，因为基准测试并非
测试二进制。

nextest 那一趟会跑两次：一次用默认档位（WebGPU 基线的 limits），一次带
`UNLIT3D_DEVICE_TIER=webgl2`，把所有无头设备收窄到 WebGL2 的 limits 与缺失的
downlevel 能力。第二趟才是在一台适配器为 Vulkan、Metal 或 DX12 的机器上触达浏览器
所走路径（着色器的数组用纹理而非 storage buffer 读取、网格的顶点偏移被烘焙进索引）
的那一趟。把该变量设为 `native` 可改为按适配器自身的 limits 运行，而非基线。接受
`--release`，两趟都生效。

与其快照不匹配的帧会在测试失败前被写下：默认那趟放在 `target/snapshot-mismatches`，
档位那趟放在 `target/snapshot-mismatches-webgl2`——各占一个目录，因此两个档位下都
不同的帧可以被区分开。测试失败时 CI 会上传它们，于是不匹配可以被人查看，而不只是被
打分。

### `cargo xtask test-wasm`

跑的是 `cargo xtask test` 所跑的同一批 GPU 测试，但在真实浏览器里、构建到
`wasm32-unknown-unknown` 上运行，随后还会启动示例本身。wasm 二进制没有进程可以启动、
也没有退出码可以交回，所以这一趟是*搭*出来的而不是直接跑的：

1. 先把整个工作区构建到 wasm——用默认 feature，也就是浏览器与 APK 实际发布的那个——
   这样某个改动若弄坏了该 target 上的示例或库，会在这里就失败，而不是只在自己的 job
   里失败。
2. `cargo nextest list --list-type binaries-only` 构建 wasm 测试二进制并报告它们落在
   哪里——之所以问 nextest，是为了让 target 列表、feature 与名字和宿主机那一趟保持
   一致。
3. `wasm-bindgen` 把每个二进制变成导出 `run_test` 的 JS 模块。
4. 测试页面（加载一个模块并调用该导出）被拷贝到它们旁边，同时写入
   `wasm_paths.json`，把模块名映射到脚本名。
5. 一个 Node runner 提供该目录，并让每个测试在一个全新的浏览器上下文里各开一个页面。
   与其快照不匹配的帧在浏览器中编码——页面没有文件系统——并交给这个 runner，由它写到
   `target/snapshot-mismatches-wasm` 供 CI 上传。
6. *同一批*原生测试二进制在设置了 `UNLIT3D_WASM_TEST` 后变成代理而不是测试套件：
   `cargo nextest` 驱动它们，每个 trial 请求 runner 在浏览器里跑那个测试。于是
   nextest 的列出、筛选、报告与退出码都和普通套件一样可用。

结果通过 `sessionStorage` 传递，因为浏览器没有退出码，而 panic 的测试会让其实例 trap
而不是 unwind——这也是 `#[should_panic]` 由 panic hook 判定、而不是靠捕获 panic 的
原因。

在跑测试之前，runner 还会加载示例——也就是浏览器真正运行的那个程序，GPU 测试从不会
启动它——并检查它能启动、能选到后端、并在画布上留下带明暗的画面而不是一片空白。它用的
正是 `run-wasm` 所提供的同一个页面、由同一份代码构建，因此两者不会走样。

runner 的 JS 依赖与浏览器会在首次运行时安装；用 `CHROME_PATH` 指定现成的浏览器可
免于下载，`--show` 打开可见窗口以观察失败。nextest 那一趟使用的 profile 是 `wasm`，
定义在 `.config/nextest.toml` 中。

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
src/test_wasm.rs      cargo xtask test-wasm：浏览器那一趟及其 runner
src/run_wasm.rs      cargo xtask run-wasm：wasm 构建、bindgen 与页面
src/build_android.rs cargo xtask build-android：交叉构建与 APK
src/publish.rs       cargo xtask publish：要发布的 crate 及其顺序
src/http.rs          run-wasm 用来提供服务的静态文件服务器
src/step.rs          运行单个子命令并报告是哪一步失败
```

## 构建它

```text
cargo run -p xtask -- check
```

作为工作区成员，它共用工作区的 `Cargo.lock` 和 `target/`。在仓库任意位置执行
`cargo xtask <task>` 是通常的入口。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
