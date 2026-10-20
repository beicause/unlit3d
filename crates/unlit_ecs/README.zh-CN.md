[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md) | 简体中文

# unlit_ecs

一个紧凑的 archetype ECS。组件按 archetype 存放，每种类型一列，而每个组件值都活在
自己的 cell 里——正因如此，一个共享的 `&World` 就能在不使用任何 `unsafe` 的前提下
读取*并*写入组件。

设计刻意保持精简：没有变化检测、没有事件与观察者、没有调度器。凡是需要其中之一的场景，都由调用者在实体本身上自行完成。除一个哈希器
外本 crate 没有其他依赖，也不依赖工作区中的任何其他 crate，因此可以单独使用；它对
渲染一无所知。唯一的例外是可选的 `reflect` feature，它为[世界信息](crate::WorldInfo)
结构体派生 [facet](https://docs.rs/facet)，好让桥接层直接序列化它们，并导出实体字段所经由的
`EntityProxy`。类型为实体的反射字段指名该 proxy，于是 ECS 自己的结构体与任何调用者的组件
都用同一种方式书写实体——即它的 [`Entity::to_bits`] 整数。ECS 本身从不读取这份反射。

本 crate 处于**极早期开发阶段**，API 会自由变动。

## 模型

- **实体的组件集合在 spawn 时就固定了。** 组件只能读写，不能增删；要改变集合，
  必须 despawn 该实体再重新 spawn。不可变的原型正是让存储可预测、并消除其他 ECS
  设计中容易泄漏的「组件被移除」记账工作的原因。
- **读写只需要一个 scope。** `&World` 可以打开根 scope，它看得见一切；一个场景的 `Scope`
  只看得见该场景的子树，看不见旁边的东西。结构变更——spawn 与 despawn——需要
  `ScopeMut`，或者由驱动器应用的 `Commands` 队列。
- **world 是 `!Send` 的。** 组件存在 `RefCell` 里，因此 world 留在创建它的线程——
  渲染器和其他与线程绑定的状态属于这里。并行是调用者的事，办法是把工作切分到纯数据上，
  而不是共享 world。
- **没有资源（resource）。** crate 不标记也不追踪任何资源；想被随处访问的实体就是
  普通实体，调用者自行保存它的句柄，需要时可以用自己的标记组件加以标记。
- **任何 `'static` 类型都是组件。** 不需要 derive，也不需要注册；元组同样是组件。
  正因如此，*bundle* 是「组件的元组」，而不是组件的层级结构——嵌套元组是一个组件，
  而不是一组组件。要展平嵌套，就用 `bundle!` 写 bundle，它在展开期遍历语法。
- **没有系统（system）。** 驱动行为意味着调用者自己读取 world 并调用它想要的方法或
  闭包——或者运行一个*行为组件*，即持有闭包、由调用者自写的驱动器调用的组件。需要
  等待的行为返回 future，由调用者决定何时 poll；库内不内置执行器。
- **唯一的关系是 scope。** 每个实体都带一个指向父实体的 `ScopeOf`，正是它让一个场景
  成为一棵子树：场景的 scope 看得见它和它的后代，别人看不见——兄弟的子树在两个方向上
  都够不着，所以并排的两个场景彼此毫无交集。任何 scope 也看得见 `World::shared` 的
  子树，场景被换掉时，每个场景共用的资源正是靠它留着。这是 world 唯一维护的
  关系，因为每次访问都要判定可见性，而由调用者维护的关系做不到这一点。实体之间的其他
  引用仍然是组件里的 `Entity`，由调用者自行维护。
- **借用冲突是 panic，而不是编译错误。** 请求一个已被借用的组件，或在同一个查询里两次
  以 `&mut` 取同一组件，都会报出组件名并终止。这是不做访问冲突分析的代价。可选组件只在
  「组件不存在」这件事上可选：借不到时同样 panic，而不会表现为 `None`。
- **有两个实体是脚手架。** `World::new` 创建 `World::root`——它的 `ScopeOf` 指向自己，
  根 scope 打开的就是它——和 `World::shared`，后者的子树每个 scope 都看得见。两者都能
  携带组件、都能被当作 scope 打开；但它们都不被计数、不被查询、不可 despawn、不可重挂。

## scope

scope 就是一个实体。每个实体都带一个指向父实体的 `ScopeOf`，这一条关系就是整棵树：
一个场景是一棵子树，而调用者手里拿到的是 `Scope`，一个收窄到树中某个实体位置的访问器。

```rust
use unlit_ecs::World;

let mut world = World::new();
let scene = world.root_scope_mut().spawn((),);
let cube = world.scope_mut(scene).unwrap().spawn((0u32,));
let camera = world.scope_mut(scene).unwrap().spawn((0f32,));

// 场景的 scope 看得见场景及其下的一切，看不见旁边的东西。
let scope = world.scope(scene).unwrap();
assert_eq!(scope.entity(), scene);
assert_eq!(scope.len(), 3);
assert!(scope.contains(cube) && scope.contains(camera));
assert_eq!(scope.children(scene).len(), 2);
assert!(scope.query::<&u32>().any(|(entity, _)| entity == cube));

// 根也是一个 scope，它看得见所有 scope。
assert_eq!(world.root_scope().len(), 3);
```

有两个实体是脚手架而不是内容。`World::root` 是根 scope 所打开的 scope，它的 `ScopeOf`
指向自己；`World::shared` 是它的子实体，每个 scope 都看得见那棵子树。两者都能携带组件、
都能被当作 scope 指名，所以资源挂在哪一个上都行——但它们都不被计数、不被查询、不可
despawn、不可重挂，正因如此空 world 计数是零而不是二。

**scope 看得见什么。** 当 `candidate` 存活，且满足下列之一时，`caller` 的 scope 看得见它：

| candidate 位于 | 谁能看见 |
| --- | --- |
| shared 子树 | 每个 scope |
| 任意位置 | 根 scope |
| scope 自身的实体 | 该 scope |
| scope 实体之下 | 该 scope |

其余一切都够不着：scope 看不见自己的祖先，也看不见兄弟及其子树。这一关系刻意不对称
——场景的 scope 看得见共享资源，共享资源的 scope 却回不到场景里——所以「A 看得见 B」从
不意味着「B 看得见 A」。不存活的句柄哪里都看不见，根也一样：看得见某个位置，不等于
那里真有东西。

**scope 能改动什么。** 所有权比可见性更严，且只覆盖结构变更：当实体就是 scope 自身
或在其下，或者 scope 是根时，scope 才能 despawn 或重挂它。shared 子树处处可读，但只有
根能搬动或删除其中的内容；shared 实体本身是根的子实体，不属于任何 scope，所以哪个
scope 都不能 despawn 或重挂它，根也不行。打开嵌套 scope 又是一次可见性判定，所以场景
scope 能打开 shared 的 scope 并在那里 spawn 资源；之后 despawn 那份资源是根的事，不是
场景的事。

**没有向上的路。** 对不是 scope 的实体，`World::scope` 回答 `None`；`Scope::scope`
只能向下——进入看得见的子实体，或进入 shared 子树。scope 说不出自己来自哪个 world，
所以拿着一个 scope 并不是通往 world 其余部分的途径。

**改变树。** `ScopeMut::spawn` 把新实体放进 scope 自身，`spawn_at` 则把它放到
`World::reserve_entity` 预留的句柄上。`ScopeMut::despawn` 删除该实体及其整棵子树，
子先于父，因此子实体的 `OnDespawn` 仍然看得见活着的祖先。`ScopeMut::link` 与
`unlink` 在同一个 scope 内重挂实体，拒绝把 scope 搬进自己的后代，也拒绝碰脚手架；
它们是纯结构变更，不跑任何生命周期回调。只读 scope 通过 `Scope::queue` 排队做同样的
变更，再由驱动器调用 `World::apply` 落盘。

**回调落在哪里。** 生命周期回调拿到的是该实体父实体的 scope，所以场景子实体上的
`OnSpawn` 看得见场景。它同样不能改结构——它也排队，和只读 scope 一样。运行中注册的
回调排到之后那次运行，位于原有回调之后；某个回调 panic 时，排在它后面的回调仍然保留。

**开销。** 每次访问都要沿父链走一遍来判定可见性，走查以节点数为上界，所以畸形的树也
不会把它拖死。`World::children` 与 `World::is_descendant` 背后的索引由 world 与
`ScopeOf` 组件一并写入，两者不可能互相矛盾。这个组件属于 world：字段私有、bundle
不能携带它，通过反射注册表写 `ScopeOf` 也会被拒绝，因为搬动实体是 `ScopeMut::link`
的职责。查询与计数会跳过 scope 看不见的行；`Scope::archetype` 对空 archetype 和只
看得见一部分的 archetype 都回答 `None`，`Scope::archetypes` 只给出非空且完整可见的
archetype，`Scope::archetype_infos` 则报告各自的可见长度。

## 内容概览

`World`、`Entity`、`Location`、`Bundle` / `ArchetypeBuilder`（以及用于展平嵌套元组的
`bundle!` 宏）、把 world 收窄到一个 scope 的 `ScopeOf` 与
`Scope` / `ScopeMut`、spawn 与 despawn 所跑的回调 `OnSpawn` / `OnDespawn`、
`Query` 与 `QueryFilter`（`With`、`Without`、`Or`、元组）、用于延迟结构变更的
`Command` / `Commands`、用于直接检视存储的 `Archetype` / `Archetypes`、用于给
只知 `TypeId` 的擦除类型读取者命名组件类型的 `World::type_name`、用于把 world
形态当作纯数据汇报的 `WorldInfo` / `ArchetypeInfo` / `EntityInfo`（实体字段反射时
指名 `EntityProxy` 与 `EntityVecProxy`），以及
专用哈希容器 `TypeIdHashMap`、`EntityHashMap` 等。

查询会按 archetype 解析一次它需要的列，再通过该状态逐行取值，因此其开销与
*组件集合*的数量成正比，而不是与实体数量成正比。`Scope::location` 与
`Scope::archetype` 把同样的形态开放给持有大量实体的调用者——比如把场景剔除成
一个列表的调用者——让它能按 archetype 分组，一次性解析每个 archetype 的列，
而不必逐实体重新查找。

`Commands` 队列之所以存在，是因为结构变更需要 `&mut World`：只持有共享 world 的
回调改为把 spawn 或 despawn 排入队列，由驱动器调用 `World::apply` 落盘。
`Commands::spawn` 会立即预留一个 `Entity`，因此该句柄在队列被应用之前就可用了
——可以存进组件里，也可以传给另一个回调。

遍历是确定性的：archetype 按创建顺序访问，行按存储顺序访问。archetype 按需创建，
永不删除。

## 用法

```rust
use unlit_ecs::{Query, Without, World};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = World::new();
let cube = world.root_scope_mut().spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
let clock = world.root_scope_mut().spawn((0.016f32,));

// Drive every `Spin`. The caller picks which entities to touch and in what
// order; the library has no built-in notion of a scene graph.
let delta = world.root_scope().get::<f32>(clock).unwrap().to_owned();
for (_, mut spin) in world.root_scope().query::<&mut Spin>() {
    spin.angle += spin.radians_per_second * delta;
}
assert_ne!(world.root_scope().get::<Spin>(cube).unwrap().angle, 0.0);

// Filters narrow the archetypes a query visits, without fetching anything.
let _ = world
    .query_filtered::<&Spin, Without<f32>>()
    .map(|(_, spin)| spin.angle)
    .count();
```

spawn 与 despawn 需要 `&mut World`，或者排入队列的命令：

```rust
use unlit_ecs::World;

let mut world = World::new();
let entity = {
    let commands = world.root_scope().queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.root_scope().contains(entity));
world.apply();
assert!(world.root_scope().contains(entity));
```

元组 bundle 不会展平，因为元组本身就是一个组件。当组件是嵌套写下的，或者超过
十六个时，用 `bundle!`。它的宽度受编译器递归上限约束——默认上限下约一百个组件；
更宽的需要在调用方 crate 里提高 `#![recursion_limit]`：

```rust
use unlit_ecs::{bundle, World};

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle!((1u32, 2.0f32), (true, ())));

// 宏展平了嵌套：得到三个组件，而不是一个嵌套元组。
assert!(world.root_scope().has::<u32>(entity));
assert!(world.root_scope().has::<f32>(entity));
assert!(world.root_scope().has::<bool>(entity));
assert!(!world.root_scope().has::<(u32, f32)>(entity));
```

用 `..value` 写下的 bundle 会被展开成它自己的组件，于是别处建好的 bundle 可以原样
并入这次 spawn：宏产出的 builder 贡献它展平后的叶子，而元组或自定义 `Bundle` 保持
它自己定义的组件。之所以需要这个语法，是因为宏只遍历源码，分不清 bundle 和组件——
裸写的话 bundle 会被当成一个组件：

```rust
use unlit_ecs::{ArchetypeBuilder, bundle, World};

let mut gathered = ArchetypeBuilder::new();
gathered.push_component(1u32);

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle!(..gathered, 2.0f32));

assert!(world.root_scope().has::<u32>(entity));
assert!(world.root_scope().has::<f32>(entity));
```

## MCP 服务器

`mcp` feature（默认关闭，仅原生目标）把本 crate 变成一个经 stdio 由客户端驱动的
[模型上下文协议](https://modelcontextprotocol.io)服务器。它放在这里，是因为协议的
传输侧必须指名一个 `World` 并把工作交给它：`McpCommand` 携带着一个 job，由拥有该
world 的线程运行。桥接层不在这个边界上借用任何东西，也没有别的 crate 拥有传输所需的类型。

本 crate 依旧保持「只有一个哈希器依赖」的形态：该模块在 `wasm32` 上被关掉（协议的
传输在该目标上无法构建），它新增的每个依赖都是可选且按目标门控的，因此浏览器构建
完全不带上它们。

工具来自定义其作用对象的各个 crate，而不是来自本 crate。crate 把一个 `ToolSource`
提交到链接期表——与 `reflect` 注册表所用的同一套 `inventory` 机制——`McpServer::new`
遍历该表逐个注册：

```rust,ignore
unlit_ecs::mcp::inventory::submit! {
    ToolSource { extend: register }
}

fn register(router: &mut ToolRouter<McpServer>) {
    router.add_route(tool("my_tool", "What it does.", my_tool));
}
```

`unlit_ecs` 自己注册实体与组件工具——`world_summary`、`list_components`、
`list_archetypes`、`list_entities`、`get_entity`、`get_component`、
`set_component`、`spawn_entity`、`despawn_entity`——它们只需要 ECS；
`unlit3d` 注册渲染类工具，`unlit3dx` 注册音频类工具。这些 crate 都不是本 crate 的
依赖，而宿主建起的服务器会带上它们全部。

每个实体工具都接受一个可选的 `scope` 参数，指名要操作哪个 scope，默认为根。
指名了某个场景的客户端只看得到该场景的子树，于是一个 MCP 会话就能检视 world 的私有
部分，而不必由宿主把它们暴露出来。scope 不拥有的操作——在它子树之外 spawn、despawn
它的祖先——会被拒绝，而不是被悄悄改道。

组件经由 `reflect` 注册表按名字寻址：组件的 crate 把一个 `ComponentEntry` 提交到
同一种链接期表，于是 `get_component` 与 `set_component` 能编解码一个 ECS 自己
从未听说过的值。

## MCP 服务器够不到什么

传输线程不是 world 的线程。它读取协议，把一次调用变成一个 `McpCommand`，再经一条
宿主负责排空的 channel 投递；拥有该 world 的一方运行 job，并在命令自带的 channel 上作答。
world 是 `!Send` 的，正是这一形状让它保持如此。

工具指名哪个 world 由客户端决定：`WorldId` 为 `"Host"` 时指服务器建立时所围绕的
world，为 `{"Handle": bits}` 时指某个宿主实体所携带的 world。想让其余 world 也可被
寻址的宿主自行回答 `list_worlds`，因为只有它知道哪些实体携带着 world。

## 为什么 ECS 这么小

<details>
<summary>这些省略是有意为之的，每一条都有替代方案</summary>

- **没有变化检测。** 在 OOP 中对象自身状态在内部维护；追踪什么变了是调用者的事。
- **只有一个关系，即 scope，别无其他。** `Children`、`ChildOf` 这类组件是调用者
  自己维护的引用；scope 是唯一不能那样维护的关系，因为每次访问都要判定可见性。
  其他引用仍然是组件里的普通实体。
- **生命周期回调挂在实体上，而不是 world 上。** `OnSpawn` 与 `OnDespawn` 就是
  持有回调的普通组件，因此没有生命周期组件的 world 不为该特性付出任何代价，调用者
  也可以自己写一个驱动。
- **没有事件和观察者。** 作为替代，可以直接调用组件的方法或使用行为组件。*行为组件*
  指包含函数指针或闭包的组件，并且该函数中能运行对 world 的访问，就像系统或观察者
  一样，其中的函数也可以是 async 的；一个组件可以持有多个这样的函数，并按顺序运行。于是类似 Godot 节点的帧更新、固定间隔更新、
  按键鼠标触摸输入等回调都可以视作实体上的行为组件，由外部调用者访问 world 调用
  它们——由此通过组合的方式实现此类功能。
- **没有资源。** 想被随处访问的实体就是普通实体，调用者自行保存其 `Entity` 句柄；
  资源引用就是实体引用，库不提供任何专用标记或机制。
- **实体的原型不可变。** 在 OOP 中类/对象的状态和行为是不可变的，不可变的原型也使得
  状态保留有强制性，避免变化检测设计的 `RemovedComponents` 和组件泄漏的坑。
- **没有系统**（借鉴自 [`hecs`](https://docs.rs/hecs)）。系统只是外部调用者对世界的
  访问，或是包含行为（闭包函数）的组件。因此不用做复杂的系统并行性（依赖、访问冲突
  等）分析、多线程调度器：让外部调用者自行决定运行的线程。
- **world 是 `!Send` 的。** GPU 资源和渲染器是 `!Send` 组件，在 `wasm32` 上即使开启
  特性开关也不是 `Send`，所以一个能跨线程的 world 在那里毫无收益。`Send` 世界曾存在过
  又被删除：world 所共享的存储，并不是让它上面的工作可切分的东西。
- **没有用到的功能绝不添加。**

</details>

## 测试

```text
cargo nextest run -p unlit_ecs   # 本 crate 的单元与集成测试
cargo xtask test                 # 整个工作区
```

测试覆盖 world 与查询行为、延迟命令。测试各层作为整体，以及 CI 所跑的内容，见
[根 README](https://github.com/beicause/unlit3d/blob/main/README.zh-CN.md#测试与基准)。

本 crate 的访问路径由 [`unlit3d_benchmarks`](../../unlit3d_benchmarks/README.zh-CN.md)
的 `ecs` 目标做基准测试，`World::get` 与「一次性解析整个 archetype 的列」两种取法的
成本对比就是在那里测的：

```text
cargo bench -p unlit3d_benchmarks --bench ecs
```

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
