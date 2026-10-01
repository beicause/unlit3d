English | [简体中文](https://github.com/beicause/unlit3d/blob/main/crates/unlit3d/README.zh-CN.md)

# unlit3d

The high-level rendering API: the ECS-integrated layer that turns a world of
component-carrying entities into GPU draw commands every frame. It bridges
[`unlit_ecs`] and [`unlit_wgpu`], and keeps both as direct dependencies — their
items are reached through their own paths, and the ones most callers need are
re-exported by [`prelude`].

Like the rest of the workspace it is at an **early stage of development**; APIs
change freely. It stays close to raw wgpu: the renderer uses [`wgpu`] resources
directly, and you are expected to know WebGPU to use it well.

## Features

| Feature | Default | Provides |
|---------|---------|----------|
| `ui` | yes | the `ui` module (an egui overlay drawn as a frame source) and the `unlit_wgpu` egui backend it draws with |
| `winit` | yes | the `winit` module: `WindowSurface`, which presents a [`Renderer`](renderer::Renderer) into a window's swap chain, and the winit input translation |
| `gltf` | no | the `gltf` module: load a glTF document into an `UnlitGltf`, patch its images, materials and meshes into another world's [`MeshSource`](mesh_source::MeshSource), and spawn the entities that draw them |

With `--no-default-features` the crate keeps the ECS components, the frame
sources, the mesh path, the pipeline abstraction and the portable [`input`]
module — none of which depend on egui or winit. Enabling `gltf` adds the glTF
loader on top of the same ECS and mesh path; see below.

## What this layer is for

It treats itself as part of a game engine, so it is built to integrate with
other features — physics, audio — and to be extended:

- Extensibility and portability come before convenience: public API prefers
  `dyn trait` over static dispatch, and multi-threading is considered to a
  degree rather than assumed away.
- The common path is easy: CPU-side scene data synchronizes into GPU buffers and
  renders without the caller managing an upload.
- Low-level control stays available: a caller can bypass the CPU-side data and
  update GPU buffers directly.
- Culling is CPU frustum culling.

## The frame model

A frame is **not** "one main scene plus extras". [`Renderer`](renderer::Renderer) draws nothing
itself: it owns the frame's render target and records the frame sources it is
given, in the order each source declares through
[`FrameOrder`](source::FrameOrder).

- Each source builds its own [`Scene`](unlit_wgpu::scene::Scene) in `build_scene`, then the
  renderer records every scene in order into one pass opened over the target's
  attachments. A frame is therefore one encoder and one submission.
- The built-in mesh rendering is one source, [`MeshSource`](mesh_source::MeshSource), and it has no more
  privilege than a caller's own. There is no mesh-specific field or draw path in
  the renderer.
- The GPU state — the [`wgpu::Device`], the [`wgpu::Queue`] and the
  [`ResourceGraph`](unlit_wgpu::resources::ResourceGraph) — lives in the world as
  resource components, addressed by a [`RenderContext`](source::RenderContext).
  [`spawn_context`](source::spawn_context) spawns it and returns the addresses.
  It also takes the frame's
  [`DeviceCapabilities`](unlit_wgpu::capabilities::DeviceCapabilities), which
  the caller derives from the adapter — what the device cannot report about
  itself travels with the frame rather than being rediscovered per source.
- **Build and record are two phases.** A source registers resources in the graph
  and stages uploads during `build_scene`; recording only reads the scenes it
  already produced, so no borrow conflict ever arises between a source's own
  state and the graph.
- **The graph is maintained once per frame, in the build phase.** `replace` and
  `remove` only mark; the source calls `ResourceGraph::maintain` exactly once,
  after its uploads and before it assembles the frame, so removals are dropped,
  orphans collected and dirty bind groups rebuilt in one pass. A frame that
  draws nothing still maintains, and the paths outside the scene — a surface
  resize, a swap-chain acquire — maintain explicitly rather than holding the
  old resources until the next scene build.
- A source's later GPU resources are its own to release; [`despawn_source`](source::despawn_source)
  queues the release before despawning, because the graph cannot notice an
  entity going away.

**Each source carries its own state and its own `Scene`, reusable across
frames.** egui's `Context`, font atlas and the UI's own UBO belong to the UI
source; the 3D pipelines, mesh pools, metadata and culling caches belong to the
mesh source. The "main 3D scene" is just the scene the first recorded source
produced, not a special case of the renderer.

<details>
<summary>Why the context and the sources are world members rather than renderer fields</summary>

This is not "taking the renderer apart" but giving frame sources and third-party
sources exactly equal standing: adding or removing a source is
spawning/despawning an entity, and a source uses the shared context directly in
its own build phase without the renderer passing it along.

- A resource reference is an entity reference, used through the `Entity` handle
  the caller keeps.
- Frame-level context is therefore passed "by entity reference", not "by
  threading borrow handles down the stack", so sources do not conflict over
  borrows just because they share one context.
- **The reverse trade-off**: were the context private to the renderer, a source
  wanting to hold both "its own mutable borrow" and "a mutable borrow of the
  resource graph" would have only two ways out: split the renderer into
  dedicated entry points, or push the conflict to a runtime panic with interior
  mutability. The former grows a borrow-checker-dodging function at every call
  site, and the latter trades convenience for crashes. Making both the context
  and the sources members of the world means the conflict never arises, so no
  dedicated entry point is needed for it.
- **A source releases its own GPU resources explicitly**, which avoids the
  complexity of releasing resources automatically by reference count. The nodes
  a source registers in the resource graph are not reclaimed when the entity is
  destroyed, so removing a source must release them explicitly.

**Building and recording are two phases.** The build phase has exclusive access
to the resource graph to register resources and to write the data this frame
will upload; the recording phase only reads each source's own `Scene` and never
touches the resource graph. Merging them into one phase would force reading the
resource graph before producing the `Scene`, leaving only interior mutability to
defer the borrow conflict to runtime — so the two phases stay, in exchange for a
compile-time guarantee.

</details>

<details>
<summary>Why a source declares its order instead of relying on creation order</summary>

The order is a required method, and [`FrameOrder`](source::FrameOrder) does not
implement `Default`. With a default value, "forgot to declare the order" and
"really meant to keep creation order" become indistinguishable, and the ordering
intent turns implicit again. Sources with equal order are logged and warned
about, and a source's order has to be an index the source carries explicitly: it
cannot borrow an entity's row order within the archetype, because that row order
is not guaranteed stable and is disturbed when components are removed.

The ordering semantics a [`Scene`](unlit_wgpu::scene::Scene) already has
internally (non-z-sorted first, then by pipeline and by material/depth) are
unchanged; the ordering among sources is not a hard constraint forced by
pass-level state — a scene carries the pass state it needs and resets it while
recording — but a need of semantics like "UI composites over 3D".

</details>

## Components

A renderable entity carries a [`GpuMesh`](components::GpuMesh),
[`GpuMaterial`](components::GpuMaterial) and
[`GpuRenderPipeline`](components::GpuRenderPipeline). A
[`GpuRenderPipeline`](components::GpuRenderPipeline) carries a *key*, not a compiled
pipeline: which concrete pipeline an entity needs depends on the frame's render
target, the mesh's vertex layout and — for a strip topology, whose pipeline
must declare the width of the index buffer it binds — the mesh's index format,
none of them known at spawn time. A *family*
closes that gap — it pairs a `Variants` cache with a `Specializer` and a
[`RenderPipelineFactory`](pipeline::RenderPipelineFactory), and resolves one key to a
concrete pipeline per frame.

[`GpuMesh`](components::GpuMesh) is deliberately small: it holds only the fields
a per-entity walk reads while culling and resolving, with the buffers a draw
binds behind a shared [`MeshParts`](components::MeshParts) handle. Culling and
resolving visit every entity, and a column is contiguous, so walking it pulls
whole cache lines through the cache while only the cull and resolve fields are
used; a mesh carrying its buffers inline would drag them through every walk for
the sake of a few bytes of bounds. The split is by read frequency, not by kind,
so a new field belongs on the side that reads it.

[`MeshSource::register_unlit_family`](mesh_source::MeshSource::register_unlit_family)
registers the built-in unlit family;
[`MeshSource::register_family`](mesh_source::MeshSource::register_family)
registers a caller's own, which is the same route the built-in one takes. Other
components: [`Transform`](components::Transform), [`Camera`](components::Camera),
[`RenderLoadOps`](components::RenderLoadOps),
[`InstanceColor`](components::InstanceColor) and the
[`ZSortedDrawing`](components::ZSortedDrawing) marker.

A frame is drawn through the first **active** [`Camera`](components::Camera) in
the world — the renderer skips an entity whose
[`Camera::active`](components::Camera::active) is `false`. A world can therefore
hold several cameras and switch between them by toggling that flag per frame;
with no active camera the frame is cleared and nothing is drawn.

<details>
<summary>Why a family sits between the entity and the pipeline cache</summary>

A family connects "what the entity wants" to `unlit_wgpu`'s variant cache:

- [`RenderPipelineKey`](pipeline::RenderPipelineKey) both locates the family
  (registration and lookup are by key type) and supplies the blueprint
  (`base_descriptor`). Because the blueprint travels with the component, one
  family can serve entities that start from different blueprints; and because
  the key is the component itself, the renderer never has to hand out a family
  handle.
- [`DrawKey`](pipeline::DrawKey) carries the dimensions the entity's own key
  does not express, those tied to this particular draw: the render target, the
  mesh's vertex layout and the format of the mesh's index buffer. The index
  format is there because a strip topology's pipeline has to declare the width
  its draw binds, and only the mesh knows it — the source picks the narrowest
  format a mesh's vertex count fits, and widens it while baking in a pool
  offset on a device without `base_vertex`. It combines with the entity's own
  key into `Specializer::Key`, and the way they combine (`From`) is the
  family's own decision. **Which dimensions to specialize on is therefore the
  family's freedom**: the built-in unlit family specializes on options plus
  target plus vertex layout plus index format, and a custom family can
  specialize on anything.
- The blueprint is **lazily evaluated**: it is asked of the key only on a cache
  miss, when a compilation is actually about to happen.
- Once the family has compiled a pipeline,
  [`RenderPipelineFactory`](pipeline::RenderPipelineFactory) turns it into the
  [`RegisteredRenderPipeline`](pipeline::RegisteredRenderPipeline) the renderer
  registers — the compiled pipeline, the three bind-group layouts and the global
  group's rebuild recipe. It and `unlit_wgpu`'s
  [`RenderPipelineDesc`](unlit_wgpu::specialize::RenderPipelineDesc) are an
  **output/input** pair, with one compilation between them.

`unlit3d` deals only in render pipelines, so its pipeline types all say
`Render` explicitly ([`GpuRenderPipeline`](components::GpuRenderPipeline),
[`RenderPipelineKey`](pipeline::RenderPipelineKey),
[`RenderPipelineId`](pipeline::RenderPipelineId),
[`RenderPipelineFactory`](pipeline::RenderPipelineFactory),
[`RegisteredRenderPipeline`](pipeline::RegisteredRenderPipeline)).

</details>

## Automatic instancing

Entries are sorted so that neighbours share a pipeline and bind groups, then each
run of neighbours whose draw state is exactly equal — pipeline, bind groups,
buffers and geometry segment — collapses into one instanced draw whose instance
range covers the whole run in visible order. Recording costs a command and a
state re-bind, so an entity that shares a mesh with the one before it is nearly
free.

<details>
<summary>Why merging is always safe for opaque draws, and never for z-sorted ones</summary>

The instance range is what keeps a merged draw correct: instance-stepped
attributes are fetched at the instance's ordinal, and the instance buffer is
packed in visible order, so instances `a..b` read exactly the records the
separate draws would have read. Per-instance data — the transform, base color
and pose base — lives in that stream, so merging changes nothing any instance
reads.

Opaque draws are depth-tested with blending off, so their order is not
observable and merging them is safe. Z-sorted entries never merge, with each
other or with anything else: they are blended back-to-front, so the order they
are drawn in *is* the result, and a merged draw would rasterize its instances in
record order instead.

</details>

## The unlit pipeline

The built-in unlit pipeline is registered as an ordinary family, and the shader
variant an entity uses contains exactly the channels its mesh has. Nothing about
it is privileged: a caller's own family is registered through the same
[`MeshSource::register_family`](mesh_source::MeshSource::register_family) call.

<details>
<summary>What the built-in variant supports, and where pose data lives</summary>

- The variant's flags cover position, UV, vertex color, per-instance transform
  and color, base-color texture, skinning and morph targets. The pipeline is
  then specialized for the frame's target by `unlit_wgpu`'s
  [`SurfaceSpecializer`](unlit_wgpu::specialize::SurfaceSpecializer).
- Joint matrices and morph weights are **not** in the mesh's bind group. Meshes
  and instances are many-to-one: the same mesh can be drawn by several entities,
  and each entity's pose usually differs. A bind group is bound per mesh and
  cannot express per-instance state, and building one bind group per instance is
  no better — that amounts to rebuilding bind groups per instance per frame.
  So the pose is split in two, each half going where it belongs:
  - **The data goes into a frame-wide shared SSBO**, bound in the global group.
    Every visible instance's joint matrices are packed into one array and its
    morph weights into another.
  - **The locating information goes into the instance stream.** Each instance's
    per-instance record carries a pose base (`Uint32x2`: the joint-matrix base
    and the weight base), which the shader uses to find its own slice.
    Per-instance-stepped attributes are addressed by instance index, so merging
    into an instanced draw changes nothing any instance reads — several
    instances of one mesh can fold into a single draw while each keeps its own
    pose, which is exactly what makes automatic instancing possible.
- At this level the pose is a component on a separate entity, and the mesh
  entity references it through two separate reference components
  ([`SkinBinding`](components::SkinBinding) and
  [`MorphBinding`](components::MorphBinding)). This follows the same line as
  "a resource reference is an entity reference" and brings two direct benefits:
  - **Sharing means sharing one entity**: several meshes referencing one pose
    entity share one pose, and changing it moves them all; to keep them
    independent, reference different entities. Both are the same mechanism and
    need no extra handle type.
  - **Changing a pose is one component write**: no GPU call at all, and the
    renderer packs and uploads it automatically next frame, consistent with
    per-frame upload being transparent to the caller.
- **The cost is that the references must be given**: when a mesh's vertex stream
  carries joint indices, or carries morph targets, its entity must carry the
  matching reference components, otherwise rendering panics rather than silently
  drawing with pose zero — a silent downgrade turns "forgot to associate" into a
  hard-to-notice visual bug. Another constraint is that the number of weights
  must match the mesh's target count, because the shader's loop bound is the
  mesh's own target count and an out-of-bounds storage read is a runtime error
  rather than a catchable panic.

</details>

## Loading glTF documents

The `gltf` module (cargo feature `gltf`, off by default) loads a
[glTF 2.0](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html) document —
`.glb` or `.gltf` — into an `UnlitGltf`. The document, its
buffers and its images are parsed and decoded eagerly in the constructor, and
the world-space transform of every node is computed up front, so `UnlitGltf` is
a *record* of the model, not a GPU resource: it owns no `World` and no
[`MeshSource`](mesh_source::MeshSource).

What it does is patch the world you already render:

- `insert_image` / `insert_material` / `insert_mesh` upload one image, material
  or mesh into the target source's resource graph and return a handle; the
  batch versions (`insert_images`, `insert_materials`, `insert_meshes`) upload
  everything the document has, aligned with the document's own indices.
- `unload_*` removes exactly what its handles name — the batch versions take a
  slice of handles, and a material must be unloaded before the image it
  samples, because the texture cannot outlive the material that depends on it.
  Nothing is reclaimed implicitly.
- `spawn_node` / `spawn_default_scene` spawn the entities that draw the node's
  mesh (or every node reachable from the default scene): one entity per
  primitive, each carrying the node's world-space [`Transform`](components::Transform), the uploaded
  [`GpuMesh`](components::GpuMesh), the [`UnlitPipeline`](components::UnlitPipeline)
  for the mesh, an [`InstanceColor`](components::InstanceColor) tinted with the
  material's base-color factor, and — when the mesh reads a base-color texture —
  the matching [`GpuMaterial`](components::GpuMaterial). A primitive whose
  material is `alphaMode: BLEND` is also marked
  [`ZSortedDrawing`](components::ZSortedDrawing), so the renderer composites it
  back-to-front after the opaque geometry. A `alphaMode: MASK` material instead
  draws binary coverage: the fragment shader discards every fragment whose alpha
  falls below the material's `alphaCutoff` (0.5 when the document leaves it
  out), so the pipeline carries `UnlitFlags::ALPHA_CUTOFF`, the material binds
  its cutoff as a uniform, and the primitive needs neither blending nor a sort.
