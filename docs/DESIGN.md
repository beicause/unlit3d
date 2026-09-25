# wgpu_unlit_render 和 unlit3d

`wgpu_unlit_render`是基于`wgpu`的、紧凑的、有主见的、内置无光照（unlit）管线的3D渲染器。

`unlit3d`是基于`wgpu_unlit_render`和`unlit_ecs`的、完整的、易用的、更上层的渲染API。
注意：`unlit3d`依然紧密拥抱`wgpu`，并直接使用底层`wgpu`资源。与主流的面向大众用户的游戏引擎不同，
`unlit3d`主要面向拥有图形渲染知识的开发者和编程智能体，不做高级别的封装，你需要有WebGPU知识才能较好地运用它。

目标硬件为WebGPU，并且移动端优先。不支持WebGL、GLES。

编码原则：
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。

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
- `wgpu_unlit_render`分层：核心不依赖任何内置实现；内置的unlit管线由`unlit` feature（默认开启）提供，基于它构建的`egui`后端由`egui` feature提供；关闭`unlit`时WESL包也不再包含unlit着色器模块。
- `unlit3d`同样分层：`egui`与其UI模块由feature门控，不用UI的构建不必编译`egui`；输入与帧源不依赖`egui`，可独立使用。
- UI与3D在同一帧、同一个render pass内绘制，UI叠在mesh之上。
- 输入事件接入ECS，与游戏逻辑共用一条事件流。

不支持：
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

### 帧源：一帧由多个源按序拼成

一帧不是「渲染器的一个主场景加若干附加物」，而是**多个帧源（`FrameSource`）各自产出一个`Scene`**，再按顺序录制，UI与3D因此处于同一帧、同一个render pass内。`Renderer`退化为纯帧级装置：建encoder、按序录制各源产出的`Scene`、提交；它不持有任何绘制逻辑，也不独占GPU上下文或帧源。

- **内置mesh渲染本身就是一个源**（`MeshSource`），与UI源完全对称。渲染器里不留任何mesh专用的绘制路径或特权字段：「3D主场景」只是第一个被录制的源产出的场景，而不是渲染器的特例。用户想换剔除策略、加自己的批处理或画阴影时，可以不挂`MeshSource`而挂自己的实现。
- **每个源自带状态与其可跨帧复用的`Scene`**。`egui`的`Context`、字体图集、UI自己的UBO都归UI源所有；3D的管线、网格池、元数据、剔除缓存都归mesh源所有。
- **帧源与GPU上下文都是世界的普通成员**：帧源是组件，`wgpu::Device`、`wgpu::Queue`与资源图是资源。这不是「把渲染器内部拆开」，而是让帧源与第三方源完全同权：新增或移除一个源就是spawn/despawn一个实体，源在自己的构建阶段直接取用共享上下文，不需要经过渲染器转交。
  - 与前面「没有资源」一节一致：资源是标记组件，资源引用是实体引用。
  - 由此帧级上下文的传递是「按实体引用取用」，而不是「把借用句柄层层递进」，源之间也就不会因为共用一个上下文而产生借用冲突。
  - **反向的取舍**：若把上下文私有在渲染器里，源想同时持有「自己的可变借用」与「资源图的可变借用」就只剩两条路——把渲染器拆成专用入口，或用内部可变性把冲突推到运行期panic。前者让每个调用点都长出一个绕过借用检查的函数，后者用崩溃换方便。让上下文与源都成为世界的成员，这个冲突根本不产生，因此不需要任何为此设计的专用入口。
  - **源的GPU资源由源自己显式释放**，这避免了引用计数自动释放资源的复杂机制。源在资源图里注册的节点不随实体销毁自动回收，移除源时必须显式释放。

**构建与录制是两个阶段**。构建阶段独占资源图以注册资源，并写入本帧要上传的数据；录制阶段只读各源自己的`Scene`，完全不碰资源图。合并成一个阶段就必须在产出`Scene`前读资源图，于是只能用内部可变性把借用冲突推迟到运行期——因此保留两阶段，换取编译期保证。

**源的绘制顺序由每个源显式声明**，而不是靠创建语句的先后隐式表达：

