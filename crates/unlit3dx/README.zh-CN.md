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
// spawn 返回一个 `WorldHandle`——代理所在的实体、命令队列与线程状态
// 合于一身。
let simulation = spawn_remote_world(&mut host, "simulation", 16, |world, control| {
    // ... 在 `world` 里推进模拟 ...
    control.wait_timeout(Duration::from_secs_f64(1.0 / 64.0));
    !control.stop_requested()
});

// job 在远程线程的 tick 之间运行；request 会在答案就绪时把它带回来。
simulation.send(Box::new(|world: &mut World| world.spawn((0u8,)))).expect("room in the queue");
let spawned = simulation.request(Box::new(|world: &mut World| world.spawn((0u8,))));
// job 一旦运行过，`spawned.poll()` 就是 `Some(entity)`。

// 快照是唯一回传的数据：在 world 的线程上 publish，在任意地方读最新一份，
// 谁也不阻塞谁。快照装什么是调用方自己的类型，怎么读也是：拥有 reader 的那个
// 帧行为负责把内容写进它所在的 world。
let (writer, reader) = snapshot_pair::<Vec<u64>>();
writer.publish(Vec::new());
if let Some(latest) = reader.latest() {
    // ... 应用 `latest` ...
}
```

按 feature 划分的模块在这套核心之上搭场景。**子 world** 让持有者留在主线程上，
但接收 job 的方式与远程 world 相同：

```rust,ignore
let render = spawn_sub_world(&mut host, "render", 64); // 64 个排队 job
let mut nested = host.get_mut::<World>(render.entity()).expect("the nested world");
drain_sub_world(&mut nested); // 运行其他线程投递的 job
```

**physics** 桥发布刚体变换与碰撞事件的 `PhysicsSnapshot`，**synth** 桥把 note job 与
MIDI 文件变成音色库音频。render 桥是调用方自己的实现：`unlit3dx_examples` crate 在
`unlit3d` 的 `MeshSource` 注册表之上写了一个，并把所有桥放进同一个场景。

## 为什么要分离 world

`unlit_ecs` 的 world 不是 `Send`：组件藏在 `RefCell` 后面，结构性修改需要
`&mut World`。常见的答案是用锁把一个 world 跨线程共享起来，于是每个 system 都
变成争用点，每次访问都变成一场借用辩论。`unlit3dx` 给出另一个答案：每个关注点
一个 world，永不共享。

- **子 world（sub-world）** 是挂在宿主实体上的嵌套 `World`，由同一条线程持有。
  绘制 world 就是其一：它在主线程上绘制，宿主只在事件之间进入它。它同样有一个
  job 收件箱：`spawn_sub_world` 把收件箱接好，其他线程通过它返回的
  `WorldHandle` 投递 job，持有者在自己的步骤之间调用 `drain_sub_world`——
  与远程 world 相同的协议，只是没有线程。
- **远程 world（remote world）** 是属于自己的线程的 `World`。物理、音频与玩法
  各运行一个。

world 之间从不互相调用。调用者发送一个 **job**——一个接收远程 world 的
`&mut World` 的装箱闭包——然后一份 **快照** 经由单槽信箱传回来。这就是全部协议。

## Job 与请求

`Job<R>` 是装箱的 `FnOnce(&mut World) -> R`。`WorldHandle::send` 把一个 job 排入
队列且从不阻塞：队列满了返回 `Err(SendError::Full)`，world 已停返回
`Err(SendError::Closed)`，因此一个向自己的队列发送的 world 线程不会与自己死锁。
远程线程在每次 tick 之前先清空队列，因此 job 永不与 tick 竞争。
`WorldHandle::request` 给 job 配上回复通道，返回一个可以 `poll`、`recv` 或
await 的 `Request<R>`：`poll` 得到 `Option<R>`，`recv` 得到
`Result<R, RequestError>`，而 job 被丢弃（队列拒绝、world 已死，或 job 自己
panic）的请求报告 `Err(RequestError::Dropped)`，而不是永久阻塞。`Request` 实现了
`Future`，所以一批请求是被 join 而不是被轮询：`executor::try_join_all` 把一批合成
一个按请求顺序给出 `Result<Vec<R>, RequestError>` 的 future，由
`executor::LocalTasks` 驱动。框架创建的每个 world 都以组件形式携带一个任务集，
因此任务与 world 同寿、属于 world 而不属于发起它的那次调用；谁驱动 world，谁就
每回合泵它一次——线程在 tick 之前，宿主在分发之前。`LocalTasks::pump` 把集合里
每个任务轮到无法再推进就返回、从不阻塞，因此固定步长的 tick 或帧行为都能持有
这个 await。`unlit3dx_examples` 里搭建场景用的就是这个形状。

## 快照

`SnapshotWriter<T>` 与 `SnapshotReader<T>` 是一个单槽信箱：写入方 `publish` 最新
状态，读取方 `latest` 窥视它。没有积压，也没有需要推演的顺序——最新的快照获胜，
未被读取的那份直接被替换。按 tick 产出快照的世界与按帧读取快照的消费者在中间
会合，谁也不阻塞谁。

## 线程与步进

world 通过一个 *runner* 生成，由它决定谁来调用 tick 循环。`WorldRunner` 就是把这个
选择做成泛型，而它是两种模式之间唯一的差别：有界 job 队列、快照信箱、宿主上的代理
实体、生成返回的句柄全都一样，因此宿主的实体与组件不随 runner 改变，tick 体内也
无需知道自己在哪种模式下。

- **`Threaded`** 给 world 一条自己的线程。`WorldThread::spawn_with` 持有这个循环：
  它先运行一次性的 `init` 闭包（在第一次清空 job 队列之前），再检查
  `ThreadControl::stop_requested`，在 `catch_unwind` 里运行 tick，并在线程结束前
  报告 panic，于是宿主知道自己的 world 为何而死，而不是 join 到一具尸体。销毁代理
  实体会请求停止其线程并至多等待两秒；拒绝停止的 tick 会被遗弃（detach），而不是把
  销毁方挂住。句柄负责汇报线程状态——`is_finished`、`panic_message`——`stop` 则
  请求它结束；`join_timeout` 与 `detach` 在 `WorldThread` 本身之上，留给直接持有
  它的调用方。
- **`Stepped`** 让 world 留在它被生成的地方。`spawn_stepped_world_with_init` 把嵌套的
  `World` 放进宿主实体上的代理组件里，`step_stepped_world` 在调用方的线程上跑一次
  迭代：先清空队列，再跑 tick，各自都在 `catch_unwind` 下——与线程版的顺序一致。
  某次步进让 world 走到终点后它被标记为已完成；`with_stepped_world` 把嵌套 `World`
  借出来，供需要直接触达它的工作使用，例如从绘制 world 读回一帧。

模式改变的是配速。`ThreadControl::wait_timeout` 会阻塞一条真实线程，而当 control
并非自驱时立刻返回，于是同一个 tick 体服务两者：在 `Threaded` 下 `FixedClock` 等
一个挂钟期限过去，在 `Stepped` 下这次 tick 在下次调用时就又到期了。因此步进一个
world 的调用方自己拥有节奏——由它决定一帧值多少步，示例里的 `SceneStepper` 正是
这么做的。

浏览器与离屏运行用的就是步进模式。`wasm32-unknown-unknown` 没有线程可生成，而快照
测试想要自己掌控的帧数，而不是由时钟决定的那一个。

## 桥

按 feature 划分的模块是世界之间的桥。它们是核心之上的便利层，而非特权：每一座都把
一个外部库——rapier、cpal、rustysynth-ext——包进同一套 job 与快照的形状里，想要换库的
调用方就自己搭这套形状。`unlit3dx_examples` crate 就是现成的范例：它的 render 桥是写在
示例 crate 里的资源服务器，建立在 `unlit3d` 的 `MeshSource` 这个通用注册表之上，注册
自己的管线族、经由同一套 API 分配资源。渲染 world 的管线选项来自其中放入的
`UnlitPipelineKey` 组件；不放则由 mesh source 的设备推导出标准选项。

各桥如下：

- **physics**（默认开启）：`spawn_physics_world_with::<R>` 以固定步率（`PHYSICS_HZ`，64）
  驱动 rapier 的 `PhysicsWorld`，并发布一份 `PhysicsSnapshot`——每个运动刚体的
  变换，加上这一步开始与结束的碰撞对，以及每个新接触的速度。
  `spawn_body`（携带 `BodyShape` 的 `BodyDesc`）与 `spawn_fixed_cuboid` 是基本构件；
  `init_physics_world` 为自行用 `step_once` 内联步进的调用方构建同样的 world。
- **audio**（默认开启）：`spawn_audio_world_with::<R>` 打开输出设备并在音频线程上
  持有其流。它的 `OnAudioFill` 组件就是音源：`dispatch_audio_fill` 每块运行一次它们，把各自叠加的
  样本混进由 `AudioConfig` 定长的环形缓冲，并按 world 的 `AudioGain` 缩放混音；
  设备回调消费这个缓冲，欠载时静音。没有任何音源的 world 不出声。
  玩法从不直接触碰音频——它发送 job。代理实体携带两个收件箱：`AudioFormatInbox`
  由音频线程在流打开的那一刻填满——按固定速率渲染的音源必须按设备实际采用的速率构建，
  而这一点在流打开之前无从知晓；以及 `AudioReadyInbox`，报告 world 真正能出声的时刻，
  调用方由此区分音色库加载中与设备损坏，并丢弃过期的音符而非把它们排在加载之后一次性齐放。
  因此安装音源就是这个 world 的普通工作：等待格式，再把生成音源的 job 排进队列。

  步进的音频 world 完全不打开设备：没有可播放的目标，也没有回调来消费环形缓冲。它
  也不发布格式，因此想要出声的调用方自行提前安装音源，并在需要一块时自己调用
  `dispatch_audio_fill`——示例的离屏运行正是这样把混音写成文件的。`dispatch_audio_fill` 就是
  音频线程每块的全部工作，所以直接步进它得到的是同一套混音，只是少了环形缓冲。

- **synth**（默认开启，蕴含 audio）：安装所需的构件，构建在
  [rustysynth-ext](https://docs.rs/rustysynth-ext) 之上——`load_soundfont` 解析字节
  （通常是 `include_bytes!`，不经过文件系统），`SynthVoice::from_font` 生成一台实时音色，
  `MidiPlayer::from_font` 生成一条按序列播放的音轨，各自带着渲染它的 `OnAudioFill` 音源；
  world 的 `AudioGain` 由调用方生成，补上音色库预设预留的余量。`note_on_job`/`note_off_job`
  是作用于合成器的现成 job。传送 job 按名字寻址音轨——`play_track_job` 播放一个文件（可选循环），
  `pause_track_job` 冻结它的音序器时钟，于是恢复时正好接在暂停处，`rewind_track_job` 回到开头，
  `stop_track_job` 放下文件。音轨彼此独立，故一段背景音乐循环与一段单次序列可共存于同一个音频 world 并各自暂停。
  文件来自 `load_midi_file`（SMF 文件）或 `midi_file`（一列定时消息）。示例里的 `install_synth`
  把整个安装收进一个函数，作为 job 排给音频 world。`MidiPlayer::ends` 数的是音轨播完的次数：
  循环播放永不计入，而记住上次计数的读者不会漏掉任何一次结束，即使中间一次都没查过。这个计数
  只是播放器的一个普通事实，需要它的人自己向音频 world 查询——快照只保留最新值，会丢掉两次
  帧之间来了又走的结束。
- **mcp**（默认关闭）：`serve_mcp` 把宿主 world 及其携带的每个 world——嵌套的
  子 world，或自有一条线程的 world——通过
  [模型上下文协议](../unlit_ecs/README.zh-CN.md)对外服务，场景
  运行时即可从终端检视与截图。每次生成都会在代理实体上、紧挨着代理留下一个
  `WorldSender` 与一个 `WorldName`：前者让传输层能把命令投进一个它借不到的
  world，后者就是 `list_worlds` 报告的名字。本 crate 自带的四个工具——
  `start_audio_recording`、`stop_audio_recording`、`cancel_audio_recording`
  与 `audio_recording_status`——在指名的 world 上驱动一次录音，并把混音写成
  WAV 文件。录音音源挂在填充顺序的最末，因此它捕获到的是所有其他音源与 world 的
  `AudioGain` 之后的混音，样本写成一个 RIFF/WAVE 文件，无需额外依赖。录音器只为这
  些工具编译，故 `mcp` 自身蕴含 `audio`。

## 最佳实践

两条规则让场景的各个 world 彼此独立，也正是场景可移植的原因：同一个场景跑在线程上或
被步进、跑在窗口里或离屏，都是因为没有一种模式需要伸手进另一个 world 的数据。

1. **world 的搭建只做配置。** 生成 physics 或 audio world——或调用方自己的绘制
   world——时传入的是它运行时所需的配置——设备、队列与能力，tick 频率，重力，缓冲
   大小——并构建该运行时自己拥有的通用资源：资源图与渲染器、网格源、rapier world、
   输出流。其中不出现任何游戏里的东西：没有球、没有音轨、没有机位、没有控制面板。
   一个空着启动的 world 也能早早启动——绘制子 world 在 GPU 到位之前就存在，于是玩法
   从第一次 tick 起就持有它的句柄，之后再填充它。
2. **对 world 的一切游戏性改动都以 job 到来。** 网格、材质与实例向 render 桥
   请求；刚体通过 physics 队列生成与销毁；音源与传送作为 job 送往音频 world，而一整套
   安装——见示例的 `install_synth`——就是一个函数包成一个 job。快照信箱承载的是从
   world 里**取出**的状态，读它的一方决定这意味着什么；而另一个 world 的内容**变成**
   什么仍然是 job，因此玩法从物理快照推出的姿态是发给 render world 的 job，而不是留在
   信箱里等它自己去应用。

检验标准是一行搭建代码能否属于另一个场景：若它提到了游戏称之为某个东西的东西，
它就该是一个 job。

## 应用骨架

多 world 应用面向窗口的那一半同样与应用无关，所以随运行时一起提供。**gpu**
模块把 `Gpu` 请求做成被请求搭建的那个 world 自己的任务，由每帧开头的那次泵送
驱动，答案落地后把 `ready` 作为排队的 command 运行——什么都不阻塞，wasm 安全：

```rust,ignore
world.spawn((request_gpu(app, |world, gpu| {
    // ... 在请求发起处搭建场景 ...
}),));
```

`Gpu::headless` 是同一个请求的免窗口版本，供测试与工具使用。**present** 模块
持有从子 world 到窗口的全部管线：`setup_presenter` 默认让场景直接画进交换链；
打开离屏路径后，每帧渲染进一个常驻目标，再由第二个 world blit 到表面——这让帧
存活得足够久、可以读回，截图与快照测试就走这条路。`draw_presented` 是每帧排空
子 world、分发帧并呈现的行为，`pump_presented` 在窗口
存活期间让帧持续到来，`resize_presented` 跟随窗口变化。绘制 world 所需的 render
桥——资源服务器及其周围的网格与相机辅助——由调用方自己构建；`unlit3dx_examples`
自带一份，位于 `src/render.rs`。

## 测试

`cargo nextest run -p unlit3dx` 覆盖核心：job 往返、请求回复、快照替换、线程停止
与 panic 报告、远程 world 的已发送 job 运行，以及步进 world 的同一批行为——它的
job 在步进时运行、走完终点的 tick 会结束它、步进一个不携带步进 world 的实体什么
也不做。集成测试补上各桥：物理 world
把球丢到地板上并报告接触、音频 world 运行 job 并发布就绪状态、多 world 的 job 与
快照往来，以及 panic 的 job 只失败自己的请求。录音也有覆盖：它捕获设备本会播放的
混音并把样本排成 RIFF/WAVE 文件，音频 MCP 工具则把两者串进下面这套分发覆盖。打开 `mcp` feature 后，MCP 命令分发也有覆盖：
world 列表、路由到嵌套 world、路由到自有一条线程或由宿主步进的 world，
以及指向不带 world 的实体、已经停止的 world、拒绝该 job 的队列时给出可读的失败；
音频录音工具同样有覆盖，从开始录音、拒绝第二次录音，到把捕获的混音写出去。
