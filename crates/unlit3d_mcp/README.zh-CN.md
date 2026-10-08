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

服务器唯一新增的东西是 **组件注册表**：组件是任意 `'static` 类型，因此单靠一个 world 无法
给它命名。注册表把每个受支持的组件与一对编解码函数配对，正是它把组件变成客户端可读写的
JSON。它是一个普通的公开类型，调用者可以用自己的组件扩展它。

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

**输入**——`input_state` 与 `send_input`。输入事件被推入 world 的 `InputState`，并经由
窗口所用的同一个 `dispatch_input` 派发，因此行为组件看到它们的方式与来自用户时完全一样。

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
