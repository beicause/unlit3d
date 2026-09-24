# 实施计划：用 offset allocator 池化 mesh 缓冲（顶点流 + 索引）

> 状态：**计划（未执行）**。本文件只描述怎么做，不改变任何代码。
> 前置：第一步（vendored allocator）已完成并提交，见下方「已完成」。

## 0. 已完成（已提交，作为本计划的基座）

| 提交 | 内容 |
|---|---|
| `08bfe87` | `crates/wgpu_unlit_render/src/offset_allocator.rs`：vendored MIT port，仅 u32 索引、构造期固定对齐 |
| `e9e960d` | 对齐不变量文档 + 10k 次随机 churn 测试 |
| `a3ca50a` | `Allocator::extend`：尾部追加空闲区（空闲尾节点原地扩大，避免相邻空闲区无法合并） |
| `2aa5ba4` | `crates/wgpu_unlit_render/src/buffer_pool.rs`：`BufferPool`（单缓冲 + 字节单位 + 对齐 4 + 翻倍扩容拷贝） |

现有公开 API（本计划要用到）：

```
offset_allocator:
  min_allocator_size(size: u32, alignment: NonZeroU32) -> u32
  Allocator::{new, with_alignment, with_max_nodes, with_max_nodes_and_alignment}
  Allocator::{reset, allocate(u32) -> Option<Allocation>, free(Allocation),
              allocation_size(Allocation) -> u32, storage_report() -> StorageReport,
              size() -> u32, alignment() -> u32, extend(u32) -> bool}
  Allocation { pub offset: u32, metadata: NodeIndex }   // Clone + Copy

buffer_pool:
  BufferPool::{new(device, label, usage, size), buffer(), size(), free_space(),
               largest_free_range(), allocate(device, queue, size) -> Option<BufferRange>,
               release(BufferRange)}
  BufferRange::{buffer(), offset(), size(), slice()}     // Clone（含 wgpu::Buffer）
```

`git status --short` 当前为空，工作树干净。

## 1. 现状：mesh 到底有几个缓冲流

内置 unlit 一条 mesh 目前**每 mesh 独立创建 4 个缓冲**（`crates/unlit3d/src/renderer.rs:706-790`）：

| 槽位 | 常量 | 内容 | stride（字节） | stepMode | 池化目标 |
|---|---|---|---|---|---|
| 0 | `POSITION_SLOT`（`pipeline.rs:36`） | 压缩 `Snorm16x4`（或未压缩 `Float32x3`） | **8**（未压缩 **12**） | Vertex | ✅ |
| 1 | `UV_COLOR_SLOT`（`pipeline.rs:38`） | UV 与顶点色**交错在同一流** | **0/4/8/12** | Vertex | ✅ |
| 2 | `INSTANCE_SLOT`（`pipeline.rs:40`） | 每实例模型矩阵 + 基色 | 80 | **Instance** | ❌ 渲染器每帧共享一个 `instance_buffer`（`scene.rs:425`） |
| — | 索引 | u16 或 u32 | 2 / 4 | — | ✅ |

槽 1 的 stride 由通道组合决定（`mesh.rs:441` `stride()`）：

| uv | color | stride | 格式 |
|---|---|---|---|
| ✗ | ✗ | 0 | 该槽整体省略 |
| ✓ 压缩 | ✗ | 4 | `Snorm16x2` |
| ✓ 未压缩 | ✗ | 8 | `Float32x2` |
| ✗ | ✓ | 4 | `Unorm8x4` |
| ✓ 压缩 | ✓ | 8 | `Snorm16x2 + Unorm8x4` |
| ✓ 未压缩 | ✓ | 12 | `Float32x2 + Unorm8x4` |

