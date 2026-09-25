[English](README.md) | 简体中文

# unlit_ecs

一个紧凑的 archetype ECS。组件按 archetype 存放，每种类型一列，而每个组件值都活在
自己的 cell 里——正因如此，一个共享的 `&World` 就能在不使用任何 `unsafe` 的前提下
读取*并*写入组件。

设计刻意保持精简。没有变化检测、没有组件钩子、没有事件与观察者、没有实体关系、
也没有调度器。凡是需要其中之一的场景，都由调用者在实体本身上自行完成。

本 crate 处于**极早期开发阶段**，API 会自由变动。除一个哈希器外它没有其他依赖，
也不依赖工作区中的任何其他 crate，因此可以单独使用。

## 在工作区中的位置

`unlit_ecs` 是上层所针对的 world；与 ECS 集成的渲染 API 是
[`unlit3d`](../unlit3d/README.zh-CN.md)，GPU 测试所用的测试骨架是
[`wgpu_unlit_test_util`](../wgpu_unlit_test_util/README.zh-CN.md)。本 crate 对渲染
一无所知。

## 模型

- **实体的组件集合在 spawn 时就固定了。** 组件只能读写，不能增删；要改变集合，
  必须 despawn 该实体再重新 spawn。不可变的原型正是让存储可预测、并消除其他 ECS
  设计中容易泄漏的「组件被移除」记账工作的原因。
- **没有资源（resource），只有 `Resource` 标记组件。** 资源就是调用者把 `Resource`
  与数据一起 spawn 出来的实体，对资源的引用就是一个 `Entity` 句柄。资源实体可被任何
  行为组件访问，没有数据隔离。
- **没有系统（system）。** 驱动行为意味着调用者自己读取 world 并调用它想要的方法或
  闭包——或者运行一个*行为组件*，即持有闭包、由调用者自写的驱动器调用的组件。需要
  等待的行为返回 future，由调用者决定何时 poll；库内不内置执行器。
- **没有实体关系。** 当一个实体指向另一个实体时，`Entity` 句柄存放在组件里，由调用者
  自行维护；world 不追踪也不清理该引用。
- **两个 world，一份实现。** `LocalWorld` 把组件存在 `RefCell` 里，是 `!Send` 的，
  因此留在自己的线程——渲染器和其他与线程绑定的状态属于这里。`SendWorld` 把它们存在
  `RwLock` 里，是 `Send + Sync` 的。两者只在 cell 上不同，其余全是共享代码。
- **借用冲突是 panic，而不是编译错误。** 请求一个已被借用的组件，或在同一个查询里两次
  以 `&mut` 取同一组件，都会报出组件名并终止。这是不做访问冲突分析的代价。

## 内容概览

`World`（以及 `LocalWorld` / `SendWorld` 别名）、`Entity`、`Bundle` /
`ArchetypeBuilder`、`Query` 与 `QueryFilter`（`With`、`Without`、`Or`、元组）、
用于延迟结构变更的 `Command` / `Commands`、`Resource` 标记、用于直接检视存储的
`Archetype` / `Archetypes`，以及专用哈希容器 `TypeIdHashMap`、`EntityHashMap` 等。

`Commands` 队列之所以存在，是因为结构变更需要 `&mut World`：只持有共享 world 的回调
改为把 spawn 或 despawn 排入队列，由驱动器调用 `World::apply` 落盘。`Commands::spawn`
会立即预留一个 `Entity`，因此该句柄在队列被应用之前就可用了——可以存进组件里，也可以
传给另一个回调。

遍历是确定性的：archetype 按创建顺序访问，行按存储顺序访问。archetype 按需创建，
永不删除。

## 用法

```rust
use unlit_ecs::{LocalWorld, Query, Resource, Without};

struct Spin {
    radians_per_second: f32,
    angle: f32,
}

let mut world = LocalWorld::new();
let cube = world.spawn((Spin { radians_per_second: 1.0, angle: 0.0 },));
let clock = world.spawn((Resource, 0.016f32));

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

读写只需要 `&World`；spawn 与 despawn 需要 `&mut World`，或者排入队列的命令：

```rust
use unlit_ecs::LocalWorld;

let mut world = LocalWorld::new();
let entity = {
    let commands = world.queue();
    commands.spawn(("entity",))
};
// Nothing has happened yet.
assert!(!world.contains(entity));
world.apply();
assert!(world.contains(entity));
```

## 测试

```text
cargo nextest run -p unlit_ecs   # 本 crate 的单元与集成测试
cargo xtask test                 # 整个工作区
```

测试覆盖 world 与查询行为、延迟命令，以及 `SendWorld` 能跨线程共享。它们都不需要
GPU，因此在任何环境都能运行。

## 许可证

双许可：MIT 或 Apache-2.0，任选其一。
