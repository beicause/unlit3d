[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md) | 简体中文

# unlit_ecs

一个紧凑的 archetype ECS。组件按 archetype 存放，每种类型一列，而每个组件值都住在
自己的 `UnsafeCell` 里——正因如此，一个共享的 `&World` 既能读取也能写入组件。
那些格子本会容许的别名由借用账本抓住：访问路径在交出任何引用之前先认领它要触及的
组件，冲突时的 panic 指名双方。

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
- **world 是 `!Send` 的。** 组件存在 `UnsafeCell` 里，共享的 `&World` 才能触及它们，
  因此 world 留在创建它的线程——渲染器和其他与线程绑定的状态属于这里。并行是调用者的事，办法是把工作切分到纯数据上，
  而不是共享 world。
- **没有资源（resource）。** crate 不标记也不追踪任何资源；想被随处访问的实体就是
  普通实体，调用者自行保存它的句柄，需要时可以用自己的标记组件加以标记。
- **任何 `'static` 类型都是组件。** 不需要 derive，也不需要注册；元组同样是组件。
  因此 *bundle* 不是「组件的元组」，而是 `bundle!` 写出的、或 `DynamicBundle`
  收集的那份组件清单；直接交给 spawn 的 `(a, b)` 是一个组件，即那个二元组。宏在展开期
  遍历语法，所以 `bundle![a, b]` 是两个组件，而带括号的项是一个组件，元组也不例外。
- **没有系统（system）。** 驱动行为意味着调用者自己读取 world 并调用它想要的方法或
  闭包——或者运行一个*行为组件*，即持有闭包、由调用者自写的驱动器调用的组件。需要
  等待的行为返回 future，由调用者决定何时 poll；库内不内置执行器。
- **唯一的关系是 scope。** 每个实体都活在某个 scope 里，而 scope 本身也是实体，正是它
  让一个场景成为一棵子树：场景的 scope 看得见它和它的后代，别人看不见——兄弟的子树在
  两个方向上都够不着，所以并排的两个场景彼此毫无交集。任何 scope 也看得见 `World::shared` 的
  子树，场景被换掉时，每个场景共用的资源正是靠它留着。这是 world 唯一维护的
  关系，因为每次访问都要判定可见性，而由调用者维护的关系做不到这一点。实体之间的其他
  引用仍然是组件里的 `Entity`，由调用者自行维护。
- **借用冲突是 panic，而不是编译错误。** 查询在交出任何引用之前先认领它将触及的组件
  类型，认领活到持有者为止。两个持有者指名同一个组件、至少一方在写、且行可能重叠时
  冲突；panic 报出组件名、两个持有者与两个源码位置，所以它说的是谁正握着，而不只是
  谁被拒。这是不做访问冲突分析的代价。可选组件只在「组件不存在」这件事上可选：行已被
  独占认领时同样 panic，而不会表现为 `None`。
- **访问失败是 error，不是 `None`。** 每个触及实体的函数都回答 `Result`，出错时的
  `EntityAccessError` 会指名被问到的那个句柄并给出原因：实体已经没了、它在 scope 之外、
  它不匹配该查询、它未被预留，或者它不属于调用者去搬动。按 id 问 archetype 则回答
  `ArchetypeAccessError`。`Option` 只留在「缺席本身不算访问失败」的地方——查询项
  `Option<&C>` 对缺 `C` 的实体就是缺席。
- **有两个实体是脚手架。** `World::new` 创建 `World::root`——它就在自己的 scope 里，
  根 scope 打开的就是它——和 `World::shared`，后者的子树每个 scope 都看得见。两者都能
  携带组件、都能被当作 scope 打开；但它们都不被计数、不被查询、不可 despawn、不可重挂。
  它们占据所在 archetype 的最前两行，于是查询每个 archetype 只跨过一次，不必逐行判断身份。

## scope

scope 就是一个实体。每个实体都活在某个 scope 里，这一条关系就是整棵树：一个场景是一棵
子树，而调用者手里拿到的是 `Scope`，一个收窄到树中某个实体位置的访问器。

```rust
use unlit_ecs::{Entity, World, bundle};

let mut world = World::new();
let scene = world.root_scope_mut().spawn(bundle![]);
let cube = world.scope_mut(scene).unwrap().spawn(bundle![0u32]);
let camera = world.scope_mut(scene).unwrap().spawn(bundle![0f32]);

// 场景的 scope 看得见场景及其下的一切，看不见旁边的东西。
let scope = world.scope(scene).unwrap();
assert_eq!(scope.entity(), scene);
assert_eq!(scope.len(), 3);
assert!(scope.contains(cube) && scope.contains(camera));
assert_eq!(scope.children(scene).unwrap().len(), 2);
let mut seen = scope.query::<(&u32, Entity)>();
assert!(seen.iter().any(|(_, entity)| entity == cube));

// 根也是一个 scope，它看得见所有 scope。
assert_eq!(world.root_scope().len(), 3);
```

