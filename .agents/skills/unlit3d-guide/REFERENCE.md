# unlit3d / unlit3dx API 速查

签名取自源码，改动代码后请顺手核对。省略了 `pub`、`&self` 之类的修饰。

## 启动与 world

| 项 | 签名 | 源 |
| --- | --- | --- |
| 建渲染上下文 | `spawn_context(world: &mut World, device, queue, graph: ResourceGraph, capabilities: DeviceCapabilities) -> RenderContext`；每 world 一次，GPU 状态进 shared 子树 | `crates/unlit3d/src/source.rs` |
| 上下文句柄 | `RenderContext { device, queue, graph, capabilities, clock }`，Copy；`ctx.graph(scope)`、`ctx.maintain_scope(scope)`、`RenderContext::of(world, anchor) -> Result<Self, EntityAccessError>` | 同上 |
| 根作用域 | `World::root_scope()` / `root_scope_mut()` / `scope(e)` -> `Result` / `scope_mut(e)` / `shared()` / `is_shared(e)` | `crates/unlit_ecs/src/world.rs` |
| 作用域只读 | `Scope`：`entity / query / query_one / archetype_of / archetypes / scope / parent_scope / children / len / is_empty / info / queue` | `crates/unlit_ecs/src/view.rs` |
| 作用域可写 | `ScopeMut`：`entity / as_scope / spawn / spawn_named / spawn_at / despawn / link / unlink / scope_mut / apply` | 同上 |
| 落地排队命令 | `World::apply(&mut self) -> Result<(), EntityAccessError>`；按序应用，期间新排的也应用，失败不中断 | `crates/unlit_ecs/src/world.rs` |
| 渲染器 | `Renderer::new(ctx)`、`Renderer::render(&mut self, world: &World)`、`context()` | `crates/unlit3d/src/renderer.rs` |

## pass 与 source

| 项 | 签名 | 源 |
| --- | --- | --- |
| pass 组件 | `Pass { pub frame_pass: FramePass, pub load: RenderLoadOps }`，`Pass::new(order: FramePassOrder)`；Bundle 展开成两者 | `crates/unlit3d/src/pass.rs` |
| 绘制顺序 | `FramePassOrder(pub i32)`，低者先录 | 同上 |
| 附件 | `FramePass::set_attachments(&mut self, &FrameAttachments, color_view: ResHandle<TextureView>)` | 同上 |
| 视口 | `FramePass::viewport: Option<ViewportRect>`；`ViewportRect::new(w, h).at(x, y)`、`fit_aspect(aspect, w, h)` | 同上 |
| 每 pass 信息 | `PassInfo { surface: SurfaceKey, width, height, viewport }` | 同上 |
| 清屏 | `RenderLoadOps { color: LoadOp<Color>, depth: LoadOp<f32>, stencil: LoadOp<u32> }`，默认 `color_clear()` / `depth_clear()`（清 0.0，reverse-z 的远平面）/ `stencil_clear()` | `crates/unlit3d/src/components.rs`、`crates/unlit_wgpu/src/render_attachments.rs` |
| source 组件 | `Source::new(impl FrameSource)`、`with_order`、`as_mut::<T>()`、`as_ref::<T>()` | `crates/unlit3d/src/source.rs` |
| source 顺序 | `FrameSourceOrder::MESH = 0`、`FrameSourceOrder::UI = 100` | 同上 |
| 自定义 source | `trait FrameSource { build_scene(&mut self, scope, ctx, info, scene, encoder); fn order(&self) -> FrameSourceOrder; }`；会被覆盖它的**每个 pass 各调一次**，per-pass 状态按 `info.surface` 存 | 同上 |

`FrameAttachments::new(world: &World, ctx: RenderContext, format: wgpu::TextureFormat, size: (u32, u32), sample_count: u32, with_depth: bool)` 只分配**深度**（`with_depth` 为真时）与 **MSAA**（`sample_count > 1` 时）两张纹理并把视图登记进图；颜色视图另由调用方交给 `set_attachments`。颜色 target 用 `create_color_target(device, format, w, h)` 建，深度用 `create_depth_target(device, w, h, sample_count)`（格式由 `default_depth_stencil_format(device)` 决定，不传参），MSAA 用 `create_msaa_target(device, format, w, h, sample_count)`。