要点：
- 槽 1 **已经是** uv 与 color 的交错流；「uv / color / uv_color 三个缓冲」在当前渲染器里对应的是**三种 stride 变体**，不是三个不同槽。
- `array_stride` 由 `pipeline.rs:734 vertex_layout()` 按「属性格式大小之和」算，并断言是 `VERTEX_ALIGNMENT`(4) 的倍数。规范同样要求 `arrayStride` 是 4 的倍数（已核对 `target/.spec-cache/webgpu.bs`）。
- `MAX_VERTEX_BUFFERS = 16`（`scene.rs:373`）；pipeline 的 `VertexState.buffers` 是 `[Option<VertexBufferLayout>; 3]`（`pipeline.rs:446-457`），`None` 表示该槽未声明。

## 2. 目标与成功标准

1. position / uv_color / index 三类数据各并入**大缓冲**，由 offset allocator 管理子分配；不再每 mesh 一个缓冲。
2. 绘制时 **绑定整个缓冲**（`slice(..)`，offset 0），范围全部由 draw 表达：顶点用 `firstVertex`/`baseVertex`，索引用 `firstIndex`。
3. 池按需自动扩容（拷贝已有内容），**已有分配的偏移不变**；扩容后所有引用者无感（句柄经资源图更新）。
4. `remove_mesh` 归还池内分配，不误删共享缓冲。
5. `allocate_mesh`（调用方自带 `wgpu::Buffer`）路径**行为不变**，仍可自定义。
6. 分配逻辑复用：索引与顶点共用同一套扩容/对齐策略，不重复实现。

## 3. 已定决策（记录用户选择，含反悔过程）

| 议题 | 决定 |
|---|---|
| 池的粒度（顶点） | **完整 vertex layout 作 key**（`array_stride` + `step_mode` + `attributes`）；stride 相同但属性不同也分池 |
| 池的粒度（索引） | **单一池**，u16 与 u32 共池，统一 4 字节对齐 |
| 多流 vs 单流 | **保留多流**（position 与 uv_color 仍是各自独立的缓冲），不学 bevy 合并成交错单流 |
| 跨流的元素下标一致性 | **共用一个「元素分配器」**（单位 = 元素，alignment = 1），不是「每流一个分配器 + 锁步调用」 |
| 对齐 | 索引池 = 4；顶点池 = 1 元素（字节偏移 = N × stride，stride 是 4 的倍数，自动满足 `COPY_BUFFER_ALIGNMENT`） |
| 扩容方式 | 单缓冲 + 追加拷贝；`Allocator::extend` 追加，已有 offset 不变 |
| 池所有权 | 通用池放 `wgpu_unlit_render`；内置 helper `allocate_unlit_mesh` 使用它；`allocate_mesh` 不变 |
| 绑定方式 | 绑整缓冲，`setVertexBuffer`/`setIndexBuffer` 不再带 offset（用户明确要求，且能吃到 `scene.rs` 录制期的状态去重） |

### 3.1 被否决的方案（留档，避免重走）

- **「每个 layout 一个独立分配器 + 每 mesh 对所有 layout 锁步分配」**：**会发散**。模拟 9 组 stride、每组 2 万次随机分配/释放，5 组发散（如 stride 8/12 在第 64 步 `n=232` 时给出元素 4021 vs 3037）。根因：分配器 bin 是 `log2` 的 8 位浮点近似，把元素数按 stride 缩放成字节后 `n*8` 与 `n*12` 落入不同 bin，`round_up`/`round_down` 边界错位。只有 stride 互为 2 的幂时恰好不发散。后果是**静默读错顶点**。
- **「每流独立分配器但用字节单位、stride 作对齐」**：同样发散（就是上面那组的另一种表述）。
- **「改成 bevy 式单流交错」**：可行但否决（要保持「按需打包通道」的灵活性）。
- **issue #9（精确尺寸分配失败）**：用户明确不修，用 `min_allocator_size` 规避。

## 4. 关键约束（规范条文，已核对）

`target/.spec-cache/webgpu.bs`，§process vertices：

