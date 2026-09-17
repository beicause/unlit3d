# bevy_unlit

bevy_unlit是精简的、定制化的、无光照（unlit）3D渲染器，只使用最小化的bevy依赖，不依赖`bevy_camera`、`bevy_render`、`bevy_pbr`、`bevy_ui`、`bevy_sprite_render`等bevy渲染相关包，而是直接使用wgpu进行渲染。

目标硬件为WebGPU，并且移动端优先。不支持WebGL、GLES。

## 功能

功能有主见的、偏底层的、移动优先的。灵活性不是本库目标。

- 交换链纹理使用8位srgb纹理，或srgb纹理视图。直接渲染或直接解析MSAA到交换链中，不使用中间纹理，不进行后处理。
- 单个render pass完成所有不透明和透明实例的渲染。
- 默认启用MSAA，深度纹理用`Depth32FloatStencil8`或`Depth24PlusStencil8`，并且MSAA纹理和深度纹理是`TRANSIENT_ATTACHMENT`。
- 有意使用很少的pipeline变体。只用内部定制的unlit材质，不支持自定义材质。
- 仅3D，不对2D进行特殊优化。
- 支持morph targets和skinning。
- 顶点属性是压缩的，位置用Snorm16x4，UV用Snorm16x2，两者配合aabb和uv范围解码。顶点色用Unorm8x4，骨骼索引用Uint16x4，骨骼权重Unorm16x4。
- 顶点缓冲用4个：一个用于位置，一个用于UV+顶点色，一个用于骨骼索引+权重，一个用于逐实例步进的属性（每实例变换矩阵、染色）。
- UI集成`egui`，用一个render pass绘制到交换链。

不支持：
- 光照、阴影
- 后处理
