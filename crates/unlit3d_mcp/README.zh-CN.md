[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d_mcp/README.md) | 简体中文

# unlit3d_mcp

一个 [Model Context Protocol](https://modelcontextprotocol.io) 服务器，让 agent 能读取并
驱动一个运行中的 unlit3d 世界。包名为 `unlit3d_mcp`，构建出的可执行程序名为
`unlit3d-mcp`。与它所用的那些 crate 一样，它也处于 **早期阶段**。

## 没有特权路径

这个服务器之所以是独立 crate，是有原因的：它没有调用者所没有的权限。每个工具都写在同一个
公开 API 之上——`World`、`Renderer`、`MeshSource`、`ResourceGraph`——`unlit3d` 与
`unlit_wgpu` 都不知道协议的存在。内置管线没有私有通道，服务器也没有捷径：服务器能做的，
调用者都能做；调用者做不到的，服务器也做不到。

唯一必须写下来的是：一个 JSON 桥能命名哪些组件——组件是任意 `'static` 类型，因此单靠一个
world 无法给它命名。组件通过向 `unlit3d` 的 `reflect` feature 里的一张链接期表提交条目而
变得可寻址；条目把类型与一条进出 JSON 的通道配对。

对多数组件来说，没有第二份声明要写：组件用 [`facet`](https://docs.rs/facet) 派生出
自己的反射，于是它的结构体字段**就是** JSON 形状，`ComponentEntry::new`（对有自然默认值
可用来合并局部值的组件则是 `ComponentEntry::default`）把这份反射变成编解码器。字段类型若
归 glam 或 ECS 所有，则通过同一个 feature 提供的代理来反射——服务器正是因此启用它。字段
无法反射的组件（GPU 句柄、私有字段）改为经由代理反射；不能写入的组件会让代理的转换失败，
于是写入会报告原因，而不是编造一个值。

条目从写下它的任何地方被收集，因此定义自己组件的 crate 用 `inventory::submit!` 自行注册
——无需把注册表交给谁。这张表是链接期的，因此从未被二进制引用到的 crate，其条目也不会被链
接进来；想要某个 crate 的组件的二进制必须指名该 crate。

组件用它的裸类型名（`Transform`）或模块限定名（`unlit3d::components::Transform`）寻址；
后者用来区分不同模块里的同名组件。

## 传输

传输是 **stdio**：服务器从 stdin 读 JSON-RPC 请求，把响应写到 stdout。这是 MCP 客户端启动
命令时所用的传输，也是本 crate 唯一实现的传输。它基于 [`rmcp`](https://docs.rs/rmcp) 与
tokio，二者都是本 crate 的非可选依赖；workspace 的其余部分从不会看到它们。

由于 world 不是 `Send`，服务器跑在两个线程上：渲染线程持有 `World` 并运行 host，协议线程
持有 tokio 与 `rmcp` 服务。每个工具都会变成一条发往渲染线程的命令，由它执行并把 JSON 值
发回。两者从不共享 world。

## 工具

服务器按四组暴露 world 的公开能力。

**World 与实体**——`world_summary`、`list_components`、`list_archetypes`、
`list_entities`、`get_entity`、`get_component`、`set_component`、`spawn_entity`、
`despawn_entity`。实体用它的 `u64` 位模式命名，组件用注册名命名。实体在生成之后无法增删
组件——这是 ECS 的约定，不是服务器的限制——所以 `spawn_entity` 接收一整套组件，而
`set_component` 只原地覆盖值。有自然默认值的组件可以逐字段给出；`set_component` 会把它
收到的字段合并到实体已有的值之上。

**资源图**——`graph_summary`、`list_resources`、`resource_dependencies`、
`graph_maintain`、`read_buffer`、`read_texture_as_image`。资源用它的槽位序号命名，
只要资源存活该序号就稳定。`graph_maintain` 就是渲染循环所跑的那一趟：丢弃无引用的资源、
重建脏的资源。`read_buffer` 会拒绝没有以 `COPY_SRC` 创建的缓冲，而不是让设备拒绝这次
拷贝。

**输入**——`input_state` 与 `send_input`。`input_state` 报告 `InputState` 持有的状态，
其中包括自上次清除以来到达的事件，因此调用者能看到这一帧发生了什么。`send_input` 把输入
事件推入 world 的 `InputState`，并经由窗口所用的同一个 `dispatch_input` 派发，因此行为组件
看到它们的方式与来自用户时完全一样。

**绘制**——`create_mesh`、`remove_mesh`、`render_frame`、`screenshot`、
`load_gltf`。`create_mesh` 从位置（以及可选的 uv、颜色与索引）分配一个 unlit 网格，并
生成绘制它的实体；`remove_mesh` 再把它释放。`render_frame` 画一帧，`screenshot` 画一帧
并把离屏目标作为 base64 PNG 返回。

## 库

可执行程序只是一层薄封装：`serve_stdio` 启动渲染线程与之上的协议。已经拥有 world 的调用者
用 `Host::from_world` 构建一个 `Host` 并改为服务它；`Host::new_offscreen` 则新建一个空
world，自带相机、一个 unlit 家族与它自己的渲染目标。

```text
cargo run -p unlit3d_mcp --bin unlit3d-mcp
```

命令行工具与示例都接受 `--mcp` 开关，把原本要渲染的那个 world 经 stdio 服务出去：

```text
cargo run -p unlit3d_cli --bin unlit3d-cli -- --mcp
cargo run -p unlit3d_examples --bin unlit3d-examples -- --mcp --scene spin_cube
```

## 测试

`tests/stdio_server.rs` 把真正的可执行程序作为子进程启动，并用 JSON-RPC 与它对话：列出
工具、读取 world、写入清除色、生成一个网格，再逐像素检查截图。这个 world 是真实的 GPU
world，因此测试验证的是整条路径——协议、host、渲染、回读——而不是彼此孤立的各个部分。

```text
cargo nextest run -p unlit3d_mcp
```

## 许可证

双许可，任选 MIT 或 Apache-2.0。