## mesh 与材质（MeshSource）

| 项 | 签名 | 源 |
| --- | --- | --- |
| 建 | `MeshSource::new(&Scope, ctx)`、`device/queue/capabilities(&Scope)`、`context()` | `crates/unlit3d/src/mesh_source.rs` |
| 注册管线族 | `register_family::<K, F, I>(&mut self, world: &Scope, factory, instances)`；同 key 类型重复注册 panic | 同上 |
| 建 mesh | `allocate_mesh(&mut self, world: &Scope, desc: MeshDesc) -> GpuMesh` | 同上 |
| 建纹理 | `register_texture_and_default_view(&mut self, world: &Scope, texture) -> (ResHandle<Texture>, ResHandle<TextureView>)` | 同上 |
| 建采样器 | `register_sampler(&mut self, world: &Scope, Option<wgpu::SamplerDescriptor>) -> ResHandle<wgpu::Sampler>` | 同上 |
| 建材质 | `allocate_material(&mut self, world: &Scope, build: impl Fn(&ResourceGraph) -> wgpu::BindGroup, dependencies) -> GpuMaterial` | 同上 |
| 释放 | `remove_mesh(&mut self, GpuMesh)`、`maintain(&mut self, &Scope)` | 同上 |
| 描述 | `MeshDesc { vertex_buffers: ArrayVec<VertexBufferDesc, MAX_VERTEX_BUFFERS>, index_buffer: Option<(Buffer, IndexFormat)>, count: u32, indexed: bool, aabb: Aabb, bind_group: Option<BindGroup>, morph_deltas: Option<MorphDeltas> }` | `crates/unlit3d/src/mesh.rs` |

## 内建 unlit family

| 项 | 签名 | 源 |
| --- | --- | --- |
| 注册 | `MeshSourceUnlitExt::register_unlit_family(&mut self, world: &Scope)` | `crates/unlit3d/src/unlit.rs` |
| 建 mesh | `allocate_unlit_mesh(&mut self, world: &Scope, key: &UnlitPipelineKey, desc: UnlitMeshDesc) -> GpuMesh` | 同上 |
| 建材质 | `allocate_unlit_material(&mut self, world: &Scope, key, view: ResHandle<TextureView>, sampler: ResHandle<Sampler>) -> GpuMaterial` | 同上 |
| 描述 | `UnlitMeshDesc { positions: &[[f32;3]], uvs: Option<&[[f32;2]]>, colors: Option<&[[u8;4]]>, indices: Option<&[u32]>, joints: Option<&[[u16;4]]>, weights: Option<&[[f32;4]]>, morph_deltas: Option<MorphDeltas> }`，positions 之外的 slice 缺省即不打包该通道 | 同上 |
| key | `UnlitPipelineKey::new(options: UnlitOptions)`，`pub options: Hashed<UnlitOptions>` | 同上 |
| 管线组件 | `type UnlitPipeline = GpuRenderPipeline<UnlitPipelineKey>`；实例=`bundle![mesh, material, UnlitPipeline::new(key)]` | 同上 |
| 每实例覆盖 | `InstanceColor(pub Vec4)`（默认白）、`InstanceCutoff(pub f32)`、`InstanceVisibility { visible: bool }`（默认 true） | 同上 |
| 实例记录 | `UnlitInstance`（family 的 `InstanceData`） | 同上 |
| 排序 | `ZSortedDrawing`：带它的在不带它的之后画，按视轴深度从后往前；不改 blend | 同上 |

## 管线策略 UnlitOptions

`UnlitOptions` 全部字段都不带默认值，用构造函数起底再改：

| 构造 | 内容 | 源 |
| --- | --- | --- |
| `standard(device)` | `standard_shape()` 再填 `default_depth_stencil_format(device)` | `crates/unlit_wgpu/src/pipeline.rs` |
| `standard_shape()` | cull `Back`；深度占位 `Depth24PlusStencil8` + 写深度 + `Greater`（reverse-z）；颜色 `Rgba8UnormSrgb`、无 blend；`multisample.count = 4`。`pub(crate)`，外部只能用 `standard` | 同上 |
| `default_depth_stencil_format(device)` | 设备有 `DEPTH32FLOAT_STENCIL8` 就是 `Depth32FloatStencil8`，否则 `Depth24PlusStencil8`；`create_depth_target` 与 `standard` 都用它 | `crates/unlit_wgpu/src/render_attachments.rs` |