- A primitive carrying both `JOINTS_0` and `WEIGHTS_0` is uploaded with its
  joint stream and drawn skinned: `spawn_node`/`spawn_default_scene` create a
  [`SkinPose`](components::SkinPose) entity for the node's skin and put a
  [`SkinBinding`](components::SkinBinding) on the mesh, so writing that pose's
  matrices is how a caller animates the skeleton.
  `UnlitGltf::skin_pose` builds the document's rest pose — one matrix per
  joint, from the joint nodes' world transforms and the skin's inverse bind
  matrices — as a starting point, and `UnlitGltf::skin_joint_count` reports how
  many joints a node's skin has.
- A primitive whose mesh declares a morph target that displaces positions is
  uploaded with those displacements and drawn morphed: `spawn_node` /
  `spawn_default_scene` create a [`MorphWeights`](components::MorphWeights) entity for the node and
  put a [`MorphBinding`](components::MorphBinding) on the mesh, so writing that entity's weights is
  how a caller morphs the mesh. `UnlitGltf::morph_weights` returns the node's
  starting weights — the node's own `weights` when it states them, the mesh's
  otherwise, and zero for a target left unweighted — padded or truncated to the
  number of targets that displace positions, which is the length the renderer
  requires. A target that displaces only normals or tangents is not counted and
  not uploaded, so a mesh whose every target displaces nothing this loader
  reads draws undeformed rather than through the morph path.