- 顺序是必需方法，且顺序类型不实现`Default`。若给出默认值，「忘记声明顺序」与「确实想沿用创建顺序」就无法区分，顺序意图又变回隐式。
- 顺序相同的源，会记录并发出警告。源的顺序必须是源显式携带的序号，不能借用实体在原型内的行序：实体的行序不保证稳定，移除组件时会被打乱。

`Scene`内部已有的顺序语义（非z-sorted在前、再按管线与材质/深度）不变；源之间的顺序不是被pass级状态逼出来的硬约束（`Scene`已携带必要的pass状态信息，在录制时都会重设pass状态），而是「UI要合成在3D之上」等类似语义的需要。

### UI：egui作为普通帧源

UI不新建平行体系，而是复用资源图、`Scene`、内置unlit管线、staging与帧循环。`wgpu_unlit_render::ui`提供不依赖ECS、不依赖winit的纯后端；`unlit3d`在其上提供UI帧源。

**界面本身是行为组件`UiPanel`，不是源上的闭包字段**。UI源只负责逐一驱动世界里所有`UiPanel`：

```rust
pub struct UiPanel(Box<dyn FnMut(&LocalWorld, Entity, &mut egui::Ui)>);
```

这与「调用者即系统」的约定一致：源是驱动器，它决定调用哪些面板、以什么顺序（查询顺序）；面板要记状态就放在它自己的兄弟组件里（行为组件运行期间被借用，不能重入借用自己）；面板拿到`&LocalWorld`，可以读写组件、也可以`queue()`结构变更。因此多个面板就是多个实体，可以按需增删，第三方也能定义自己的面板类组件由源筛选——源本身不需要知道有哪些界面。

**单位约定是这套API最容易错的地方**：egui的顶点与裁剪矩形使用逻辑点，而窗口事件坐标是物理像素。因此投影用点（物理尺寸除以缩放因子），而scissor必须乘缩放因子——两者单位相反。此外egui为多趟布局会**多次**调用面板闭包，因此面板必须幂等，或按趟数判断该做什么；这是egui的既有语义，所有后端都一样。

UI本身只需要color附件，不需要深度附件，但是管线的深度状态必须与pass的附件**完全一致**（wgpu按格式强校验，不看写入与比较设置）。在带深度附件的pass里（UI与mesh共用同一pass是常见情形），UI管线仍须声明**相同的**深度格式，只靠「不写入深度、不深度测试」来避免干扰mesh的深度——「不需要深度」不等于「不声明深度」。「目标没有深度附件」也是合法用法，因此管线的深度状态是可选的。可选的深度状态服务于**目标本身没有深度附件**的情形，那时pass里的所有管线都必须不声明深度，并非UI独有。这一点与`egui`官方后端一致：它默认不带深度状态，但在被给予深度格式时，仍会建出同一格式、不写深度、比较函数为`Always`的状态。

### 输入：事件与行为组件

事件类型**不依赖winit，也不依赖egui**：核心类型自持，winit与egui的转换放在各自feature之后。这样游戏逻辑与UI共用同一条事件流，不需要两套事件。

- **事件回调是行为组件**（包含闭包的组件），与ECS既有的行为组件范式同构：按事件类别调用所有对应行为组件。`OnInput`收到全部事件，`OnKey`/`OnPointer`/`OnTouch`/`OnText`/`OnIme`各收一类。
- **`InputState`是资源实体**，持有「自上次清空以来到达的事件」以及事件留下的状态（修饰键、光标、按下的按钮、焦点、窗口尺寸与缩放）。事件列表在一帧内被多个消费者读取（分发器与UI源），所以设计为**只读遍历、帧末显式清空**，而不是取走。
- **分发由调用方在帧循环里显式调用**，不由`render`内部自动进行。原因：回调内排队的结构变更需要`&mut world`才能落地，而`render`只拿得到`&LocalWorld`；自动分发会让回调排队的变更延迟到下一次外部`apply`，且这个延迟对用户不可见。显式调用也让调用方掌控分发时机与顺序，与「调用者即系统」一致。
- 分发**直接遍历**行为组件即可，不需要先收集实体：回调借用的是不同实体的不同cell，互不冲突。真正的限制来自ECS自身有意为之的取舍——回调不能重入借用自己那个组件，也不能重入同类分发；要保留状态就放到兄弟组件上。这些限制写进文档而非改造ECS。

