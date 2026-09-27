
## 编码约定

- **项目依然在极早期开发阶段，不用担心兼容性破坏**，注重架构和API设计，不要怕改动已有API，并且没用的API要及时删掉。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：遍历迭代器而不是收集到Vec再遍历、返回迭代其而不是Vec、每帧复用Vec/HashMap而不是重新创建。
- **字节转换统一走 zerocopy**。
- **完成任务后cargo检查**：运行`cargo clippy`和`cargo fmt`（或直接 `cargo xtask check`）。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见根目录 [`README.md`](./README.md) 的「测试与基准」一节

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

## 关于本项目

- **设计文档分散在各`README`中**：根[`README.md`](./README.md)记动机、功能与编码原则，各 crate 的`README.md`记该 crate 的设计抉择与架构。
  它们要随代码一并更新，保持最新。同时更新中英文版的`README`，不要只更新一版。
- **代码内的文档注释、git提交信息默认全用英文**。文档注释随代码一并更新，保持最新；注释面向用户，不含无关上下文、特定于用户与Agent的会话内容、显而易见的细节等。
- **不要看docs/internal/**。这里面是WIP的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`unlit3d_asset_files`子模块中**。
