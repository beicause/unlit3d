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

// 独占一条线程的 world：一个 `World` 加一条线程，由这个闭包驱动 tick。
// spawn 返回一个 `WorldHandle`——代理所在的实体、命令队列与线程状态
// 合于一身。
// 由时钟把循环压在 60 Hz：每次 wait 睡到下一步到期，
// 因此偶尔跑快的一轮不会把节拍推乱。
let mut clock = FixedClock::new(Duration::from_secs_f64(1.0 / 60.0));
let simulation = spawn_threaded_world(&mut host, "simulation", 16, move |world, control, _| {
    // ... 在 `world` 里推进模拟 ...
    // 驱动 world 的一方在 tick 跑完后替它向订阅者发布快照，
    // 因此 tick 体不需要自己发布。
    clock.wait(control);
    !control.stop_requested()
});

// job 在该 world 线程的 tick 之间运行，且不回答任何东西：要带回的值发布出去，或者用 Pending 请求回来。
simulation.sender.send(Box::new(|world: &mut World| world.root_scope_mut().spawn(bundle![0u8]))).expect("room in the queue");

// 快照是两种回传方式之一（另一种是承载事件的流），而且按需生成：world 每 tick 运行一次订阅携带的收集闭包，
// 把它返回的内容发布出去。快照装什么是调用方自己的类型——本库不定义任何载荷——
// 所以场景想要什么，由收集闭包决定。
let reader = simulation.sender.watch(|world: &World| {
    let bodies: Vec<u64> = world.root_scope().query::<&u64>().map(|(_, id)| *id).collect();
    Some(bodies)
}).expect("room in the queue");
if let Some(latest) = reader.latest() {
    // ... 应用 `latest` ...
}
// 丢弃 reader 即退订：world 不再为它收集。
```

按 feature 划分的模块在这套核心之上搭场景。**子 world** 让持有者留在主线程上，
但接收 job 的方式与独占线程的 world 相同：

```rust,ignore
let render = spawn_sub_world(&mut host, "render", 64); // 64 个排队 job
let mut nested = host.root_scope_mut().get_mut::<World>(render.entity).expect("the nested world");
drain_sub_world(&mut nested); // 运行其他线程投递的 job
```

**physics** 桥每步把 rapier 的碰撞事件排空进 `PhysicsContacts` 组件，**synth** 桥把
note job 与 MIDI 文件变成音色库音频。render 桥是调用方自己的实现：`unlit3dx_examples` crate 在
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
  与独占线程的 world 相同的协议，只是没有线程。
- **独占线程的 world（threaded world）** 是属于自己的线程的 `World`。物理、音频与玩法
  各运行一个。

world 之间从不互相调用。调用者发送一个 **job**——一个接收目标 world 的
`&mut World` 的装箱闭包——并通过 **订阅** 接收：状态用快照，事件用流。
这就是全部协议。

## Job

`Job` 是装箱的 `FnOnce(&mut World)`。`WorldHandle::sender` 把一个 job 排入
队列且从不阻塞：队列满了返回 `Err(SendError::Full)`，world 已停返回
`Err(SendError::Closed)`，因此一个向自己的队列发送的 world 线程不会与自己死锁。
world 的线程在每次 tick 之前先清空队列，因此 job 永不与 tick 竞争。

job 也是世界改变另一个世界的唯一方式，而且它不回答任何东西：需要汇报什么的
world 改为通过订阅发布。回复会让发送方等待一个可能正忙、已停止或已经消失的
world，而回来的东西是状态，状态有自己的通道。

只有一个例外值得单独命名。想要某个 job 产出的值的调用方——某次构建生成的实体、
某次安装返回的句柄——用 `Pending` 请求它：job 带着一个 key 发出，答案在调用方下一个
tick 上按这个 key 回来。没有人等待；world 从不回答的请求会被判过期，而不是无限等下去。

框架创建的每个 world 都以组件形式携带一个任务集，因此任务与 world 同寿、属于
world 而不属于发起它的那次调用；谁驱动 world，谁就每回合泵它一次——线程在 tick
之前，宿主在分发之前。`LocalTasks::pump` 把集合里每个任务轮到无法再推进就返回、
从不阻塞，因此固定步长的 tick 或帧行为都能持有这个 await。GPU 桥就是这么驱动
设备初始化的。

## 快照与流

接收就是订阅：`JobSender::watch` 在目标 world 里装一个收集闭包并交回一个
reader。驱动该 world 的一方——它的线程、步进它的宿主、或排空它的宿主——在每一轮
tick 跑完后代为发布，因此 tick 体自己什么也不发布，也不可能发布两次。收集闭包填充
哪种形状由调用方选择：

- `watch` 返回 `SnapshotReader<T>`：状态，只关心最新的值。收集闭包返回
  `Option<T>`。
- `subscribe` 返回 `StreamReader<T>`：事件，每一个值都重要。收集闭包向
  交给它的缓冲区追加，每个值都会进入每个 reader 的队列。

用 `Pending` 请求是第三种形状：一次请求一个 job、一个 job 一个答案，只发布一次，
并由请求时的 key 指名。world 从不回答的请求——或 world 已经结束的请求——会被判过期，
而不是悬着不管。

该用哪一个，取决于调用方要的答案：

- 只关心**最新**的值——移动过的刚体的位姿、播放头的位置、world 对自己状态的
  报告——用 `watch`。
- 每一个值都按顺序要——刚生成的一个刚体、渲染侧回收的一个实例、结束的一个音符——用
  `subscribe`。
- 一次 job、一个值——某次安装创建的实体、某次构建产出的资源——用 `Pending` 请求。

载荷是调用方自己的类型——本库不定义任何载荷——所以场景只收集它真正要的数据，
不为别的付出代价。

快照 reader 是一个单槽信箱：`latest` 窥视最新一份，没有积压，也没有需要推演的
顺序，未被读取的那份直接被替换。槽是 `ArcSwapOption`，因此双方都不必去抢对方持有
的锁：world 把最新一份换进去，已经持有旧快照的 reader 可以一直把那一份读完。按 tick
收集的世界与按帧读取的消费者在中间会合，谁也不阻塞谁。收集闭包返回 `None` 时保留
最新一份快照，状态未变的世界因此保持安静。还没有任何 reader 时发布的值同样会保留，
后来才订阅的 reader 仍能看到它，而不必等下一次发布。

流 reader 持有自己的队列：每 tick `pump` 一次，把期间到达的值折成这一 tick 的批次，
`with_batch` 读取它。tick 比生产者慢的 reader 仍会按顺序看到每一个事件，而且一个
reader 的节奏拖不垮另一个。碰撞、序列结束需要的就是这个；输入也是这样到达 world 的：
`InputWriter` 就是生产者不是 world 而是窗口的一条流。

不订阅就什么都不收集，而订阅的寿命与 reader 相同：最后一个 reader 克隆被丢弃后，
收集闭包会在下一 tick 被清除。订阅本身是一个 job，因此队列已满或 world 已停止时，
它按队列自身的错误被拒绝。

## 线程与步进

world 通过一个 *runner* 生成，由它决定谁来调用 tick 循环。`WorldRunner` 就是把这个
选择做成泛型，而它是两种模式之间唯一的差别：有界 job 队列、订阅、宿主上的代理
实体、生成返回的句柄全都一样，因此宿主的实体与组件不随 runner 改变，tick 体内也
无需知道自己在哪种模式下。

- **`Threaded`** 给 world 一条自己的线程。`WorldThread::spawn_with` 持有这个循环：
  它先运行一次性的 `init` 闭包（在第一次清空 job 队列之前），再检查
  `ThreadControl::stop_requested`，在 `catch_unwind` 里运行 tick，并在线程结束前
  报告 panic，于是宿主知道自己的 world 为何而死，而不是 join 到一具尸体。销毁代理
  实体会请求停止其线程并至多等待两秒；拒绝停止的 tick 会被遗弃（detach），而不是把
  销毁方挂住。句柄负责汇报 world 的状态——`is_finished`，以及 `state`（一个
  `SnapshotReader<WorldState>`，读到 `Running`、`Stopped` 或
  `Panicked(message)`）——`stop` 则请求它结束；`join_timeout` 与 `detach` 在
  `WorldThread` 本身之上，留给直接持有它的调用方。
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

两种模式用同一条规则把实测的间隔换成整数步：`fixed_steps` 把一次 tick 没花掉的
零头记到下一次，超过 `MAX_CATCH_UP_STEPS`（四步）的部分则直接丢弃，于是停摆的宿主
是恢复运行而不是重放停摆。tick 以秒为单位拿到这个间隔——`spawn_threaded_world`
及其 `_with_init` 版本把它作为 tick 的最后一个参数传入——因此 tick 体不必假设
自己是以请求的速率被唤醒的。

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

- **physics**（默认开启）：`spawn_physics_world_with::<R>` 以固定步率（`PHYSICS_HZ`，60 Hz）
  驱动 rapier 的 `PhysicsWorld`。自驱动的 world 按实际流逝的时间追赶，一次 tick 至多四步；
  由宿主步进的 world 每被步进一次就走一步。每一步把 rapier 的碰撞事件排空进 `PhysicsContacts`
  组件——这一步开始的碰撞对（含每对相互接近的速度）与结束的碰撞对。它自己不发布
  任何载荷：调用方用收集闭包订阅，由闭包指名要什么状态，再由驱动该 world 的一方发布。
  `spawn_body`（携带 `BodyShape` 的 `BodyDesc`）与 `spawn_fixed_cuboid` 是基本构件；
  `init_physics_world` 为自行用 `step_once` 内联步进的调用方把同样的 world 作为
  `Physics` bundle 生成。
- **audio**（默认开启）：`spawn_audio_world_with::<R>` 打开输出设备并在音频线程上
  持有其流。它的 `OnAudioFill` 组件就是音源，一个实体可以在一个组件里叠加多层，
  每层带着自己的 `AudioFillOrder`：
  `dispatch_audio_fill` 按该顺序每块运行每个回调，把各自叠加的样本混进由 `AudioConfig`
  定长的环形缓冲，并按 world 的 `GlobalAudioGain` 缩放混音；
  设备回调消费这个缓冲，欠载时静音。没有任何音源的 world 不出声。
  玩法从不直接触碰音频——它发送 job。world 在流打开的那一刻生成 `AudioFormat`
  组件——按固定速率渲染的音源必须按设备实际采用的速率构建，而这一点在流打开之前
  无从知晓——并同时生成 `AudioOutputOpen` 标记，调用方由此区分有地方播放的 world
  与仍在等设备的 world，并丢弃过期的音符而非把它们排在音色库加载之后一次性齐放。
  因此安装音源就是这个 world 的普通工作：订阅格式，再把生成音源的 job 排进队列。

  步进的音频 world 完全不打开设备：没有可播放的目标，也没有回调来消费环形缓冲。它
  也不发布格式，因此想要出声的调用方自行提前安装音源，并在需要一块时自己调用
  `dispatch_audio_fill`——示例的离屏运行正是这样把混音写成文件的。`dispatch_audio_fill` 就是
  音频线程每块的全部工作，所以直接步进它得到的是同一套混音，只是少了环形缓冲。

- **synth**（默认开启，蕴含 audio）：安装所需的构件，构建在
  [rustysynth-ext](https://docs.rs/rustysynth-ext) 之上——`load_soundfont` 解析字节
  （通常是 `include_bytes!`，不经过文件系统），`SynthVoice::from_font` 生成一台音色，带着
  渲染它的 `OnAudioFill` 音源。合成器在第一次填充时按那次填充给出的采样率建立，因此音色
  可以在输出设备还没报出采样率之前就生成；在那之前发出的消息会留在音色里，由第一次填充补放。
  `midi_job` 把一条 `MidiEventKind` 发给它指定的音色，可以带一段以秒计的延迟。音色就是音频 world
  在合成方面的全部接口。

  序列播放归玩法 world，时钟也在那里：`MidiSequencer` 持有一个文件、已走到的时刻、下一条事件的
  下标与循环点，`MidiSequencer::advance` 把这一帧到期的事件作为 `midi_job` 发给某台音色。
  播放、暂停、回卷、循环都是这个组件的字段，就在它所在的地方读写——面板与它的传送之间没有 job。
  音色是共享的，因此任何打断一趟播放的动作——暂停、回卷、绕回循环点——都会松开那一趟留下
  还在响的音，而不是让它们响进下一趟；而且只松开这些音：序列记着自己按下的音，逐个按名字松开，
  绝不用一次清整个通道的全音关闭，那会把别的序列的乐声也切掉。
  文件来自 `load_midi_file`（SMF 文件，循环约定由 `load_midi_file_with_loop_type` 指定）或
  `midi_file`（定时的 `MidiEventKind`，在这里排序，故调用方不必先排好）。文件自带的循环标记
  会设定循环点；文件从未标记过的段落，也可以直接写 `loop_point` 让它重复。示例里的
  `install_synth` 把整个安装收进一个函数，作为 job 排给音频 world，并回传音色的实体。
- **ui**（随 `render` 开启）：把 `ui::UiPanelHost` 生成在面板旁边，它把一个 world 的
  `UiPanel` 布局给另一个 world 里的 `UiSource` 绘制。面板只能在它自己的 egui context
  所运行的地方构建——回调拿到的是正在构建它的那一趟 pass 的 `&mut egui::Ui`——所以面板
  所在的 world 负责布局并三角化，绘制它的 world 负责上传与记录回来的结果。它持有通向绘制
  world 的 sender，以及绘制侧报回的屏幕尺寸所经的 `unlit3d::relay::SnapshotReader`；绘制
  侧的 source 用 `unlit3d::ui::UiSource::publish_target` 接收目标，槽位由它自己持有。帧以
  job 的形式过去，槽位因此留在绘制它的那条线程上；绘制侧量出的目标反向以快照回来，因为
  布局永远只需要最新的那一个。无事可画的一 tick——面板静止、没有输入、目标未变、没有到期
  的重绘——什么都不发，所以静止的 UI 零开销；已有的那一帧会一直被绘制，直到更新的一帧
  取代它。绘制 world 的队列拒收的帧会再次发送，而不是被当作已送达，因此队列满只会推迟
  画面，不会丢帧。两侧保持独立的 context：共用会让两个 world 同时写同一份 memory 与字体
  图集，而 egui 把一次 `run_ui` 当作一个完整帧。两侧的纹理 id 都从零开始分配，所以帧保留
  它被布局时拿到的 id，GPU 集成按来源与 id 建键——不重编号任何东西。
- **mcp**（默认关闭）：`serve_mcp` 把宿主 world 及其携带的每个 world——嵌套的
  子 world，或自有一条线程的 world——通过
  [模型上下文协议](../unlit_ecs/README.zh-CN.md)对外服务，场景
  运行时即可从终端检视与截图。每次生成都会在代理实体上、紧挨着代理留下一个
  `WorldSender` 与一个 `WorldName`：前者让传输层能把命令投进一个它借不到的
  world，后者就是 `list_worlds` 报告的名字。录音工具——
  `start_audio_recording`、`stop_audio_recording`、`read_audio_recording`、
  `cancel_audio_recording` 与 `audio_recording_status`——在指名的 world 上驱动一次录音，
  并把混音写成 WAV 文件或直接在内存中返回，测试因此无需落盘就能断言样本。`request_exit`
  请求携带 `ExitRequest` 的宿主退出；没有窗口事件循环时，`pump_mcp` 负责分发命令，
  这也正是没有 winit 的宿主仍能对外服务 MCP 的原因。录音音源挂在填充顺序的最末，
  因此它捕获到的是所有其他音源与 world 的 `GlobalAudioGain` 之后的混音，
  样本写成一个 RIFF/WAVE 文件，无需额外依赖。录音器只为这些工具编译，
  故 `mcp` 自身蕴含 `audio`。

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
   请求；刚体通过 physics 队列生成与销毁；音符作为 job 送往音频 world，而一整套
   安装——见示例的 `install_synth`——就是一个函数包成一个 job。快照信箱承载的是从
   world 里**取出**的状态，读它的一方决定这意味着什么；而另一个 world 的内容**变成**
   什么仍然是 job，因此玩法从物理快照推出的姿态是发给 render world 的 job，而不是留在
   信箱里等它自己去应用。

检验标准是一行搭建代码能否属于另一个场景：若它提到了游戏称之为某个东西的东西，
它就该是一个 job。
3. **场景为每个要对话的 world 持有一个端口。** world 的地址就是一个 `JobSender`
   加上它建立的那些订阅，于是场景把它们收进自己的一个值里——示例的 `Scene` 带着
   render、physics、audio 三个 sender 与物理快照——并把它交给需要触达 world 的一切：
   tick 体、生成函数、控制面板。原本每个对象都要为每个通道留一个字段，现在只是一个参
   数；从不向音频 world 发送的对象也就不会持有它的 sender。端口留在调用方的 crate
   里：运行时没有场景要描述，而调用方自己的桥也得到同样的对待。

## 应用骨架

多 world 应用面向窗口的那一半同样与应用无关，所以随运行时一起提供。**gpu**
模块把 `Gpu` 请求做成被请求搭建的那个 world 自己的任务，由每帧开头的那次泵送
驱动，答案落地后把 `ready` 作为排队的 command 运行——什么都不阻塞，wasm 安全：

```rust,ignore
world.root_scope_mut().spawn(bundle![request_gpu(app, |world, gpu| {
    // ... 在请求发起处搭建场景 ...
})]);
```

`Gpu::headless` 是同一个请求的免窗口版本，供测试与工具使用。**present** 模块
持有从子 world 到窗口的全部管线：`setup_presenter` 默认让场景自己的 pass 直接
画进交换链；打开离屏路径后，在同一个 world 里再加一个 pass，把常驻目标拷到表面
——这让帧存活得足够久、可以读回，截图与快照测试就走这条路。`draw_presented` 是每帧排空
子 world、分发帧并呈现的行为，`pump_presented` 在窗口
存活期间让帧持续到来，`resize_presented` 跟随窗口变化。绘制 world 所需的 render
桥——资源服务器及其周围的网格与相机辅助——由调用方自己构建；`unlit3dx_examples`
自带一份，位于 `src/render.rs`。

## 测试

集成测试是一个二进制，它要求 `audio`、`physics` 与 `synth`。用
`--no-default-features` 跑不会失败，而是让 suite 不进入这一趟：required features
不满足的 target 会被 Cargo 整体跳过。（没有用例用到 GPU，因此不要求 `render`。）

`cargo nextest run -p unlit3dx` 覆盖核心：job 及其队列、订阅——多个 reader 各自
拿到自己的快照、收集闭包返回空时保留最新一份、reader 被丢弃后订阅被清除、队列满时
订阅被拒绝——线程停止与 panic 报告、独占线程的 world 的已发送 job 运行，以及步进 world 的
同一批行为——它的 job 在步进时运行、走完终点的 tick 会结束它、步进一个不携带步进
world 的实体什么也不做。集成测试补上各桥：物理 world
把球丢到地板上并报告接触、订阅者把两次分别落地的碰撞都排进队列（信箱会把它们并成
一次）、音频 world 运行 job 并报告输出已打开、多 world 的 job 与
快照往来，以及一个 panic 的 job 结束它的 world、排在它后面的 job 不再运行。录音也有覆盖：它捕获设备本会播放的
混音并把样本排成 RIFF/WAVE 文件，音频 MCP 工具则把两者串进下面这套分发覆盖。打开 `mcp` feature 后，MCP 命令分发也有覆盖：
world 列表、路由到嵌套 world、路由到自有一条线程或由宿主步进的 world，
以及指向不带 world 的实体、已经停止的 world、拒绝该 job 的队列时给出可读的失败；
音频录音工具同样有覆盖，从开始录音、拒绝第二次录音，到把捕获的混音写出去。
