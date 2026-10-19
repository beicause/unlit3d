//! The example's own scene: a spinning, textured cube with two egui panels.
//!
//! This is the scene the windowed example always showed. Its snapshot test
//! is the workspace's CI check against `spin_cube.webp`: 30 frames at the
//! default window size, so a rendering regression fails the build.

use super::{BASELINE_SIZE, SceneControl, SceneDef, SceneOptions};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit3d::prelude::*;
use unlit3d::ui::{UiSource, egui};

/// The number of samples every frame is rendered with.
pub const SAMPLE_COUNT: u32 = 4;

/// How fast the cube spins, in radians per second.
const SPIN: f32 = 0.8;

/// How far from the target the camera orbits, in world units.
const ORBIT_RADIUS: f32 = 3.4;

/// How high above the horizon the camera starts, in radians.
const ORBIT_ELEVATION: f32 = 0.5;

/// The point the camera looks at, in world units.
const ORBIT_TARGET: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 0.0);

/// The scene: a spinning cube at the example's default window size, whose
/// snapshot test verifies `spin_cube.webp`.
pub static SCENE: SceneDef = SceneDef {
    id: "spin_cube",
    title: "Spinning cube",
    description: "the example's own scene: a textured cube with two panels",
    size: (960, 720),
    baseline: Some(BASELINE_SIZE),
    frames: 30,
    step_seconds: None,
    samples: SAMPLE_COUNT,
    depth: true,
    ui: true,
    reproducible_ui: true,
    build,
};