- A primitive whose material is `doubleSided` is drawn from either side: the
  pipeline variant drops its cull mode, so the back faces the single-sided
  variant would discard are rasterized instead. Nothing else about the variant
  changes, because the unlit fragment shader reads no normal — there is no
  back-face normal to reverse and no lighting equation to evaluate.
- A document's animations are sampled rather than played:
  `UnlitGltf::animation_count`, `UnlitGltf::animation_name` and
  `UnlitGltf::animation_duration` describe the clips it carries, and
  `UnlitGltf::apply_animation(world, clip, time, &spawned)` evaluates one at a
  chosen time. It samples each channel — `STEP`, `LINEAR` or `CUBICSPLINE`, with
  rotations slerped — into the animated nodes' local transforms, recomposes the
  world matrices down the node hierarchy, and writes the result where the
  spawned entities read it: each animated node's [`Transform`](components::Transform) (and
  its descendants', which inherit the change), every [`SkinPose`](components::SkinPose) whose mesh
  follows a moved joint, and every [`MorphWeights`](components::MorphWeights) a weight channel names.
  It takes `&World`, so a caller drives it from wherever it already has one, and
  it touches nothing the passed `&[GltfNode]` does not name.

What loads: positions, UVs, vertex colors, joints, weights and indices;
base-color textures and the materials that sample them; blended
(`alphaMode: BLEND`), cut-off (`alphaMode: MASK`) and double-sided materials;
skinned primitives; positional morph targets; node hierarchies, accumulated
into world transforms; and the animations that drive them. What does not yet:
normal and tangent morphs, normals and tangents.
The pipeline key a mesh is uploaded with is always derived from the primitive's
own attributes (see `UnlitGltf::pipeline_key`),
so the variant it draws with never asks for a stream the mesh does not have.

