[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit3dx/README.md) | 简体中文

# unlit3dx

面向游戏的多 world 运行时：渲染、物理、音频与玩法各自运行在自己的 world 里，以
发出去的 job 和传回来的快照相连。包名为 `unlit3dx`。与它所用的那些 crate 一样，
它也处于 **早期阶段**。

## 用法

核心只有三个动作：生成一个 world、给它发 job、读它的快照。

```rust,ignore
use std::time::Duration;

use unlit3dx::prelude::*;
use unlit_ecs::World;

let mut host = World::new();

// 远程 world：一个 `World` 加一条线程，由这个闭包驱动 tick。
let simulation = spawn_remote_world(&mut host, "simulation", 16, |world, control| {
    // ... 在 `world` 里推进模拟 ...
    control.wait_timeout(Duration::from_secs_f64(1.0 / 64.0));
    !control.stop_requested()
});

// job 在远程线程的 tick 之间运行；request 会在答案就绪时把它带回来。
let proxy = host.get::<RemoteWorld>(simulation).expect("the proxy exists");
proxy.send(Box::new(|world: &mut World| world.spawn((0u8,))));
let spawned = proxy.request(Box::new(|world: &mut World| world.spawn((0u8,)).to_bits()));
// job 一旦运行过，`spawned.poll()` 就是 `Some(entity_bits)`。

// 快照是唯一回传的数据：在 world 的线程上 publish，在任意地方读最新一份，
// 谁也不阻塞谁。
let (writer, reader) = snapshot_pair::<TransformFrame>();
writer.publish(TransformFrame { transforms: Vec::new() });
if let Some(frame) = reader.latest() {
    // ... 应用 `frame.transforms` ...
}
```

按 feature 划分的模块在这套核心之上搭场景。**子 world** 让持有者留在主线程上，
但接收 job 的方式与远程 world 相同：

```rust,ignore
let render = spawn_sub_world(&mut host, 64); // 64 个排队 job
let mut nested = host.get_mut::<World>(render).expect("the nested world");
drain_sub_world(&mut nested); // 运行其他线程投递的 job
```

**render** 桥的 `RenderServer` 把它变成资源服务器：`create_mesh`、
`create_material` 与 `spawn_instance` 这些 job 返回实体位，远程的玩法 world
把它们当作句柄持有。**physics** 桥发布刚体变换与碰撞事件的 `PhysicsSnapshot`，
**synth** 桥把 note job 与 MIDI 文件变成音色库音频。`unlit3dx_examples` crate 把所有桥
放进同一个场景。

## 为什么要分离 world

`unlit_ecs` 的 world 不是 `Send`：组件藏在 `RefCell` 后面，结构性修改需要
`&mut World`。常见的答案是用锁把一个 world 跨线程共享起来，于是每个 system 都
变成争用点，每次访问都变成一场借用辩论。`unlit3dx` 给出另一个答案：每个关注点
一个 world，永不共享。

- **子 world（sub-world）** 是挂在宿主实体上的嵌套 `World`，由同一条线程持有。
  渲染 world 就是其一：它在主线程上绘制，宿主只在事件之间进入它。它同样有一个
  job 收件箱：`spawn_sub_world` 把收件箱接好，其他线程通过宿主实体携带的
  `SubWorld` 代理投递 job，持有者在自己的步骤之间调用 `drain_sub_world`——
  与远程 world 相同的协议，只是没有线程。
- **远程 world（remote world）** 是属于自己的线程的 `World`。物理、音频与玩法
  各运行一个。

world 之间从不互相调用。调用者发送一个 **job**——一个接收远程 world 的
`&mut World` 的装箱闭包——然后一份 **快照** 经由单槽信箱传回来。这就是全部协议。

## Job 与请求

`Job<R>` 是装箱的 `FnOnce(&mut World) -> R`。`RemoteWorld::send` 把一个 job 排入
队列；远程线程在每次 tick 之前先清空队列，因此 job 永不与 tick 竞争。
`RemoteWorld::request` 给 job 配上一个有界回复通道，返回一个可以稍后 `poll` 或
`recv` 的 `Request<R>`：发送是即发即忘的，答案什么时候到就什么时候到。`poll_all`
只在整批答案都就绪时才取出整批结果——过早地取批不会弄丢已经作答的请求。丢弃一个
`Request` 没有任何代价；丢弃一个 job 的结果只是没人去读而已。

## 快照