/// Build the cube scene's world content.
fn build(
    world: &mut World,
    context: RenderContext,
    ui: Option<Entity>,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let SceneOptions { ui: panels, .. } = options;

    // The built-in mesh source and the frame driver, as resource entities.
    let mut source = MeshSource::new(&world.root_view(), context);
    source.register_unlit_family(&world.root_view());
    let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(&world.root_view())));
    let source_entity = world.root_view_mut().spawn((Source::new(source),));

    // Geometry, its base-color texture and its material, all allocated
    // through the mesh source so they live in the frame's resource graph.
    let root = world.root_view();
    let (mesh, material) = root
        .with_mut::<Source, _>(source_entity, |source| {
            let source = source
                .as_mut::<MeshSource>()
                .expect("the source entity carries a MeshSource");
            let (positions, uvs, colors, indices) = super::cube();
            let mesh = source.allocate_unlit_mesh(
                &root,
                &key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    ..Default::default()
                },
            );
            let texture = checkerboard(&source.device(&root), &source.queue(&root), 64);
            let view = source.register_texture_and_default_view(&root, texture).1;
            // Linear filtering: the checkerboard is a high-frequency pattern,
            // and point sampling it under minification aliases into moire on
            // the faces the camera sees at a glancing angle.
            let sampler = source.register_sampler(
                &root,
                Some(wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::MipmapFilterMode::Linear,
                    anisotropy_clamp: 4,
                    ..Default::default()
                }),
            );
            let material = source.allocate_unlit_material(&root, &key, view, sampler);
            (mesh, material)
        })
        .expect("the source entity exists");

    // The camera the frame is viewed from, and the cube itself. A frame with
    // no RenderLoadOps component is opened with the defaults, so the pass
    // clears color and depth on its own.
    super::spawn_in(
        world,
        source_entity,
        (orbit_camera(size, 0.6, ORBIT_ELEVATION),),
    );
    // The panel is drawn by the UI source and the cube by the mesh source, and
    // one source's scope reaches nothing of the other's. The state both of them
    // touch therefore lives in the shared subtree, which every view reaches: the
    // spin the panel drives, the frame count it reads, the drag the pointer
    // behaviour follows and the orbit it writes.
    let (spin, orbit) = {
        let mut shared = world
            .view_mut(world.shared())
            .expect("the shared scope exists");
        (
            shared.spawn((
                Spin {
                    radians_per_second: SPIN,
                    spinning: true,
                    angle: 0.0,
                },
                SpinReset(false),
                DragFrom(None),
                FrameCount(0),
            )),
            shared.spawn((CameraOrbit {
                azimuth: 0.6,
                elevation: ORBIT_ELEVATION,
            },)),
        )
    };
    // The cube is drawn by the mesh source, so it hangs in that source's scope
    // beside the camera.
    super::spawn_in(
        world,
        source_entity,
        (
            Transform::default(),
            mesh,
            material,
            UnlitPipeline::new(key),
        ),
    );

    if panels {
        let panel = UiPanel::new(move |world, _entity, ui| {
            // The id is set explicitly: `Window::new` derives it from the
            // title's `Atoms` text, whose hash differs from a plain string
            // id, so other panels could not find this window's rect.
            egui::Window::new("unlit3d")
                .id(egui::Id::new("unlit3d"))
                .show(ui.ctx(), |ui| {
                    let frames = world.get::<FrameCount>(spin).map_or(0, |frames| frames.0);
                    ui.label(format!("frame {frames}"));
                    ui.label("The cube spins behind this panel.");

                    let spinning = world.get::<Spin>(spin).is_some_and(|state| state.spinning);
                    let mut spinning_now = spinning;
                    if ui.checkbox(&mut spinning_now, "Spin").changed() {
                        let _ =
                            world.with_mut::<Spin, _>(spin, |state| state.spinning = spinning_now);
                    }

                    let mut speed = world
                        .get::<Spin>(spin)
                        .map_or(SPIN, |state| state.radians_per_second);
                    if ui
                        .add(egui::Slider::new(&mut speed, 0.0..=4.0).text("rad/s"))
                        .changed()
                    {
                        let _ = world.with_mut::<Spin, _>(spin, |state| {
                            state.radians_per_second = speed;
                        });
                    }

                    if ui.button("Reset the spin").clicked() {
                        let _ = world.with_mut::<SpinReset, _>(spin, |reset| reset.0 = true);
                    }

                    // The world's input state, read out under the panel's own
                    // controls: what makes the events visible next to the UI
                    // they also drive. One window, so no second title bar can
                    // ever sit on top of this one and be painted as the window
                    // the user is working in.
                    ui.separator();
                    let held = world
                        .query::<&InputEvents>()
                        .next()
                        .is_some_and(|(_, events)| events.with_state(|state| state.pointer_down));
                    ui.label(if held { "pointer: down" } else { "pointer: up" });
                    match world.get::<CameraOrbit>(orbit) {
                        Some(orbit) => ui.label(format!(
                            "azimuth {:.2}, elevation {:.2}",
                            orbit.azimuth, orbit.elevation
                        )),
                        None => ui.label("no camera state"),
                    };
                });
        });
        super::spawn_in(world, ui.expect("the UI source was mounted"), (panel,));
    }
    // The second panel shows the world's input state, which is what makes the
    // events visible next to the UI they also drive.
    // A key behaviour: space toggles the spin, and it reads the state the
    // panel's checkbox also writes.
    world
        .root_view_mut()
        .spawn((OnKey::new(move |world, _entity, key| {
            if key.pressed && !key.repeat && key.key == Key::Space {
                let _ = world
                    .root_view()
                    .with_mut::<Spin, _>(spin, |state| state.spinning = !state.spinning);
            }
        }),));

    // A pointer behaviour: dragging orbits the camera. It is mounted on
    // `OnPointer` rather than `OnMouse`, so the same drag works with a mouse
    // and with a finger on a touch screen.
    world
        .root_view_mut()
        .spawn((OnPointer::new(move |world, _entity, event| {
            let Some(position) = event.position else {
                if matches!(
                    event.action,
                    PointerAction::Released { .. } | PointerAction::Left
                ) {
                    let _ = world
                        .root_view()
                        .with_mut::<DragFrom, _>(spin, |drag| drag.0 = None);
                }
                return;
            };

            // A press or drag over one of the panels drives the panel, not the
            // camera: the UI's context knows what it claimed of the frame it laid
            // out, and a pointer behaviour yields to it. The claim is broad — it
            // is `true` whenever the pointer hangs over a panel, let alone drags
            // one — so a gesture that started on the scene keeps working even when
            // it sweeps over a panel. The contact is dropped here too, so a press
            // that ends over a UI never leaves a drag behind for the scene.
            let over_ui = world.root_view().query::<&Source>().any(|(_, source)| {
                source
                    .as_ref::<UiSource>()
                    .is_some_and(|ui| ui.context().egui_wants_pointer_input())
            });
            if over_ui {
                let _ = world
                    .root_view()
                    .with_mut::<DragFrom, _>(spin, |drag| drag.0 = None);
                return;
            }

            match event.action {
                PointerAction::Pressed => {
                    let _ = world.root_view().with_mut::<DragFrom, _>(spin, |drag| {
                        drag.0 = Some(PointerContact {
                            kind: event.kind,
                            id: event.id,
                            position,
                        });
                    });
                }
                PointerAction::Moved => {
                    let previous = world.root_view().with_mut::<DragFrom, _>(spin, |drag| {
                        let tracked = drag.0.as_mut()?;
                        if (tracked.kind, tracked.id) != (event.kind, event.id) {
                            return None;
                        }
                        Some(std::mem::replace(&mut tracked.position, position))
                    });
                    let Some(Some(previous)) = previous else {
                        return;
                    };
                    const RADIANS_PER_PIXEL: f32 = 0.01;
                    let (dx, dy) = (position[0] - previous[0], position[1] - previous[1]);
                    let _ = world.root_view().with_mut::<CameraOrbit, _>(orbit, |drag| {
                        drag.azimuth -= dx * RADIANS_PER_PIXEL;
                        drag.elevation = (drag.elevation + dy * RADIANS_PER_PIXEL).clamp(-1.4, 1.4);
                    });
                }
                PointerAction::Released { .. } | PointerAction::Left => {
                    let _ = world
                        .root_view()
                        .with_mut::<DragFrom, _>(spin, |drag| drag.0 = None);
                }
                PointerAction::Zoom(_) | PointerAction::Rotate(_) => {}
            }
        }),));

    SceneControl {
        advance: Box::new(move |world, _frame, delta_time, frame_size| {
            // The spin and the orbit live in the shared subtree, which the root
            // view reaches along with everything else.
            let root = world.root_view();

            // The panel's button leaves its press as state; consume it here,
            // once.
            for (_, mut reset) in root.query::<&mut SpinReset>() {
                if core::mem::take(&mut reset.0) {
                    for (_, mut spin) in root.query::<&mut Spin>() {
                        spin.angle = 0.0;
                    }
                }
            }

            // The behaviour components run first: the world the renderer reads
            // is this frame's.
            for (_, mut frames) in root.query::<&mut FrameCount>() {
                frames.0 += 1;
            }
            for (_, mut spin) in root.query::<&mut Spin>() {
                if spin.spinning {
                    spin.angle += spin.radians_per_second * delta_time;
                }
            }

            // The cube and the camera are what the mesh source draws, so they
            // are read and written through its own scope.
            let view = world
                .view(source_entity)
                .expect("the source entity is alive");
            let angle = root
                .query::<&Spin>()
                .next()
                .map_or(0.0, |(_, spin)| spin.angle);
            for (_, mut transform) in view.query::<&mut Transform>() {
                transform.rotation = glam::Quat::from_rotation_y(angle);
            }

            // The camera follows the orbit a slider or a drag set — and the
            // frame's own size, so a window that changes shape re-aims it
            // instead of stretching the cube.
            let orbit = root.query::<&CameraOrbit>().next().map_or(
                CameraOrbit {
                    azimuth: 0.6,
                    elevation: ORBIT_ELEVATION,
                },
                |(_, orbit)| CameraOrbit {
                    azimuth: orbit.azimuth,
                    elevation: orbit.elevation,
                },
            );
            for (_, mut camera) in view.query::<&mut Camera>() {
                *camera = orbit_camera(frame_size, orbit.azimuth, orbit.elevation);
            }
        }),
        snapshot: Box::new(|frame| (frame == 29).then(|| "spin_cube.webp".to_owned())),
    }
}

