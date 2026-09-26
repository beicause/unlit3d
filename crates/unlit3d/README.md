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

With `--no-default-features` the crate keeps the ECS components, the frame
sources, the mesh path, the pipeline abstraction and the portable [`input`]
module — none of which depend on egui or winit.

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
- **Build and record are two phases.** A source registers resources in the graph
  and stages uploads during `build_scene`; recording only reads the scenes it
  already produced, so no borrow conflict ever arises between a source's own
  state and the graph.
- A source's later GPU resources are its own to release; [`despawn_source`](source::despawn_source)
  queues the release before despawning, because the graph cannot notice an
  entity going away.

## Components

A renderable entity carries a [`GpuMesh`](components::GpuMesh),
[`GpuMaterial`](components::GpuMaterial) and
[`GpuPipeline`](components::GpuPipeline). A
[`GpuPipeline`](components::GpuPipeline) carries a *key*, not a compiled
pipeline: which concrete pipeline an entity needs depends on the frame's render
target and the mesh's vertex layout, neither known at spawn time. A *family*
closes that gap — it pairs a `Variants` cache with a `Specializer` and a
[`PipelineFactory`](pipeline::PipelineFactory), and resolves one key to a
concrete pipeline per frame.

[`MeshSource::register_unlit_family`](mesh_source::MeshSource::register_unlit_family)
registers the built-in unlit family;
[`MeshSource::register_family`](mesh_source::MeshSource::register_family)
registers a caller's own, which is the same route the built-in one takes. Other
components: [`Transform`](components::Transform), [`Camera`](components::Camera),
[`RenderLoadOps`](components::RenderLoadOps),
[`InstanceColor`](components::InstanceColor) and the
[`ZSortedDrawing`](components::ZSortedDrawing) marker.

`ui::UiPanel` is itself a behaviour component holding a closure, so
an interface is an entity — a frame can hold as many panels as entities, and the
`ui::UiSource` driver runs whatever panels the world carries, in
query order.

## Example

The frame skeleton, with the `device`/`queue` pair passed in. `spawn_context`
puts the GPU state in the world, `MeshSource` is mounted as a source, and
`Renderer` is the frame driver:

```rust
use unlit3d::prelude::*;
use unlit_wgpu::pipeline::UnlitOptions;
use unlit_wgpu::resources::ResourceGraph;

let (device, queue) =
    wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
let mut world = LocalWorld::new();

// 1. Spawn the frame's GPU context, the built-in mesh source and the
//    frame driver, and register the built-in unlit family.
let ctx = spawn_context(&mut world, device, queue, ResourceGraph::new());
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
                ft.depth.create_view(&wgpu::TextureViewDescriptor::default()),
                &[],
            )
            .unwrap();
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
# fn main() {
use unlit3d::prelude::*;

let mut world = LocalWorld::new();
world.spawn((UiPanel::new(|_world, _entity, ui| {
    ui.label("hello");
}),));
# }
# #[cfg(not(feature = "ui"))]
# fn main() {}
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

## Tests

```text
cargo xtask test                # the whole workspace
cargo nextest run -p unlit3d    # just this crate
```

The GPU integration tests render scenes into offscreen targets and inspect the
pixels that come back. The multi-frame snapshot coverage of those scenes lives
in [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md),
whose headless path compares them against the SSIMULACRA2 snapshots under
[`unlit3d_asset_files`](https://github.com/beicause/unlit3d/blob/main/unlit3d_asset_files/README.md).
Clone the submodule with `git submodule update --init`; re-bless intentional
changes with `cargo run -p unlit3d_examples --features snapshot -- --headless
--scene all --update` and review the image diff.

## See also

- [`unlit_wgpu`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_wgpu/README.md)
  — the renderer underneath.
- [`unlit_ecs`](https://github.com/beicause/unlit3d/blob/main/crates/unlit_ecs/README.md)
  — the world the components live in.
- [`unlit3d_examples`](https://github.com/beicause/unlit3d/blob/main/unlit3d_examples/README.md)
  — a runnable windowed program built on this API.
- [Design document](https://github.com/beicause/unlit3d/blob/main/docs/DESIGN.md)
  — architecture and rationale (in Chinese).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