An image is uploaded in the GPU format closest to the pixels the loader decoded,
so textures keep their channels and precision instead of all being widened to
RGBA8: 8-bit layouts upload as `R8Unorm`/`Rg8Unorm`/`Rgba8UnormSrgb`, 16-bit
ones as the half-float formats of the same width, and 32-bit float ones as
`Rgba32Float`. Three-channel layouts widen by one, since neither an sRGB nor a
float format comes in three channels. Two consequences are worth knowing: a
grayscale texture carries its luminance in the red channel — WebGPU has neither
a luminance format nor a component swizzle — so the unlit shader's
`BASE_COLOR_LUMINANCE` and `BASE_COLOR_LUMINANCE_ALPHA` flags expand it to
RGB(A) and decode the luminance from sRGB, which `UnlitGltf::pipeline_key` sets
for exactly those uploads; and `Rgba32Float` is `unfilterable-float` on devices
without `Features::FLOAT32_FILTERABLE`, in which case the material binds a
non-filtering sampler and `UnlitOptions::texture_filtering` specializes the
bind-group layout to match.

## UI

`ui::UiPanel` is itself a behaviour component holding a closure, so
an interface is an entity — a frame can hold as many panels as entities, and the
`ui::UiSource` driver runs whatever panels the world carries, in
query order.