```
vertexIndexList 里的 vertexIndex 是所有顶点流共享的
每个流：attributeOffset = vertexBufferOffset + vertexElementIndex * arrayStride
                          + attributeDesc.offset
        vertexElementIndex = vertexIndex（stepMode = vertex）
vertexIndex = baseVertex + relativeVertexIndex（索引绘制）
            = firstVertex + i               （非索引绘制）
```

推论：
1. 绑整缓冲（`vertexBufferOffset = 0`）时，同一条 mesh 的**所有顶点流必须落在各自池的同一个元素下标 N** 上，`firstVertex`/`baseVertex` 才能同时对上。→ 这是「共享元素分配器」存在的原因。
2. `arrayStride` 必须是 4 的倍数 ⇒ `N × stride` 必然 4 对齐 ⇒ `queue.write_buffer` 的 `COPY_BUFFER_ALIGNMENT` 约束自动满足。
3. 索引不受此约束：`firstIndex` 独立；`baseVertex` 只作用于索引值。池内偏移 4 对齐时，`firstIndex = offset / format_size` 对 u16(2)/u32(4) 都是整数。
4. `draw` / `drawIndexed` 的范围参数单位就是元素（顶点或索引），与本方案「元素分配器」的单位天然一致。
5. **shader 不使用 `@builtin(vertex_index)`**（全仓无匹配，已核实）。这是 `firstVertex = N` 安全的前提；若未来 shader 引入该 builtin，本方案需重新评估。
6. `base_vertex` 是 `i32`：池元素容量需断言 `<= i32::MAX`。

`scene.rs` 现有 API 已能表达全部三个量，**无需改动**：

| bevy 写法 | 本项目对应 |
|---|---|
| `set_vertex_buffer(0, slice(..))` | `with_vertex_buffer(slot, buffer.slice(..))`（已是整缓冲） |
| `draw(vertex_range, …)` | `DrawRange::vertices(N..N+count)`（`scene.rs:98`） |
| `drawIndexed(index_range, baseVertex, …)` | `DrawRange::indexed(F..F+count).with_base_vertex(N)`（`scene.rs:106/135`） |

## 5. 参考：bevy 的做法（`bevy_render 0.20.0-rc.1`，本地 registry 已读）

- `src/slab_allocator.rs`：通用 slab 分配器，**底座就是 `offset_allocator`**（`use offset_allocator::{Allocation, Allocator};`）。每 slab = 一个 `wgpu::Buffer` + 一个 `Allocator`。
- 按 layout 分 slab：`slab_layouts: HashMap<ElementLayout, Vec<SlabId>>`；`ElementLayout::vertex` 用 `array_stride` 作元素大小，`ElementLayout::index` 用 2/4。
- `elements_per_slot = [1, 4, 2, 4][size & 3]`（等价 `4/gcd(4,size)`）：解决「元素大小不是 4 的倍数」时 slot 边界对齐问题。**我们的顶点 stride 必是 4 的倍数，索引走字节池，故不需要这个概念**；但值得留档。
- `SlabAllocatorSettings`：`min_slab_size` 1 MiB、`max_slab_size` 512 MiB、`growth_factor` 1.5、`large_threshold` 256 MiB（超阈值独占 buffer）。
- 扩容：`grow_if_necessary` 按 `growth_factor` 增长，`reallocate_slab` 整缓冲拷贝，把该 slab 的 key 记入 `displaced_keys`。
- **绘制（`bevy_pbr/src/render/mesh.rs`）**：
  ```rust
  pass.set_vertex_buffer(0, vertex_buffer_slice.buffer.slice(..));      // 4712
  pass.draw_indexed(index_range.start..(index_range.start + count),     // 4733
                    vertex_range.start as i32, batch_range);
  pass.draw(vertex_buffer_slice.range, batch_range);                    // 4806
  ```
  `SlabAllocationBufferSlice::range` 的单位是**元素**（`slab_allocator.rs:558`），于是三个量天然对齐、无需除法。
- bevy 没有多流问题，因为它的顶点是单流交错的（`bevy_mesh/src/mesh.rs:957`：所有属性累加 offset 交错，`array_stride = accumulated_offset`）。