UI对输入的**捕获**（是否想独占指针/键盘）按egui的语义需要上一帧的结果：源把这两个标志写回世界，游戏逻辑可以读它决定是否响应。本期只提供数据，不做自动拦截。

### 逐帧数据上传

逐帧变化的数据，如相机和globals uniform、逐实例数据、mesh元数据，通过跨帧复用的staging buffer上传，而不是逐次调用`queue.write_buffer`：

- `queue.write_buffer`每次调用都新分配一个临时staging buffer，并自行提交一次copy，因此既无法与帧内其他工作合批，又每帧都有分配。
- `wgpu`的`StagingBelt`同样不合适：帧间尺寸增长会让它永久持有每种出现过的尺寸的块，且从不释放。
- 因此每个目标buffer自持一个staging buffer池：host写入复用的映射，encoder记录copy，复制完成后映射交还host供后续帧再次使用。

池的大小稳定在在飞帧数，不随帧数增长；帧变大时替换过小的buffer而不是并存；尺寸长期回落后可显式回收。逐帧上传对调用方透明：调用方只维护CPU侧数据，如分配或移除mesh，渲染帧时自动把变更同步到GPU，无需记住调用上传API。这不同于前面「资源」一节中依赖图的延迟更新，后者在buffer等资源被替换后仍由用户调用API触发。

一帧的上传与消费它们的render pass记录进同一个encoder，因此一帧一次提交，这保持了渲染结束的确定性；没有内容的帧也提交，以带上该帧的上传。

### 上层API

上层API在`unlit3d`包中实现，并且依赖于`wgpu_unlit_render`。

基本需求：
- 拓展性和移植性。要将自身视作游戏引擎一部分，考虑架构、拓展性，便于与其他功能如物理、音频等集成。
- 对于公开API，优先使用dyn trait而不是静态分发来提高灵活性和拓展性，不要过分担心性能。
- 一定程度考虑多线程。
- 易用性，根据CPU场景数据同步至GPU缓冲并渲染。
- 允许底层控制，能绕过CPU数据直接更新GPU缓冲。
- 采用CPU视锥剔除。

反思其他引擎的做法：
- bevy，采用ECS架构，好的地方：模块化程度高，插件的拓展性好，ECS能充分利用多线程。不好的地方：
  主要来自ECS的缺点和复杂性：组件之间的依赖关系导致易出错，继承和复用不够明显，复杂对象状态管理困难，场景图用组件表示不如直接用树数据结构和对象表示直观。一些例子：
  1.组件变化检测机制复杂、易失效、易出错，组件依赖更新和派生机制几乎没有。如果每帧同步而不保留组件，开销可能较大。而如果保留组件，则涉及复杂的变化检测和依赖更新问题。例：`bevy_render`的资源提取模式易出错，尤其是在组件增删时，容易泄漏衍生组件，容易漏掉更新或使变化检测失效导致过度更新。复杂性体现在bevy的渲染系统往往很庞大，可能要处理大量组件，并带有大量`Changed`、`RemovedComponents`查询。而在OOP中，对象自身状态在内部维护。
  2.拓展性体现在系统的相加上，而不在组件上，组件缺乏OOP的继承性。例：bevy的材质拓展需要用户为每个自定义材质注册一个插件，较繁琐。bevy的材质所需的GPU句柄往往绑死了`ShaderBuffer`，`Mesh`，`Image`等类型的Handle，用户无法自定义自己的资源句柄用于材质。而这在OOP中，用基类并拓展非常容易。
  由于渲染资源类型单一，bevy的`RenderAssetUsages`机制也易出错：主世界被提取了渲染资源的数据后，再访问它就会报错。