<details>
<summary>Why the panel is a component, and the unit convention that is easy to get wrong</summary>

**The interface is the behaviour component, not a closure field on the source.**
This matches the "the caller is the system" convention: the source is the driver
and decides which panels to call and in what order; a panel that wants to keep
state puts it in its own sibling components (a behaviour component is borrowed
while it runs, so it cannot re-enter a borrow of itself); and a panel receives
`&World`, so it can read and write components and can `queue()` structural
changes. Several panels are therefore several entities, added and removed as
needed, and third parties can define their own panel-like components for the
source to select — the source itself need not know which interfaces exist.

**The unit convention is the easiest thing to get wrong in this API**: egui's
vertices and clip rectangles use logical points while window event coordinates
are physical pixels. So the projection uses points (the physical size divided by
the scale factor) while the scissor must multiply by the scale factor — the two
units are opposite. On top of that, egui calls a panel closure **several times**
for multi-pass layout, so a panel must be idempotent or decide what to do based
on the pass number; that is egui's existing semantics, the same in every
backend.

UI itself needs only a color attachment and no depth attachment, but a
pipeline's depth state must **exactly match** the pass's attachments (wgpu
validates strictly by format, ignoring the write and compare settings). In a
pass that has a depth attachment — UI and meshes sharing one pass is the common
case — the UI pipeline must still declare the **same** depth format and avoid
disturbing the meshes' depth only by not writing depth and not depth-testing:
"needs no depth" is not the same as "declares no depth". "The target has no
depth attachment" is also legal, which is why a pipeline's depth state is
optional; that optional depth state serves the case where the target itself has
no depth attachment, and then every pipeline in the pass must declare no depth,
which is not specific to UI. This matches egui's official backend: by default it
carries no depth state, but when given a depth format it still builds a state
with that same format, no depth writes, and a compare function of `Always`.