有两个实体是脚手架而不是内容。`World::root` 是根 scope 所打开的 scope，它就在自己的
scope 里；`World::shared` 是它的子实体，每个 scope 都看得见那棵子树。两者都能携带组件、
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

**没有向上的路。** 对不是 scope 的实体，`World::scope` 报错；`Scope::scope`
只能向下——进入看得见的子实体，或进入 shared 子树。scope 说不出自己来自哪个 world，
所以拿着一个 scope 并不是通往 world 其余部分的途径。

**改变树。** `ScopeMut::spawn` 把新实体放进 scope 自身，`spawn_at` 则把它放到
`World::reserve_entity` 预留的句柄上——已释放、已 spawn 过、或从未预留的句柄会得到
`EntityAccessError::NotReserved`。`ScopeMut::despawn` 删除该实体及其整棵子树，
子先于父，因此子实体的 `OnDespawn` 仍然看得见活着的祖先，并回答这次带走了多少实体。
`ScopeMut::link` 与
`unlink` 在同一个 scope 内重挂实体，拒绝把 scope 搬进自己的后代，也拒绝碰脚手架；
它们是纯结构变更，不跑任何生命周期回调。三者都回答 `Result`，失败时给出的错误指明
是哪一种原因：实体已不在、它在 scope 之外、它在 shared 子树里因而只有根能动、它是
脚手架、或者这次搬动会把树切成两半。于是手里握着别处句柄的调用者不必猜，可以直接去问
真正拥有它的那个 scope。只读 scope 通过 `Scope::queue` 排队做同样的
变更，再由驱动器调用 `World::apply` 落盘；后者报告第一个失败的命令，并把其余命令照常跑完。

`World::children` 与 `Scope::children` 对不活着的句柄报错，而不是给一个空列表——
空列表和没有子实体读起来是一样的。

**回调落在哪里。** 生命周期回调拿到的是该实体父实体的 scope，所以场景子实体上的
`OnSpawn` 看得见场景。它同样不能改结构——它也排队，和只读 scope 一样。回调在运行前被搬出这一行，运行后再放回，所以回调里访问自己的生命周期组件会读到那一行，
而不是撞上借用检查；回调 panic 时，它仍然留在当初 spawn 的那一行上。

**开销。** 查询的一行可见不可见，代价是一次数组读：archetype 在实体数组旁记下每一行的
scope，所以过滤不再走树。问两个实体是否相关仍要沿父链走，走查以节点数为上界，所以畸形的树
也不会把它拖死。这条关系只有 world 的树里
一份，而不放在组件里：可见性每次访问都要判定，而组件要被借用来回答它，于是独占借用
某个实体的组件就会让另一个实体的可见性检查 panic。只有一份也意味着没有东西需要对账：
只有 `ScopeMut::link` 与 `unlink` 会写这棵树，而它们需要 `&mut World`，所以经共享
world 拿到的借用永远不可能与它们相撞。从 `Scope` 读取时，每问一个实体就在树里查一次，
`Scope::entity_info` 把它报告为 `scope`。
查询与计数会跳过 scope 看不见的行；`Scope::archetype` 对空 archetype 和只
看得见一部分的 archetype 都报错，`Scope::archetypes` 只给出非空且完整可见的
archetype，`Scope::archetype_infos` 则报告各自的可见长度。`Scope::archetype_of` 是
`Scope::archetype` 的逐行版本：它只问实体所在的那一行，不做整原型判断，所以在
`Scope::archetype` 无法作答处作答——两个 scope 共用一个 archetype 时正是它派上用场
的场合。

**不经过 world 也能命名组件。** `Archetype` 自带其组件名字，所以持有它的读取者不必
回到 `World::type_name` 就能描述它。world 每个名字只存一份，不随实体数增长：
`World::archetypes` 直接交出这个池子本身，供想遍历它的调用者使用。

## 名字

实体带一个名字，`{:?}` 会把它印在标识句柄的 bits 旁边——
`Entity { name: "cube", bits: 3 }`——于是 panic 与日志里写的是
对象本身，而不是一个光秃秃的数字。名字来自生成它的 bundle，而 bundle 会给自己
起名：调用者自己的类型以自己的类型名作答，`bundle!` 则以宏所在的源码位置作答。
于是 spawn 一个对象，实体就以这个对象命名：