**结论**：bevy 的可复用部分 = 「绑整缓冲 + 元素为单位作 draw 范围 + 扩容整缓冲拷贝 + 句柄位移需通知引用者」。我们额外要解决的只有「多流共享同一元素下标」。

## 6. 设计

### 6.1 索引池：直接复用 `BufferPool`

索引数据按**字节**分配，`alignment = 4`，单一池。

- 分配尺寸 = `index_count * format_size()`，向上取整到 4（`BufferPool` 已经这么做）。
- `firstIndex = pool_offset / format_size`（4 对齐 ⇒ u16 时整除 2、u32 时整除 4）。
- 写入前仍按 `COPY_BUFFER_ALIGNMENT` 补齐（现有代码已做，`renderer.rs:779-793`）。
- u16 与 u32 混在同一池是安全的：`set_index_buffer` 绑整缓冲（offset 0），格式逐 draw 指定。

### 6.2 顶点流池（新增类型）

新增 `crates/wgpu_unlit_render/src/vertex_pool.rs`（公开模块），核心是**一个元素分配器 + 每个 layout 一个缓冲**：

```
VertexStreamPool {
    label: &'static str,
    usage: wgpu::BufferUsages,          // VERTEX | COPY_DST（创建时自动加 COPY_SRC）
    allocator: Allocator,               // 单位 = 元素，alignment = 1，size = 元素容量
    streams: HashMap<VertexBufferLayoutDesc, wgpu::Buffer>,  // 每个 layout 一个缓冲
    generation: u64,                    // 每次扩容 +1，便于调用方同步资源图
}

VertexAllocation { allocation: Allocation, count: u32 }   // 元素区间 [offset, offset+count)
```

不变量：
- 某个 layout 的缓冲字节大小 == `allocator.size() * layout.array_stride`。
- 一切分配的字节偏移 == `元素偏移 × stride`，因此必然是 stride 的整数倍（进而 4 的倍数）。

API（草案）：

```rust
impl VertexStreamPool {
    pub fn new(device, label, usage, initial_element_capacity: u32) -> Self;
    /// 确保 `layouts` 每个都有缓冲，然后分配 `count` 个元素。
    pub fn allocate(&mut self, device, queue, layouts: &[VertexBufferLayoutDesc], count: u32)
        -> Option<VertexAllocation>;
    pub fn release(&mut self, allocation: VertexAllocation);
    pub fn buffer(&self, layout: &VertexBufferLayoutDesc) -> Option<&wgpu::Buffer>;
    pub fn generation(&self) -> u64;
    pub fn element_capacity(&self) -> u32;
    /// 元素下标 → 该 layout 缓冲内的字节偏移。
    pub fn byte_offset(layout: &VertexBufferLayoutDesc, index: u32) -> u64;   // index as u64 * stride
}
```

扩容（内部 `grow`，与 `BufferPool::grow` 同构）：
1. `needed = min_allocator_size(count, NonZeroU32::MIN)`（规避 issue #9：精确尺寸的空闲区可能搜不到）；
2. `additional = current_capacity.max(needed)`；`allocator.extend(additional)`，失败则返回 `None`；
3. 为**每个已注册 layout** 新建缓冲，大小 = `新容量 × stride`，把旧缓冲内容一次性拷入（一个 encoder、多个 `copy_buffer_to_buffer`、一次 submit）；
4. `generation += 1`。

注意点：
- 空流（`array_stride == 0`，无属性）**不注册缓冲、不分配**，也**不进** `GpuMesh.vertex_buffers`。这同时修掉现有代码在「无 uv 且无 color」变体下可能创建 0 字节缓冲的隐患（`renderer.rs:722-727`）。需确认该变体当前是否可达。
- 扩容拷贝时旧缓冲「有效长度」取 `旧容量 × stride`（不是整个旧缓冲大小），避免越界。
- `max_nodes` 取小值（如 4096；注意 `Allocator` 的 128K 默认是 C++ 原版遗留，约 3.5 MB 元数据，池用不起）。