</details>

## Example

The frame skeleton, with the `device`/`queue` pair passed in. `spawn_context`
puts the GPU state in the world, `MeshSource` is mounted as a source, and
`Renderer` is the frame driver:

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::resources::{ResourceGraph, TextureExt};

let (device, queue) =
    wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
let mut world = World::new();

// 1. Spawn the frame's GPU context, the built-in mesh source and the
//    frame driver, and register the built-in unlit family.
// `DeviceCapabilities` carries what the device cannot report about itself
// (its `base_vertex` support). Derive it from the adapter when one is at
// hand; `default()` is the WebGPU baseline, which is also what `noop` is.
let ctx = spawn_context(
    &mut world,
    device,
    queue,
    ResourceGraph::new(),
    DeviceCapabilities::default(),
);
let mut mesh_source = MeshSource::new(&world, ctx);
mesh_source.register_unlit_family(&world);
let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(&world)));
let source = spawn_source(&mut world, mesh_source);
let renderer = world.spawn((Renderer::new(ctx),));

// 2. Allocate geometry and a material through the mesh source.
let (mesh, material) = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        let positions = [[0.0; 3]; 3];
        let uvs = [[0.0; 2]; 3];
        let colors = [[255u8; 4]; 3];
        let indices = [0u32, 1, 2];
        let mesh = source.allocate_unlit_mesh(
            &world,
            &key,
            UnlitMeshDesc {
                positions: &positions,
                uvs: Some(&uvs),
                colors: Some(&colors),
                indices: Some(&indices),
                ..Default::default()
            },
        );
        let device = source.device(&world);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("example::texture"),
            size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // The material reads a view of the texture and a sampler, both of
        // which the graph keeps for it: only ids cross the boundary.
        let view = source.register_texture_and_default_view(&world, texture).1;
        let sampler = source.register_sampler(&world, None);
        let material = source.allocate_unlit_material(&world, &key, view, sampler);
        (mesh, material)
    })
    .unwrap();

// 3. Spawn a renderable entity, carrying the unlit key.
world.spawn((
    Transform::default(),
    mesh,
    material.unwrap(),
    UnlitPipeline::new(key),
));

// 4. Bind a render target from the frame's resource graph and render one
//    frame (uses the noop device, so it produces a valid command buffer
//    without touching a GPU).
let ft = create_render_target(
    &world.get::<wgpu::Device>(ctx.device).unwrap(),
    wgpu::TextureFormat::Rgba8UnormSrgb,
    1280, 720, 1,
);
let (color_view, depth_view) = world
    .with_mut::<Source, _>(source, |source| {
        let source = source.as_mut::<MeshSource>().unwrap();
        let color_view = source.register_texture_and_default_view(&world, ft.color).1;
        let depth_view = MeshSource::graph(&world, ctx)
            .insert_strong(
                TextureExt::create_view(
                    &ft.depth,
                    &wgpu::TextureViewDescriptor::default(),
                ),
                None,
            );
        (color_view, depth_view)
    })
    .unwrap();