```rust
use unlit_ecs::{World, bundle};

let mut world = World::new();
let cube = world.root_scope_mut().spawn(bundle![0u32]);
assert!(!cube.name().is_empty(), "bundle 会给它建出的实体起名");
```

运行时拼出来的动态 bundle 没有这样的位置可言，所以由 `DynamicBundle::named`
自己给它一个标签，`spawn_named` 则把标签直接交给那次 spawn。
`World::reserve_entity` 的句柄在预留处就已命名，因为预留正是调用者说明这个实体
是什么的地方。

名字只是标签。它不参与比较、排序与哈希，所以指向同一个实体的两个句柄无论叫什么
都相等，而经过 `Entity::to_bits` 还原的句柄没有任何名字。名字也是跟着句柄而不是
跟着实体的：同一个实体从两处看可以印出两个不同的标签，带到另一个世界的句柄仍带着
它来自的那个世界的名字。

从裸 bits 还原的句柄没有名字可印，所以存储这个实体的那一行才是标签唯一长久的副本：
`Scope::entity_name` 按句柄把它读回来。`{:?}` 只印句柄的名字和 bits，不印构成它们的
索引与世代：那是分配器自己的事，而句柄过线到达调用者时，只剩 bits 这一份。
报错与引擎自己的消息都这样印。列表里的
`EntityInfo::name` 是同一个标签，因为句柄本身在那里只是一串整数。

| 实体的来源 | 它的名字 |
| --- | --- |
| 实现了 `Bundle` 的自定义类型 | 该类型自己的名字，如 `unlit3d_examples::scenes::spin_cube::TexturedCube` |
| `bundle!` | 宏所在的源码位置，如 `crates/unlit3d/src/source.rs:514` |
| `DynamicBundle::named` | 在那里给的标签 |
| `spawn_named` | 在那里给的标签 |
| `World::reserve_entity` | 预留时给的标签 |
| 以上都不是，手搓动态 bundle | 空 |

这一切都由 `Bundle::name` 一处作答，所以有自己命名规则的 bundle 覆盖它即可。

## 内容概览

`World`、`Entity`、`Location`、`Bundle` / `DynamicBundle`（以及写出 bundle 的
`bundle!` 宏）、把 world 收窄到一个 scope 的
`Scope` / `ScopeMut`、spawn 与 despawn 所跑的回调 `OnSpawn` / `OnDespawn`、
`Query`（指明一行交出什么的类型参数，项可以是 `Entity` 与 `Has<C>`）、
`QueryBorrow` / `QueryIter` / `QueryOne`（查询交出的持有者，以及它交出的行与单行，
archetype 谓词写作 `QueryBorrow::by`）、用于延迟结构变更的
`Command` / `Commands`、用于直接检视存储的 `Archetype` / `Archetypes`（archetype
自带其组件名字）、用于给只知 `TypeId` 的擦除类型读取者命名组件类型的
`World::type_name`、用于把 world
形态当作纯数据汇报的 `WorldInfo` / `ArchetypeInfo` / `EntityInfo`（实体字段反射时
指名 `EntityProxy` 与 `EntityVecProxy`），以及
专用哈希容器 `TypeIdHashMap`、`EntityHashMap` 等。

查询是一个类型参数。`Scope::query` 与 `Scope::query_one` 指明要取什么，`Q` 可以是
`&C`、`&mut C`、`Option<&C>`、`Option<&mut C>`、`Entity`、`Has<C>`，或者它们的元组，
顺序即书写顺序。每个 archetype 只解析一次列，所以交出的行不再查找。

`Scope::query` 本身不交出各行：它返回一个 `QueryBorrow`，后者在活着期间认领查询指名的
组件，行则由 `QueryBorrow::iter` 交出，或先由 `QueryBorrow::by` 用 `Archetype` 上的闭包
收窄要访问的 archetype 再交出。项借自持有者而不是别的什么，所以在认领被释放之前就已
消失：同时读两行是允许的，而向第二个持有者索要第一个持有者握着的组件——或把同一组件
写两次——会 panic。`Has<C>` 只读存在性、不认领列。`Scope::location` 与
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
use unlit_ecs::{Entity, Has, World, bundle};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = World::new();
let cube = world.root_scope_mut().spawn(bundle![Spin { radians_per_second: 1.0, angle: 0.0 }]);
let clock = world.root_scope_mut().spawn(bundle![0.016f32]);