### 6.3 与资源图的结合（本计划最需要小心的一环）

`docs/DESIGN.md` 要求资源图是 GPU 资源的单一真相来源，因此池缓冲必须进图。

- 池缓冲以 **strong 节点**入图（调用方 = `Renderer` 持有），**不挂在 mesh 虚拟根下**：它们被多条 mesh 共享，不属于任何一条。
- 每个 layout 一个节点，索引池一个节点；渲染器维护 `HashMap<VertexBufferLayoutDesc, (ResourceId, u64 /*已同步的 generation*/)>` 与 `Option<ResourceId>`（索引）。
- **扩容后**用 `ResourceGraph::replace(node, Resource::Buffer(new))` 换句柄：`ResourceId` **保持不变**，因此 `GpuMesh` 里存的 id 无需更新，draw 路径每帧从图里取当前句柄自然拿到新缓冲。
- 为避免每帧都 `replace`（会置 dirty），用 `generation` 判断：仅当 `pool.generation() > 已同步值` 时替换。
- mesh 的 bind group **不再依赖顶点/索引缓冲**（它只读 mesh-info uniform）。现有 `renderer.rs:599-607` 把所有 buffers 加进依赖属于过宽，池化后应去掉。

### 6.4 `GpuMesh` 变化（`crates/unlit3d/src/components.rs:123`）

保留 `vertex_buffers` / `index_buffer` 的**形状**（仍存 `ResourceId`，池化后指向池缓冲节点），新增范围字段：

```rust
pub vertex_offset: u32,          // 元素下标 N：firstVertex / baseVertex；调用方自带缓冲时为 0
pub index_offset: u32,           // firstIndex；调用方自带缓冲时为 0
// 归还池分配用（Copy 小值，不持有 wgpu 句柄）：
pub(crate) vertex_allocation: Option<VertexAllocation>,
pub(crate) index_allocation: Option<Allocation>,
```

`vertex_offset`/`index_offset` 默认 `0` 使**两条路径统一**：`allocate_mesh`（自带缓冲）保持今天的行为（`0..count`），无需枚举区分。

`remove_mesh`（`renderer.rs:1005`）增加两步：归还 `vertex_allocation` 与 `index_allocation` 给各自的池；其余（`remove_drop(root)` + `cleanup_drop()` + 归还 metadata 槽）不变。池缓冲是 strong 节点，`cleanup_drop` 不会收集它们。

### 6.5 绘制路径变化

- `scene.rs:416-421`：`with_vertex_buffer` 仍绑 `buffer.slice(..)`（本来就是整缓冲）。
- `DrawShape`（`scene.rs:334`）新增 `vertex_offset: u32` 与 `index_offset: u32`；`EntryHandles` 的 `index_buffer` 沿用。
- `assemble_scene`（`scene.rs:400-404`）改为：
  ```rust
  let range = if handle.shape.indexed {
      DrawRange::indexed(index_offset .. index_offset + count)
          .with_base_vertex(vertex_offset as i32)
  } else {
      DrawRange::vertices(vertex_offset .. vertex_offset + count)
  };
  ```
- 取值处（`renderer.rs:1105-1190`）把 `mesh.vertex_offset` / `mesh.index_offset` 填进 `DrawShape`。
- 收益：同一池的多条 mesh 绑定的 `BufferSlice` 完全相同，`scene.rs` `record` 的状态缓存能跨 draw 跳过重复 `set_vertex_buffer`/`set_index_buffer`（这正是用户要「绑整缓冲」的实际理由）。

### 6.6 `BufferRange` 小重构（建议，非必须）

`BufferRange` 现在内嵌 `wgpu::Buffer`，存进 `GpuMesh` 会让旧缓冲因引用而延迟释放。建议改为不持有句柄：

