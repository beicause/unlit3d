
## 编码约定

- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **字节转换统一走 zerocopy**。
- **文档和注释**：保持文档和注释为最新，更新代码的同时，更新有关注释。文档和注释是面向用户的，不要包含不必要的内部细节、不要包含无关的上下文或显而易见的信息。代码、文档、注释默认全用英文。
- **完成任务后cargo检查**：运行`cargo clippy`和`cargo fmt`。

## 关于本项目

总体设计文档参见`docs/DESIGN.md`。如有设计上的变更，请注意提醒用户和更新该文档，但未经许可，绝不覆写`DESIGN.md`的已有内容。

## 行为偏好

- **积极使用ask、question等提问工具**。你不是无人看守地运行，要积极报告问题和提问，并期待用户来提供反馈。
