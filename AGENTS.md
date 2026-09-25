
## 编码约定

- **项目依然在极早期开发阶段，不用担心兼容性破坏**，注重架构和API设计，不要怕改动已有API，并且没用的API要及时删掉。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：遍历迭代器而不是收集到Vec再遍历、返回迭代其而不是Vec、每帧复用Vec/HashMap而不是重新创建。
- **字节转换统一走 zerocopy**。
- **文档和注释**：保持文档和注释为最新，更新代码的同时，更新有关注释。文档和注释是面向用户的，不要包含不必要的内部细节、不要包含无关的上下文或显而易见的信息。代码、文档、注释默认全用英文。
- **完成任务后cargo检查**：运行`cargo clippy`和`cargo fmt`（或直接 `cargo xtask check`）。

## 常用命令

- **`cargo xtask check`** — clippy（全工作区、全 target、全 feature，`-D warnings`）后接 `cargo fmt --check`。提交前的门槛。加 `--release` 走 release profile。
- **`cargo xtask test`** — 跑测试：`cargo nextest run` 覆盖单元与集成测试，随后 `cargo test --doc` 补上 nextest 不跑的 doctest。CI 跑的就是这一条命令，本地别自己拼 `cargo test`。加 `--release` 走 release profile。
- **`cargo xtask run-wasm`** — 构建 web 示例并用内置静态服务器提供（WebGPU 需要 secure context，`file://` 不行）。`--no-serve` 只构建，`--release` 走 release。
- **`cargo nextest run`** — 需要按名筛选或重跑单个测试时直接用（`-p <crate>`、`-E 'test(<name>)'`）。nextest 不跑 doctest，也别用它代替 `cargo xtask test`。
- **`typos`** — 拼写检查，扫全仓库。
- **`tombi lint --error-on-warnings`** 与 **`tombi format`** — TOML 的 lint 与格式检查。改过任何 `Cargo.toml` 后务必跑。

由于`cargo`命令可能运行较慢，要注意：
- **若改动较小，cargo测试时要按包或名称筛选**，对于较小的改动不要总是跑全量测试。
- **不要过度跑cargo build**：
  1. 尽可能少用cargo build，优先用check或clippy。若cargo check或clippy通过了，则大概率cargo build也能通过。
  2. 如果改动不是平台特定的，如没有用到`#[cfg(...)]`，就无需跑平台特定（如wasm,android）的检查或构建。
  4. 有CI兜底。若怀疑存在未捕获的错误可以查看最新一次的github action是否通过。

测试日志走 `log` crate，不靠 `println!`/`eprintln!`：

- 日志后端由 `wgpu_unlit_test_util::Ctx::headless`（native 用 `env_logger`，web 用 `console_log`）在首次使用时装好；不建 `Ctx` 的测试（如 ECS 的）需要日志时自行调用 `wgpu_unlit_test_util::init_logging()`。
- 默认级别 `warn`；要看细节用 `RUST_LOG=debug cargo xtask test`。nextest 默认按测试捕获输出，失败时才回显，故 `RUST_LOG` 对失败诊断足够。
- 因为每个测试跑在独立进程里，一个测试装的后端不影响别的测试。

## 关于本项目

- 总体设计文档参见`docs/DESIGN.md`，其中记录了基本的功能设计、设计抉择、设计哲学、基本架构、原始的粗糙API设计等。这些设计应该是稳定的、不随实施细节变化而变化的。如果你认为有变更应该纳入`DESIGN.md`，请注意提醒用户更新该文档，但未经许可，绝不擅自修改`DESIGN.md`。
- **不要看docs/internal/**。这里面是WIP的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`wgpu_unlit_render_asset_files`子模块中**。