| 字段 | 类型 |
| --- | --- |
| `shader` | `Option<UnlitShader>` |
| `srgb_to_linear_output` | `bool` |
| `base_color` | `BaseColorChannels` |
| `alpha_cutoff` | `bool` |
| `texture_filtering` | `bool` |
| `primitive` | `wgpu::PrimitiveState` |
| `depth_stencil` | `Option<wgpu::DepthStencilState>` |
| `color_target` | `wgpu::ColorTargetState` |
| `multisample` | `wgpu::MultisampleState` |

## 相机与变换

| 项 | 签名 | 源 |
| --- | --- | --- |
| 相机 | `Camera { view_from_world: Mat4, clip_from_view: Mat4, active: bool }`、`clip_from_world()`、`position()` | `crates/unlit3d/src/components.rs` |
| 变换 | `Transform { translation: Vec3, rotation: Quat, scale: Vec3 }`，`Default` 是单位变换 | 同上 |
| 可见性 | `InstanceVisibility { visible }`、`MeshParts`、`SkinBinding`、`SkinPose`、`MorphBinding`、`MorphWeights` | 同上 |
| 投影 | `glam::camera::rh::proj::directx::perspective_infinite_reverse(fov_y, aspect, z_near)` | glam |

`culling::is_culled` / `bounds::{Aabb, Obb, FrustumPlanes}` 自己算视锥剔除；`Renderer` 不做遮挡剔除。

## 帧

| 项 | 签名 | 源 |
| --- | --- | --- |
| 帧 | `Frame { pub delta_time: f32, pub index: u32, pub size: (u32, u32) }` | `crates/unlit3d/src/frame.rs` |
| 时钟 | `FrameClock { time, delta_time, index }`、`advance(delta_time, index)` | 同上 |
| 派发 | `dispatch_frame(world: &mut World, frame: &mut Frame)`：先 reset `FrameSkip`、推进所有 `FrameClock`，再把 `OnFrame` 按 order 跑一遍（`sort_unstable_by_key` 只按 order，实体不参与比较），最后 `world.apply()` | 同上 |
| 行为 | `OnFrame::new(f)`、`OnFrame::with_order(order, f)`，f 形如 `FnMut(&World, Entity, &Frame)` | 同上 |
| 顺序 | `FrameBehaviourOrder(pub i32)`，低者先跑，**同序无序**；dispatch 期间不得增删行为 | 同上 |
| 跳过 | `FrameSkip` | 同上 |

## 输入

| 项 | 签名 | 源 |
| --- | --- | --- |
| 写端 | `InputWriter::new()`、`push(event)` | `crates/unlit3d/src/input/mod.rs` |
| 订阅组件 | `InputFeed(InputWriter)`，spawn 进世界才收事件 | 同上 |
| 派发 | `dispatch_input(&World) -> bool`：取一个 batch、折进 `InputState`、跑行为；不 apply 结构变更 | 同上 |
| 读状态 | `InputEvents::with_state(R, f)`、`with_batch`、`refresh()`、`copy_events_into` | 同上 |
| 行为 | `OnInput`（catch-all）、`OnKey`、`OnPointer`、`OnMouse`、`OnTouch`、`OnText`、`OnIme` | 同上 |
| 分发 | `subscribe_input(&mut World)`（向各 world 的缓冲登记订阅）、`distribute_input(&World)`（把已到的事件发给行为组件） | 同上 |

## UI

| 项 | 签名 | 源 |
| --- | --- | --- |
| source | `UiSource::new()`、`publish_target(Snapshot<UiTarget>)`、`context()`、`context_mut()` | `crates/unlit3d/src/ui/mod.rs` |
| 面板 | `UiPanel::new(f)`，f 形如 `FnMut(Scope<'_>, Entity, &mut egui::Ui)` | 同上 |
| 跨 world | `ExternalFrame`、`ExternalFrames::new() / publish(slot, frame) / with_frames`、`UiTarget { screen, max_texture_side }`、`lay_out(..)`、`raw_input(..)`。`publish` 把被顶掉帧的纹理增量 `append` 给继任者——egui 只在写出纹理的那一帧带增量，丢掉它就没材质可绑 | 同上 |