// 驱动每一个 `Spin`。由调用者决定碰哪些实体、按什么顺序；库内没有场景图的概念。
let delta = *world.root_scope().query_one::<&f32>(clock).unwrap().get();
{
    let mut spins = world.root_scope().query::<&mut Spin>();
    for spin in spins.iter() {
        spin.angle += spin.radians_per_second * delta;
    }
}
assert_ne!(world.root_scope().query_one::<&Spin>(cube).unwrap().get().angle, 0.0);

// 谓词在认领任何列之前收窄要访问的 archetype：这里取「没有时钟」的那些。
let _ = world
    .root_scope()
    .query::<&Spin>()
    .by(|archetype: &unlit_ecs::Archetype| !archetype.has::<f32>())
    .iter()
    .count();

// `Entity` 与 `Has<C>` 也可以是查询项：存在性读取不认领列。
let mut presence = world.root_scope().query::<(Entity, Has<Spin>)>();
for (entity, has_spin) in presence.iter() {
    let _ = (entity, has_spin);
}
```

查询是类型而不是值：`&C` 读组件，`&mut C` 写组件，`Option<&C>` 与 `Option<&mut C>`
表示可选，`Entity` 交出实体本身，`Has<C>` 交出 `C` 在不在。`Has<C>` 不收窄任何
东西，所以实体没有 `C` 时它交出 `false` 而不是跳过该行；只想到访携带者的查询要配合
`QueryBorrow::by`，即 `by(|archetype| archetype.has::<C>())`。项不可嵌套——`&(a, b)`
是一个元组组件，`(&a, &b)` 才是两个——而组件数超出元组 impl 覆盖面时，由调用者自己
`impl Query`。`Scope::query_one` 在单个实体上跑同一个查询，交出的是 `QueryOne` 持有者，
由 `QueryOne::get` 交出那一行，于是按句柄查找与遍历 scope 共用同一套词汇；句柄指不到
可见实体、或实体不匹配该查询，都是一个 `EntityAccessError`。

spawn 与 despawn 需要 `&mut` scope，或者排入队列的命令：

```rust
use unlit_ecs::{World, bundle};

let mut world = World::new();
let entity = {
    let commands = world.root_scope().queue();
    commands.spawn(bundle!["entity"])
};
// 此时还什么都没发生。
assert!(!world.root_scope().contains(entity));
world.apply().unwrap();
assert!(world.root_scope().contains(entity));
```

bundle 是 `bundle!` 写出的或 `DynamicBundle` 收集的那份清单；元组不是 bundle，因为
元组本身就是组件。清单里每一项都是一个组件——带括号的元组仍然是元组，所以
`bundle![(a, b), c]` 是一个元组组件加一个 `c`，`bundle![()]` 则是一个空元组组件。宏在本
crate 与其下游里一律用方括号书写，`bundle![a, b]`，以便与函数调用的实参区分；只有一项
时不需要逗号，`bundle![a]` 就是那唯一的组件 `a`。宽度受编译器递归上限约束，默认上限下
约一百个组件；更宽的需要在调用方 crate 里提高 `#![recursion_limit]`：

```rust
use unlit_ecs::{Has, World, bundle};

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle![(1u32, 2.0f32), (true, ())]);

// 两个组件：一个元组，一个嵌套的元组。
let scope = world.root_scope();
assert!(scope.query_one::<Has<(u32, f32)>>(entity).unwrap().get());
assert!(scope.query_one::<Has<(bool, ())>>(entity).unwrap().get());
assert!(!scope.query_one::<Has<u32>>(entity).unwrap().get());
```

用 `..value` 写下的 bundle 会被展开成它自己的组件，于是别处建好的 bundle 可以原样
并入这次 spawn。之所以需要这个语法，是因为宏只遍历源码，分不清 bundle 和组件——
裸写的话 bundle 会被当成一个组件：

```rust
use unlit_ecs::{DynamicBundle, Has, World, bundle};

let mut gathered = DynamicBundle::new();
gathered.push_component(1u32);

let mut world = World::new();
let entity = world.root_scope_mut().spawn(bundle![..gathered, 2.0f32]);

let scope = world.root_scope();
assert!(scope.query_one::<Has<u32>>(entity).unwrap().get());
assert!(scope.query_one::<Has<f32>>(entity).unwrap().get());
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
  持有一个回调的普通组件，因此没有生命周期组件的 world 不为该特性付出任何代价，
  调用者也可以自己写一个驱动。
- **没有事件和观察者。** 作为替代，可以直接调用组件的方法或使用行为组件。*行为组件*
  指包含函数指针或闭包的组件，并且该函数中能运行对 world 的访问，就像系统或观察者
  一样，其中的函数也可以是 async 的。于是类似 Godot 节点的帧更新、固定间隔更新、
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