- Godot、Three.js等，采用OOP、场景树/图和中心化渲染器，好的地方：易用性好，Godot的场景树轻松可以拆分和组合。API可以非常上层（如Godot的节点树），也可以比较底层（如Godot的服务器模式）。不好的地方：
  1.对于Rust来说，OOP和继承不方便，强行模拟OOP很别捏。
  1.不擅长处理大量实体，遍历节点树开销较高。2.不像ECS那样自动和充分利用并行。并行是粗粒度的，需要手动调整。Godot的并行体现在服务器、节点的每帧更新逻辑可以选择后台线程，服务器和节点内部逻辑不能并行，除非手动调用线程池。

功能提议：
- ECS，即unlit_ecs库
  - ECS框架保持精简，用Archetype储存，目前没有用到的功能绝不添加。
  - 没有变化检测、组件钩子等。
  - 没有实体关系，如Children，ChildOf等。理由：让用户根据需要自行管理实体之间的引用关系。
  - 没有事件和观察者。作为替代可以直接调用组件的方法或行为组件。“行为组件”指包含函数指针或闭包的组件，并且该函数中能运行对world的访问，就像系统或观察者一样。
    并且行为组件内的函数可以是async的。
    另外你可以将类似Godot的节点内部的帧更新、固定间隔更新、按键鼠标触摸输入等回调视作实体上的一些行为组件，然后由外部调用者访问世界调用这些行为组件，由此通过组合的方式实现此类功能。
  - 没有资源。资源是标记组件。资源实体可被所有行为组件访问，无视数据隔离。资源引用是实体引用。
  - 实体的原型不可变，组件不能运行时增删，只能读写，要改变实体上的组件种类必须despawn再重新spawn。
    理由：在OOP中类/对象的状态和行为是不可变的，不可变的原型也使得状态保留有强制性，避免bevy的`RemovedComponents`和组件泄漏的坑。
  - 没有系统（借鉴自`hecs`）。系统只是外部调用者对世界的访问，或是包含行为（闭包函数）的组件。
    因此不用做复杂的系统并行性（依赖、访问冲突等）分析、多线程调度器。让外部调用者自行决定运行的线程。
  - `!Send`世界和`Send`世界分离，两者用channel传递数据。只有`Send`世界的组件才是`Send`，`!Send`世界只能在主线程运行。GPU资源和渲染器是`!Send`组件，为了兼容`wasm32`。

- 渲染，即unlit3d库
  - render世界系统驱动渲染。渲染器是组件，在render系统中渲染所有带有renderable组件的实体，并输出到render target上（纹理或交换链）。相机是渲染器的一个数据组件。
  - 自定义管线是一等公民，unlit管线基于此之上。
  - 管线可以在运行时针对顶点布局、透明度混合等参数特化。
  - 渲染器本身只负责帧级装配，一帧的绘制由多个帧源按序拼成（见上文「帧源」）。

## 实施计划

- [x] `wgpu`资源管理、基本的pipeline和mesh绘制API、unlit管线。
- [x] 支持`egui`（`wgpu_unlit_render::ui`的纯后端）。
- [x] 初步实现`unlit_ecs`。
- [ ] 设计并实施上层渲染API。
- [ ] 帧源重构：`Scene`自持句柄、`FrameSource`机制、内置mesh渲染搬进`MeshSource`。
- [ ] `unlit3d`的UI集成：`UiPanel`行为组件与UI帧源。
- [ ] 输入：事件类型、`InputState`、行为组件与winit适配。
- [ ] 支持skinning和morph targets。

提示：
- 字节转换统一用`zerocopy`。
- 着色器预处理用`wesl`。
- mesh顶点压缩可参考<https://github.com/bevyengine/bevy/blob/main/crates/bevy_render/src/utils.wesl>，<https://github.com/bevyengine/bevy/blob/main/crates/bevy_mesh/src/mesh.rs>，<https://github.com/bevyengine/bevy/blob/main/crates/bevy_mesh/src/vertex.rs>，<https://github.com/bevyengine/bevy/pull/24616>，虽然我们不需要法线和切线，但我们仍然可以提供它们供用户使用。

## 测试

添加GPU集成测试，回读渲染纹理的结果，并保存为图像作为快照，使用图像差异算法库（如`fast-ssim2`）比较图像差异。
