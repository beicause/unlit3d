
## 编码约定

- **项目依然在极早期开发阶段，不用担心兼容性破坏**，注重架构和API设计，不要怕改动已有API，并且没用的API要及时删掉。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：遍历迭代器而不是收集到Vec再遍历、返回迭代其而不是Vec、每帧复用Vec/HashMap而不是重新创建。
- **字节转换统一走 zerocopy**。
- **完成任务后cargo检查**：运行`cargo clippy`和`cargo fmt`（或直接 `cargo xtask check`）。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见[根目录`README.md`中的`## 测试划分`](./README.md)

## Git 工作流

- **主分支（`main`）保持线性历史，不要有 merge commit**。合入改动用 `git cherry-pick`、`git rebase` 或 squash，不要用 `git merge`。

## 常用命令

常用命令遵循`cargo xtask`约定，各命令及其解释见根目录 [`README.md`](./README.md) 的「常用命令」一节；更新`xtask`时，注意同步更新该列表。

由于`cargo`命令可能运行较慢，要注意：
- **使用`cargo nextest`，而不是`cargo test`**。
- **针对特定改动、特定bug时，使用`cargo nextest`时要筛选**。不要总是跑全量`cargo nextest`或`cargo xtask test`测试。
- **合并整合多次改动后，再跑检查或测试**，不要改一点测一点，减少检查或测试次数。
- **不要过度跑cargo build**：
  1. 尽可能少用cargo build，优先用check或clippy。若cargo check或clippy通过了，则大概率cargo build也能通过。反正有CI兜底。
  2. 如果改动不是平台特定的，如没有用到`#[cfg(...)]`，就无需跑平台特定（如wasm,android）的检查或构建。

测试日志走 `log` crate，不靠 `println!`/`eprintln!`：

- 日志后端由 `wgpu_unlit_test_util::Ctx::headless`（native 用 `env_logger`，web 用 `console_log`）在首次使用时装好；不建 `Ctx` 的测试（如 ECS 的）需要日志时自行调用 `wgpu_unlit_test_util::init_logging()`。
- 默认级别 `warn`；要看细节用 `RUST_LOG=debug cargo xtask test`。nextest 默认按测试捕获输出，失败时才回显，故 `RUST_LOG` 对失败诊断足够。
- 因为每个测试跑在独立进程里，一个测试装的后端不影响别的测试。

## 关于本项目

- **总体设计文档参见`docs/DESIGN.md`**，其中记录了基本的功能设计、设计抉择、设计哲学、基本架构、原始的粗糙API设计等。这些设计应该是稳定的、不随实施细节变化而变化的。如果你认为有变更应该纳入`DESIGN.md`，请注意提醒用户更新该文档，但未经许可，绝不擅自修改`DESIGN.md`。
- **保持文档、注释、对应库目录下的`README.md`等为最新**，更新代码的同时，一并更新有关文档。文档和注释是面向用户的，不要包含不必要的内部细节、不要包含无关的上下文或显而易见的信息。代码、文档、注释默认全用英文。
- **不要看docs/internal/**。这里面是WIP的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`unlit3d_asset_files`子模块中**。