world
    .with_mut::<Renderer, _>(renderer, |r| {
        r.set_render_target(&world, Some(color_view), Some(depth_view), None);
        r.render(&world);
    })
    .unwrap();
```

A UI panel is a behaviour component, so mounting an interface is an ordinary
spawn:

```rust
# #[cfg(feature = "ui")]
# {
use unlit3d::prelude::*;

let mut world = World::new();
world.spawn((UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),));
# }
```

## Input

[`input`] is the portable half of input handling: event types and the behaviour
components that react to them, depending on neither winit nor egui. A frame
feeds events into the [`InputState`](input::InputState) resource, runs
[`dispatch_input`](input::dispatch_input) to drive the behaviours, then calls
[`InputState::clear_events`](input::InputState::clear_events) once every
consumer has read them — the events are traversed read-only, never taken,
because more than one consumer sees the same frame. Translation from a
windowing library's events lives behind the corresponding feature:
`input::winit::WinitInput` forwards `WindowEvent`s, and `ui::convert` turns the
crate's events into egui's.

<details>
<summary>The naming rule, and why dispatch is explicit</summary>

- **Event types depend on neither winit nor egui**: the core types stand alone,
  and the winit and egui conversions sit behind their respective features. Game
  logic and UI thus share one event stream and need no two sets of events.
- **Event callbacks are behaviour components**, isomorphic to the ECS's existing
  behaviour-component paradigm: all matching behaviour components are called per
  event category. `OnInput` receives every event, while
  `OnKey`/`OnMouse`/`OnPointer`/`OnTouch`/`OnText`/`OnIme` each receive one
  category. The mouse family is always named `Mouse`
  (`MouseEvent`/`MouseButton`/`MouseButtons`/`OnMouse`), while `Pointer` means
  specifically a **device-independent "point input"**: both mouse and touch
  produce `PointerEvent`, so the same drag behavior works on desktop and
  touchscreen; `Touch` keeps what is unique to a finger (touch id, pressure).
  Conversely, touch **does not** set mouse buttons, and the button state in
  `InputState` is mouse-only.
- **`InputState` holds "the events that arrived since it was last cleared" plus
  the state the events left behind** (modifiers, pointer position and the
  pointers pressed, cursor, buttons pressed, focus, window size and scale). A
  pointer's pressed state is recorded per "contact point"
  (`PointerContact`, identified by kind and id), so lifting one finger out of
  several does not misread as the gesture ending.
- **Dispatch is called explicitly by the caller in the frame loop**, not done
  automatically inside `render`. The reason: structural changes queued inside a
  callback need `&mut world` to land, while `render` only gets `&World`;
  automatic dispatch would defer a callback's queued changes to the next
  external `apply`, and that delay would be invisible to the user. Explicit
  dispatch also gives the caller control of the timing and order, consistent
  with "the caller is the system".
- Dispatch **traverses** the behaviour components directly without collecting
  entities first: callbacks borrow different cells of different entities, so
  they do not conflict. The real limits come from the ECS's own deliberate
  trade-offs — a callback cannot re-enter a borrow of its own component, nor
  re-enter a dispatch of the same kind; state is kept in sibling components
  instead.

UI's **capture** of input (whether it wants to monopolize the pointer/keyboard)
needs the previous frame's result by egui's semantics: the source writes these
two flags back into the world, and game logic can read them to decide whether to
respond. This iteration provides the data only, with no automatic interception.

</details>

## Tests

```text
cargo xtask test                # the whole workspace
cargo nextest run -p unlit3d    # just this crate
```

The GPU integration tests render scenes into offscreen targets and inspect the
pixels that come back. The multi-frame snapshot coverage of those scenes lives
in [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md),
whose `tests/gpu_scenes.rs` compares them against the SSIMULACRA2 snapshots under
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md).
Clone the submodule here with `git submodule update --init --checkout`, which is
what gets past its `update = none`; re-bless intentional
changes with `SNAPSHOT_UPDATE=1 cargo nextest run -p unlit3d_examples` and review
the image diff. Where each test layer sits
across the workspace, and what CI runs, is in the
[root README](https://github.com/beicause/unlit3d/blob/main/README.md#tests-and-benchmarks).

## See also

- [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.md)
  — the renderer underneath.
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md)
  — the world the components live in.
- [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md)
  — a runnable windowed program built on this API.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
