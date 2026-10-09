[English](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md) | 简体中文

# unlit_ecs

一个紧凑的 archetype ECS。组件按 archetype 存放，每种类型一列，而每个组件值都活在
自己的 cell 里——正因如此，一个共享的 `&World` 就能在不使用任何 `unsafe` 的前提下
读取*并*写入组件。

设计刻意保持精简：没有变化检测、没有组件钩子、没有事件与观察者、没有实体关系、
也没有调度器。凡是需要其中之一的场景，都由调用者在实体本身上自行完成。除一个哈希器
外本 crate 没有其他依赖，也不依赖工作区中的任何其他 crate，因此可以单独使用；它对
渲染一无所知。唯一的例外是可选的 `reflect` feature，它为[世界信息](crate::WorldInfo)
结构体派生 [facet](https://docs.rs/facet)，好让桥接层直接序列化它们，并导出实体字段所经由的
[`EntityProxy`]。类型为实体的反射字段指名该 proxy，于是 ECS 自己的结构体与任何调用者的组件
都用同一种方式书写实体——即它的 [`Entity::to_bits`] 整数。ECS 本身从不读取这份反射。

本 crate 处于**极早期开发阶段**，API 会自由变动。

## 模型

- **实体的组件集合在 spawn 时就固定了。** 组件只能读写，不能增删；要改变集合，
  必须 despawn 该实体再重新 spawn。不可变的原型正是让存储可预测、并消除其他 ECS
  设计中容易泄漏的「组件被移除」记账工作的原因。
- **读写只需要 `&World`。** 结构变更——spawn 与 despawn——需要 `&mut World`，或者
  由驱动器应用的 `Commands` 队列。
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
- **没有实体关系。** 当一个实体指向另一个实体时，`Entity` 句柄存放在组件里，由
  调用者自行维护；world 不追踪也不清理该引用。
- **借用冲突是 panic，而不是编译错误。** 请求一个已被借用的组件，或在同一个查询里两次
  以 `&mut` 取同一组件，都会报出组件名并终止。这是不做访问冲突分析的代价。

## 内容概览

`World`、`Entity`、`Location`、`Bundle` / `ArchetypeBuilder`（以及用于展平嵌套元组的
`bundle!` 宏）、
`Query` 与 `QueryFilter`（`With`、`Without`、`Or`、元组）、用于延迟结构变更的
`Command` / `Commands`、用于直接检视存储的 `Archetype` / `Archetypes`、用于给
只知 `TypeId` 的擦除类型读取者命名组件类型的 `World::type_name`、用于把 world
形态当作纯数据汇报的 `WorldInfo` / `ArchetypeInfo` / `EntityInfo`（实体字段反射时
指名 `EntityProxy` 与 `EntityVecProxy`），以及
专用哈希容器 `TypeIdHashMap`、`EntityHashMap` 等。

查询会按 archetype 解析一次它需要的列，再通过该状态逐行取值，因此其开销与
*组件集合*的数量成正比，而不是与实体数量成正比。`World::location` 与
`World::archetype` 把同样的形态开放给持有大量实体的调用者——比如把场景剔除成
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
let cube = world.spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
let clock = world.spawn((0.016f32,));

// Drive every `Spin`. The caller picks which entities to touch and in what
// order; the library has no built-in notion of a scene graph.
let delta = world.get::<f32>(clock).unwrap().to_owned();
for (_, mut spin) in world.query::<&mut Spin>() {
    spin.angle += spin.radians_per_second * delta;
}
assert_ne!(world.get::<Spin>(cube).unwrap().angle, 0.0);

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
    let commands = world.queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.contains(entity));
world.apply();
assert!(world.contains(entity));
```

元组 bundle 不会展平，因为元组本身就是一个组件。当组件是嵌套写下的，或者超过
十六个时，用 `bundle!`。它的宽度受编译器递归上限约束——默认上限下约一百个组件；
更宽的需要在调用方 crate 里提高 `#![recursion_limit]`：

```rust
use unlit_ecs::{bundle, World};

let mut world = World::new();
let entity = world.spawn(bundle!((1u32, 2.0f32), (true, ())));

// 宏展平了嵌套：得到三个组件，而不是一个嵌套元组。
assert!(world.has::<u32>(entity));
assert!(world.has::<f32>(entity));
assert!(world.has::<bool>(entity));
assert!(!world.has::<(u32, f32)>(entity));
```

## 为什么 ECS 这么小

<details>
<summary>这些省略是有意为之的，每一条都有替代方案</summary>

- **没有变化检测、组件钩子。** 在 OOP 中对象自身状态在内部维护；追踪什么变了是调用者
  的事。
- **没有实体关系**，如 `Children`、`ChildOf`：让用户根据需要自行管理实体之间的引用
  关系。
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
