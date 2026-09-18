# wgpu_unlit_render

wgpu_unlit_render是基于wgpu的紧凑的、有主见的、无光照（unlit）3D渲染器。

目标硬件为WebGPU，并且移动端优先。不支持WebGL、GLES。

## 功能

功能是有主见的、移动优先的：
- API最终目标是使用一个上下文/状态绘制到一个`TextureView`渲染目标。深度纹理的格式和有无可以配置。
- 支持`egui`，可将`egui`的`FullOutput`转为本库的上下文并且更新上下文，以绘制到渲染目标，但不集成平台输入输出。
- 单个render pass完成所有不透明和透明实例的渲染。
- 默认启用MSAA。MSAA纹理和深度纹理默认是`TRANSIENT_ATTACHMENT`。
- unlit管线默认支持基础色、基础色纹理。支持morph targets和skinning。
- 支持自定义的着色器、管线和绑定组。
- 仅3D，不对2D进行特殊优化。
- 顶点属性是压缩的，位置用Snorm16x4，UV用Snorm16x2，两者配合aabb中心和范围、uv最小值和范围解码。顶点色用Unorm8x4，骨骼索引用Uint16x4，骨骼权重Unorm16x4。
- 默认管线的顶点缓冲可以有4个：一个用于位置，一个用于骨骼索引+权重，一个用于UV+顶点色，一个用于逐实例步进的属性（每实例变换矩阵和基础色，因此材质绑定无需基础色）。
- 不自动实例化，支持手动实例化。
- 渲染何时结束是确定性的，便于快照测试。

不支持：
- 视锥剔除、遮挡剔除，用户需自行剔除。
- 光照、阴影。
- 后处理。

## API设计

### 资源

- 拥抱wgpu，直接管理和使用wgpu的资源。不提供不必要的包装；不提供面向CPU数据管理和同步的上层API。但是可以提供便于创建本库特定GPU资源的结构体和函数，如UBO和SSBO结构体声明、Mesh顶点量化和压缩API。
- 内部用一个状态追踪和管理所有GPU资源及它们的依赖关系，数据结构为有向无环图（`petgraph`）。
- 暴露API以允许获取、替换和移除底层的wgpu资源，并且追踪资源：当资源被替换时，标记其为脏，这也意味着依赖它的资源也需要更新；当资源被移除时，它和依赖它的资源都将被移除。
  资源状态的追踪是精准，更新是延迟的，用户调用特定API来使更新资源状态，如在buffer扩容后更新依赖它的绑定组。

![WebGPU资源依赖图](./assets/webgpu-draw-diagram.svg)

### 绘制/RenderPass

绘制过程，即render pass，也遵循数据驱动，用一个声明式的数据结构描述绘制过程所使用的资源及依赖，以允许用户进行自定义、hook和修改。

绘制过程伪代码：
```rust
for pipeline in scene.pipelines {
    pass.set_pipeline(pipeline); // builtin unlit pipeline, and custom.

    // By default, there is 1 global binding in index 0.
    for (idx, global_binding) in pipeline.bindings {
        pass.set_bind_group(idx, global_binding);  // camera, globals (time, delta, frame count), mesh metadata (vertex decoding metadata, morph targets info), and custom.
    }

    for material in pipeline.materials {
        // By default, there is 0 or 1 material binding in index 1.
        for (idx, material_binding) in material.bindings {
            pass.set_bind_group(idx, material_binding); // base color texture, and custom.
        }

        for mesh in material.meshes {
            // By default, there is 1 mesh binding in index 2.
            for (idx, mesh_binding) in mesh.bindings {
                pass.set_bind_group(idx, mesh_binding); // mesh metadata index, joint matrices, morph deltas, morph weights, and custom.
            }

            // By default, there is 3 or 4 vertex buffers: position, uv+color, instance data, and optional joint index+joint weight
            for (idx, vertex_buffer) in mesh.vertex_buffers {
                pass.set_vertex_buffer(idx, vertex_buffer); // per-vertex attributes, per-instance model transform, base color, and custom.
            }

            if let Some(index_buffer) = mesh.index_buffer {
                pass.set_index_buffer(index_buffer);
                pass.draw_indexed(mesh.index_range, mesh.base_vertex, mesh.instance_range);
            } else {
                pass.draw(mesh.vertex_range, mesh.instance_range);
            }
        }
    }
}

```

作为优化：在循环中，快速比较本次循环设置的资源和上次循环设置的资源是否相等，若相等可避免循环中频繁的状态切换。

## 实施计划

- [x] `wgpu`资源管理、基本的pipeline和mesh绘制API。
- [ ] 支持`egui`。
- [ ] 支持skinning和morph targets。

提示：
- 字节转换统一用`zerocopy`。
- 着色器预处理用`wesl`。
- mesh顶点压缩可参考<https://github.com/bevyengine/bevy/blob/main/crates/bevy_render/src/utils.wesl>，<https://github.com/bevyengine/bevy/blob/main/crates/bevy_mesh/src/mesh.rs>，<https://github.com/bevyengine/bevy/blob/main/crates/bevy_mesh/src/vertex.rs>，<https://github.com/bevyengine/bevy/pull/24616>，虽然我们不需要法线和切线，但我们仍然可以提供它们供用户使用。

## 测试

添加GPU集成测试，回读渲染纹理的结果，并保存为图像作为快照，使用图像差异算法库（如`fast-ssim2`）比较图像差异。