```rust
#[derive(Clone, Copy, Debug)]
pub struct BufferRange { allocation: Allocation, size: u32 }
impl BufferRange { pub fn offset(&self)->u32; pub fn size(&self)->u32; pub fn allocation(&self)->Allocation; }
```

写入处改为 `queue.write_buffer(pool.buffer(), range.offset() as u64, data)`（`BufferPool::buffer()` 已存在）。这样 `GpuMesh` 只存 `Allocation`，且 `BufferPool::release` 可直接收 `Allocation`（或保留 `release(BufferRange)` 作语法糖）。项目处于早期，破坏性改动可接受。

## 7. 分阶段实施（每阶段可独立编译 + 测试 + 提交）

**P0 — 准备**
1. `BufferRange` 按 6.6 重构（含 `buffer_pool.rs` 测试更新）。
2. 给 `BufferPool` 加 `pub fn release_allocation(&mut self, Allocation)`（或让 `release` 收 `Allocation`）。
3. `Allocator` 无需改动。

**P1 — 索引池（先用单缓冲验证「图 replace + generation」机制）**
1. `Renderer` 增加字段：`index_pool: BufferPool`、`index_pool_node: ResourceId`、`index_pool_generation: u64`。
2. `allocate_unlit_mesh` 的索引分支改为 `index_pool.allocate(...)` + `write_buffer(pool.buffer(), offset, padded)`；`GpuMesh.index_offset = offset / format_size`，`index_allocation = Some(allocation)`。
3. 扩容后 `graph.replace(index_pool_node, Resource::Buffer(...))`。
4. `remove_mesh` 归还索引分配。
5. 绘制路径支持 `index_offset`（`DrawShape` 加字段）。
6. 测试：多 mesh 共用一个索引缓冲；删一条后其索引区间可复用；扩容后偏移不变。

**P2 — 顶点流池**
1. 新增 `vertex_pool.rs`（6.2）+ 单元测试。
2. `Renderer` 增加 `vertex_pool: VertexStreamPool` 与 layout→节点表。
3. `allocate_unlit_mesh` 改为：压缩后按 layout 注册、`allocate(vertex_count)`、按 `N×stride` 写入各流；`GpuMesh.vertex_offset = N`。
4. `remove_mesh` 归还顶点分配。
5. 绘制路径支持 `vertex_offset`（`DrawShape` 加字段）。
6. 空流处理（6.2 注意点）。

**P3 — 收尾**
1. 更新受影响的既有测试（见 §8）。
2. `cargo clippy` → `cargo fmt`（顺序不可颠倒）。
3. 全量测试 + 示例运行 + 快照对比。
4. 提交；并在 `docs/DESIGN.md` **征求用户同意后**补充池化说明（未经许可不擅自改）。

## 8. 测试计划

**新增（`wgpu_unlit_render`）**
- `vertex_pool`：元素下标跨 stride 一致；释放后区间复用；扩容后旧分配偏移不变；`byte_offset == N × stride`；空 stride 不建缓冲；容量上限断言。
- 复用性验证：用一个"两流"（stride 8 与 12）场景跑随机分配/释放，断言**两个流拿到的元素区间始终相同**（把之前的一次性模拟固化为回归测试，防未来有人改成按流分配）。

**需要修改的既有测试（会因池化而失效）**
| 位置 | 现状 | 池化后 |
|---|---|---|
| `renderer.rs:1816` `removing_a_mesh_frees_every_resource_built_from_it` | 断言顶点/索引缓冲随 mesh 一起从图里消失 | 应改为：root / bind group / mesh-info 消失；**池缓冲仍在**；且池的空闲空间增加（分配已归还） |
| `renderer.rs:1849` `a_mesh_registers_every_part_under_its_virtual_root` | 断言顶点/索引缓冲是 root 的依赖 | 应改为：root 只依赖 bind group 与 mesh-info；池缓冲不属于 root |
| `renderer.rs:2309`（metadata 槽复用附近） | 依赖 remove_mesh 释放缓冲的图大小不变断言 | 需重新表述（池缓冲常驻） |
| `tests/gpu_ecs.rs:171` | 取 `vertex_buffers[0].1` 的缓冲 | id 仍有效（指向池缓冲），但语义变了，需确认断言仍成立 |
| `tests/custom_pipeline.rs:340/460` | 用 `allocate_mesh` + 自带缓冲 | **不应改动**，用于证明自定义路径未被破坏 |