`SnapshotWriter<T>` 与 `SnapshotReader<T>` 是一个单槽信箱：写入方 `publish` 最新
状态，读取方 `latest` 窥视它。没有积压，也没有需要推演的顺序——最新的快照获胜，
未被读取的那份直接被替换。按 tick 产出快照的世界与按帧读取快照的消费者在中间
会合，谁也不阻塞谁。

## 线程

`WorldThread::spawn` 持有这个循环：它检查 `ThreadControl::stop_requested`，在
`catch_unwind` 里运行 tick，并在线程结束前把 panic 通过通道报告出去，于是宿主
知道自己的 world 为何而死，而不是 join 到一具尸体。`ThreadControl::wait_timeout`
为 tick 配速，`FixedClock` 在其上为想要稳定步率、独立于帧率的世界服务。丢弃一个
`RemoteWorld` 会停止并 join 它的线程。

## 桥

按 feature 划分的模块就是世界之间的桥：

- **render**（默认开启）：`apply_transform_frames` 每帧把 `TransformFrame` 快照
  变成渲染 world 里的变换，赶在任何绘制之前；`RenderEntity` 标记被模拟驱动的
  渲染实体。`RenderServer` 是资源服务器的那一半：`create_mesh`、
  `create_material` 与 `spawn_instance` 这些 job 在渲染 world 里运行并返回
  实体位，远程的玩法 world 借此搭建它从不直接触碰的场景——`GpuMesh` 持有
  `Rc`，所以跨线程的句柄就是实体位。
- **physics**（默认开启）：`spawn_physics_world` 以固定步率（`PHYSICS_HZ`，64）
  驱动 rapier 的 `PhysicsWorld`，并发布一份 `PhysicsSnapshot`——每个运动刚体的
  变换，加上这一步开始与结束的碰撞对，以及每个新接触的速度。
  `spawn_body`（携带 `BodyShape` 的 `BodyDesc`）与 `spawn_fixed_cuboid` 是基本构件，
  `PhysicsBody` 标记实体拥有的刚体。
- **audio**（默认开启）：`spawn_audio_world` 打开输出设备并在音频线程上持有其
  流。`PcmSource` 填充由 `AudioConfig` 定长的环形缓冲；设备回调消费它，欠载时静音。
  玩法从不直接触碰音频——它发送 job。代理还携带一个 `AudioReadyInbox`：world 在真正
  能出声时发布状态，调用方由此区分音色库加载中与设备损坏，并丢弃过期的音符而非
  把它们排在加载之后一次性齐放。
- **synth**（默认开启，蕴含 audio）：`synth_source_bytes` 从音色库字节构建混音器——通常是 `include_bytes!`，不经过文件系统——在音频线程上加载
  [rustysynth-ext](https://docs.rs/rustysynth-ext) 合成器，
  `note_on_job`/`note_off_job`/`notes_off_all_job` 是作用于它的现成 job。
  `play_midi_job` 与 `stop_midi_job` 驱动一台 `MidiPlayer`：MIDI 文件音序器，
  由 `load_midi_file`（SMF 文件）或 `midi_file`（一列定时消息）喂入。`gain`
  参数缩放混音，补上音色库预留的余量。
- **mcp**（默认关闭）：`serve_mcp` 把宿主 world 及其子 world 通过
  [模型上下文协议](../crates/unlit3d_mcp/README.zh-CN.md)对外服务，场景
  运行时即可从终端检视与截图。

## 应用骨架

多 world 应用面向窗口的那一半同样与应用无关，所以随运行时一起提供。**gpu**
模块在主线程之外请求 `Gpu`，把答案送回主线程上的一个闭包——wasm 安全，不阻塞：

```rust,ignore
world.spawn_frame_behaviour(request_gpu(app, |world, gpu| {
    // ... 在请求发起处搭建场景 ...
}));
```

`Gpu::headless` 是同一个请求的免窗口版本，供测试与工具使用。**present** 模块
持有从子 world 到窗口的全部管线：`setup_presenter` 把场景渲染进离屏目标再
blit 到交换链（这也让帧存活得足够久、可以读回——截图与快照测试正依赖这一点），
`draw_presented` 是每帧排空子 world、分发帧并呈现的行为，`pump_presented` 在窗口
存活期间让帧持续到来，`resize_presented` 跟随窗口变化。网格与相机辅助——
`uv_sphere`、`cuboid_mesh`、`camera_looking_at`、`linear_sampler`——补全骨架。

## 测试

`cargo nextest run -p unlit3dx` 覆盖核心：job 往返、请求回复、快照替换、线程停止
与 panic 报告，以及远程 world 的已发送 job 运行。