## winit 桥

| 项 | 签名 | 源 |
| --- | --- | --- |
| 宿主 | `WinitHost<U>::new()`、`world()` / `world_mut()`、`run(event_loop)` | `crates/unlit3d/src/winit/event.rs` |
| 内置行为 | `WindowSetup`、`ExitOn(ExitTrigger::CloseRequested\|Escape)`、`ExitRequest`、`CreateWindowRequest`、`request_redraw_while_foreground()` | `crates/unlit3d/src/winit/builtin.rs` |
| 窗口 | `WindowSurface::new(..)`、`acquire(nested_world, &mut renderer) -> Option<Frame>`、`Frame::color_view()`、`Frame::present(&queue)` | `crates/unlit3d/src/winit/surface.rs` |

回调里拿不到 `ActiveEventLoop`：要窗口就 spawn `WindowSetup`，要退出就 spawn `ExitRequest(true)`，宿主在一轮 dispatch 之后统一执行。回调只拿 `&World` 且不得再进宿主；结构变更走 `Scope::queue`。

## 多 world（unlit3dx）

| 项 | 签名 | 源 |
| --- | --- | --- |
| runner | `trait WorldRunner { const SELF_DRIVEN: bool; fn spawn<I, S, T>(host: &mut World, name, command_capacity, init, tick) -> WorldHandle; }` | `crates/unlit3dx/src/world.rs` |
| 自带线程 | `spawn_threaded_world<T: FnMut(&mut World, &ThreadControl, f32) -> bool + Send>(host, name, cap, tick)`、`..._with_init` | 同上 |
| 定步长 | `spawn_stepped_world_with_init`、`step_stepped_world(host: &World, entity, delta_seconds: f32)`、`with_stepped_world` | 同上 |
| 节拍 | `ThreadControl::self_driven()`、`wait_timeout(Duration)`、`FixedClock::{new, set_step, wait}`、`fixed_steps(&mut debt, delta_seconds, step_seconds) -> u32` 与 `MAX_CATCH_UP_STEPS = 4` | `crates/unlit3dx/src/thread.rs` |
| 子 world | `spawn_sub_world(host: &mut World, name, cap) -> WorldHandle`、`drain_sub_world(&mut World)` | `crates/unlit3dx/src/host.rs` |
| 句柄 | `WorldHandle { entity, sender: JobSender, state: SnapshotReader<WorldState> }`、`new`、`is_finished()`、`stop()` | `crates/unlit3dx/src/handle.rs`、`world.rs` |
| 名字 | `WorldName(&'static str)` 组件；`Threaded` / `Stepped`；`WorldState = Running\|Stopped\|Panicked` | `crates/unlit3dx/src/world.rs` |

| 通信 | 签名 | 语义 |
| --- | --- | --- |
| job | `type Job = Box<dyn FnOnce(&mut World) + Send>`；`JobSender::send(Job) -> Result<(), SendError>` | 有界队列，`Full` / `Closed`，永不阻塞；世界每次 tick 前 drain |
| 状态 | `Snapshot<T>`：`publish(T) -> usize`、`reader()`、`has_readers()`；`SnapshotReader::latest() -> Option<Arc<T>>` | 单槽，订阅前发布的值也留着 |
| 事件 | `Stream<T>`：`subscribe() -> StreamReader<T>`、`publish` / `publish_all` | 每个 reader 自带队列、按序、reader drop 即退订 |
| 应答 | `Pending::new(state, timeout)`、`request(sender, key, make)`、`advance(&mut Vec<(K, Settled<T>)>)` | 一次请求一次应答；`Settled::Answered` / `Expired` |
| 发布 | `publish_collected(world) -> usize` | 世界里收集到的快照/流在 tick 之后统一发布 |

## 展示路径（unlit3dx）

