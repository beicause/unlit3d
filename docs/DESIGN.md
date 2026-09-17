# bevy_unlit

bevy_unlit是精简的、定制化的、无光照（unlit）3D渲染器，只使用最小化的bevy依赖，不依赖`bevy_camera`、`bevy_render`、`bevy_pbr`、`bevy_ui`、`bevy_sprite_render`等bevy渲染相关包，而是直接使用wgpu进行渲染。

目标硬件为WebGPU，并且移动端优先。不支持WebGL、GLES。

## 功能

功能有主见的、偏底层的、移动优先的。灵活性不是本库目标。

- 交换链纹理使用8位srgb纹理，或srgb纹理视图。直接渲染或直接解析MSAA到交换链中，不使用中间纹理，不进行后处理。
- 单个render pass完成所有不透明和透明实例的渲染。
- 默认启用MSAA，深度纹理用`Depth32FloatStencil8`或`Depth24PlusStencil8`，并且MSAA纹理和深度纹理是`TRANSIENT_ATTACHMENT`。
- unlit管线默认支持基础色、基础色纹理。支持morph targets和skinning。
- 支持自定义的着色器、绑定和管线。
- 仅3D，不对2D进行特殊优化。
- 顶点属性是压缩的，位置用Snorm16x4，UV用Snorm16x2，两者配合aabb中心和范围、uv最小值和范围解码。顶点色用Unorm8x4，骨骼索引用Uint16x4，骨骼权重Unorm16x4。
- 顶点缓冲用4个：一个用于位置，一个用于UV+顶点色，一个用于骨骼索引+权重，一个用于逐实例步进的属性（每实例变换矩阵、染色）。
- 不自动实例化，支持手动实例化。
- UI集成`egui`，用一个render pass绘制到交换链。

不支持：
- 光照、阴影。
- 后处理。
- 与bevy的渲染包（`bevy_render`及其依赖）的兼容性。

## API设计

原则：
- 直接使用和暴露wgpu的底层GPU资源，不用不必要的包装。

![WebGPU资源依赖图](./assets/webgpu-draw-diagram.svg)

每帧渲染过程伪代码：
```rust
for pipeline in pipelines {
    pass.set_pipeline(pipeline); // builtin, or custom render pipeline.
    pass.set_bind_group(0, globals); // camera, globals (time, delta, frame count), mesh metadata (vertex decoding metadata, morph targets info), and other global resources.
    
    for material_binding in material_bindings {
        pass.set_bind_group(1, material_binding); // base color texture, or custom.

        for mesh in meshes {
            pass.set_bind_group(2, mesh_binding); // mesh metadata index, joint matrices, morph deltas, morph weights.
            pass.set_vertex_buffer(mesh_vertex_attributes);
            pass.set_vertex_buffer(mesh_instance_data); // model transform, tint color.
            // If `draw_indexed`:
            // pass.set_index_buffer(mesh_index_buffer);
            pass.draw(vertices, instances);
        }
    }
}

```