**回归关注**
- `unlit3d_examples` 与快照测试：几何应逐像素不变（池化不改变绘制内容）。若快照变了，说明 `firstVertex`/`baseVertex` 算错。
- `cargo test -p unlit3d`、`-p wgpu_unlit_render`、workspace 全量。

## 9. 风险与未决问题

1. **`firstVertex`/`baseVertex` 的正确性**是全局风险点：一旦 N 与流不一致就静默错几何。→ 用 §8 的跨流一致性回归测试兜底。
2. **`base_vertex: i32`**：元素容量理论上限 2^31；需显式断言或用 `try_into` 并在超限时拒绝分配。
3. **空 uv_color 流**：现有代码对「既无 uv 也无 color」的变体会创建 0 字节缓冲（可能 panic）。需先确认可达性，再决定是修掉还是断言。
4. **扩容时机与帧边界**：`grow` 内部 `queue.submit` 一次拷贝。`allocate_unlit_mesh` 在录制帧之外调用，安全；但需确认没有「帧录制中途分配」的调用路径。
5. **池缓冲的生命周期**：strong 节点永不自动回收 ⇒ 不再使用的 layout 会常驻。是否需要「空闲池回收」策略（如引用计数为 0 时释放）留待后续；当前先记录为已知代价。
6. **mesh-info uniform 未池化**：每 mesh 一个 uniform 缓冲（需要 256 字节动态偏移对齐才适合池化）。本计划**不含**，留作后续。
7. **`STORAGE`/其它 usage 的自定义池**：本计划只覆盖内置 unlit 的三类；`VertexStreamPool` 保持通用（usage 可配），供自定义管线复用。
8. **是否重命名 `GpuMesh.vertex_buffers`**：池化后其元素仍是「槽位 → 缓冲节点」，语义未变，建议**保留名字**以减少 churn；如需更精确可改 `vertex_streams`（会连带改测试）。

## 10. 验证命令

```bash
# 单元层
cargo test -p wgpu_unlit_render --lib
# 集成层
cargo test -p unlit3d
# 示例 / 快照
cargo test --workspace
# 静态检查（先 clippy 后 fmt）
cargo clippy --workspace --all-targets
cargo fmt --check
```

## 11. 附：需要改动的文件清单

| 文件 | 改动 |
|---|---|
| `crates/wgpu_unlit_render/src/vertex_pool.rs` | **新增**：`VertexStreamPool` + `VertexAllocation` + 测试 |
| `crates/wgpu_unlit_render/src/lib.rs` | 注册 `pub mod vertex_pool;` 并补模块列表文档 |
| `crates/wgpu_unlit_render/src/buffer_pool.rs` | `BufferRange` 去句柄化（6.6）；如需 `release_allocation` |
| `crates/unlit3d/src/renderer.rs` | 池字段；`allocate_unlit_mesh` 改写；`remove_mesh` 归还；绘制路径填 `vertex_offset`/`index_offset`；`DrawShape` 传递；相关测试更新 |
| `crates/unlit3d/src/components.rs` | `GpuMesh` 新增 `vertex_offset`/`index_offset`/两个 allocation 字段 |
| `crates/unlit3d/src/scene.rs` | `DrawShape` 加两个 offset 字段；`assemble_scene` 用它们构造 `DrawRange` |
| `crates/unlit3d/src/mesh.rs` | 预计不改（`MeshDesc` 保持调用方自带缓冲的形状） |
| `crates/wgpu_unlit_render/src/offset_allocator.rs` | 预计不改（`extend` 已具备） |
| `docs/DESIGN.md` | **需用户同意后**再补池化说明 |