| 项 | 签名 | 源 |
| --- | --- | --- |
| GPU | `request_gpu(app, ready: FnOnce(&mut World, &Gpu))` 返回 `OnFrame`；`Gpu { instance, adapter, device, queue, capabilities }`；`Gpu::request(instance, surface)` / `Gpu::headless()` 都是 `async fn` 返回 `Result`，实际用法是让 `request_gpu` 在 GPU 就绪那次回调里交给你 | `crates/unlit3dx/src/gpu.rs` |
| 常量 | `SCENE_PASS_ORDER = FramePassOrder(0)`、`BLIT_PASS_ORDER = FramePassOrder(1)`（这两个公开；同文件的 `OFFSCREEN_FORMAT` / `FRAME_SAMPLES` 是 `pub(crate)`，外部用不到） | `crates/unlit3dx/src/present.rs` |
| 装配 | `setup_presenter(world, render, renderer, scene_pass, gpu, fallback_size, offscreen) -> Result<(), String>` | 同上 |
| 离屏 | `bind_offscreen(world, ctx, pass, device, size) -> Result<OffscreenTarget, EntityAccessError>`；`OffscreenTarget { texture, view }`。`device` 必须是 `setup_presenter` 用的那个 `gpu.device`，否则附件进不了同一个图 | 同上 |
| 每帧行为 | `draw_presented()`、`pump_presented() -> (OnAboutToWait, OnWindowEvent)`、`resize_presented()` | 同上 |
| 组件 | `Presenter { render, renderer, scene_pass, blit, gpu, surface, attachments, fallback_size }` | 同上 |

## 物理 / 音频 / 合成

| 项 | 签名 | 源 |
| --- | --- | --- |
| 物理 | `Physics::new(gravity)`、`init_physics_world(world, &PhysicsConfig)`、`spawn_physics_world_with::<R>(host, config)` | `crates/unlit3dx/src/physics.rs` |
| 刚体 | `spawn_body(world, BodyDesc) -> RigidBodyHandle`、`spawn_fixed_cuboid(world, position, half_extents, friction)`、`despawn_body`、`step_once(world) -> bool` | 同上 |
| 常量 | `PHYSICS_HZ = 60.0`、`FRAME_SECONDS` | 同上 / `crates/unlit3dx/src/lib.rs` |
| 音频 | `spawn_audio_world_with::<R>(host, AudioConfig)`、`dispatch_audio_fill(world, out, sample_rate, channels)`、`OnAudioFill::new(f)`、`AudioFillOrder(i32)`、`GlobalAudioGain(f32)` | `crates/unlit3dx/src/audio.rs` |
| 合成 | `load_soundfont(bytes)`、`load_midi_file(bytes)`、`SynthVoice::from_font(world, &Arc<SoundFont>)`、`MidiSequencer::new(file: Arc<MidiFile>) / advance(delta, voice, audio)`、`midi_job(voice, kind, delay)` | `crates/unlit3dx/src/synth.rs` |

## glTF

`UnlitGltf`（`crates/unlit3d/src/gltf.rs`）：`load(path)` / `from_bytes`、`pipeline_key`、`insert_resources(&self, source: &mut MeshSource, world: &Scope) -> GltfResources`、`spawn_default_scene(&self, world: &mut ScopeMut, &GltfResources) -> Vec<GltfNode>`、`apply_animation` / `animation_duration` / `animation_name`、`skin_pose` / `morph_weights`、`insert_image` / `insert_material` / `insert_mesh`。

## 常用常量与命令

| 项 | 值 |
| --- | --- |
| 示例格式 | 示例侧常量，定义在 `unlit3d_examples/src/scenes/mod.rs`，不是库 API：`TEST_FORMAT = Rgba8UnormSrgb`、`TEST_SIZE = (256, 192)`、`BASELINE_SIZE = (960, 720)`、`SEQUENCE_STEP = 0.5` |
| 相机基线 | 眼 `(0.0, 1.2, 3.2)`、目标 `(0.0, 0.2, 0.0)`、fov 60°、near 0.1 |
| 测试 | `cargo xtask test`（`nextest` + `cargo test --doc`）；单 crate `cargo nextest run -p <crate> --all-features` |
| 快照目录 | `target/snapshot-mismatches`、`-webgl2`、`-wasm`；单跑由 `UNLIT3D_SNAPSHOT_MISMATCH_DIR` 指定 |
| 格式 | 先 `cargo clippy` 再 `cargo fmt` |