/// Set by the panel's button, read once by the frame loop.
struct SpinReset(bool);

/// A behaviour component: the cube turns by `radians_per_second`.
///
/// `spinning` is a sibling the panel's checkbox writes, so the panel and the
/// spin never borrow the same component.
struct Spin {
    radians_per_second: f32,
    /// Whether the cube is currently turning.
    spinning: bool,
    /// The angle turned so far, advanced once per frame.
    angle: f32,
}

/// How far the camera has been dragged around the cube, in radians.
struct CameraOrbit {
    /// The azimuth the camera looks from.
    azimuth: f32,
    /// How far above the horizon it sits.
    elevation: f32,
}

/// The pointer the drag is following, and where it was at the previous move.
struct DragFrom(Option<PointerContact>);

/// Counts the frames drawn, for the panel's readout.
struct FrameCount(u64);

/// A camera orbiting [`ORBIT_TARGET`] at `azimuth` and `elevation`, in radians.
///
/// The projection comes from [`super::camera_looking_at`], so it follows the
/// frame's own aspect: the built-in pipeline compares depth with `Greater` and
/// clears to the far plane, so the projection is reverse-z infinite.
fn orbit_camera(size: (u32, u32), azimuth: f32, elevation: f32) -> Camera {
    let eye = ORBIT_TARGET
        + ORBIT_RADIUS
            * glam::Vec3::new(
                elevation.cos() * azimuth.sin(),
                elevation.sin(),
                elevation.cos() * azimuth.cos(),
            );
    super::camera_looking_at(eye, ORBIT_TARGET, size)
}

/// A `size` x `size` checkerboard base-color texture.
fn checkerboard(device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> wgpu::Texture {
    let mut texels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let value = if (x + y) % 2 == 0 { 235u8 } else { 60 };
            texels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("example::checkerboard"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    texture
}
