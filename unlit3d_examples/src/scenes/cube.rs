//! The example's own scene: a spinning, textured cube with two egui panels.
//!
//! This is the scene the windowed example always showed. Its headless capture
//! is the workspace's CI check against `example.webp`: 30 frames at the
//! default window size, so a rendering regression fails the build.

use super::{SceneControl, SceneDef, SceneOptions};
use unlit3d::prelude::*;
use unlit3d::ui::egui;
use wgpu_unlit_render::pipeline::UnlitOptions;

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
/// headless capture verifies `example.webp`.
pub static SCENE: SceneDef = SceneDef {
    id: "cube",
    title: "Spinning cube",
    description: "the example's own scene: a textured cube with two panels",
    size: (960, 720),
    frames: 30,
    samples: SAMPLE_COUNT,
    depth: true,
    ui: true,
    reproducible_ui: true,
    build,
};

/// Build the cube scene's world content.
fn build(
    world: &mut LocalWorld,
    context: RenderContext,
    _renderer: Entity,
    size: (u32, u32),
    options: SceneOptions,
) -> SceneControl {
    let SceneOptions { ui, .. } = options;

    // The built-in mesh source and the frame driver, as resource entities.
    let mut source = MeshSource::new(world, context);
    source.register_unlit_family(world);
    let key = UnlitPipelineKey::new(UnlitOptions::standard(&source.device(world)));
    let source_entity = spawn_source(world, source);

    // Geometry, its base-color texture and its material, all allocated
    // through the mesh source so they live in the frame's resource graph.
    let (mesh, material) = world
        .with_mut::<Source, _>(source_entity, |source| {
            let source = source
                .as_mut::<MeshSource>()
                .expect("the source entity carries a MeshSource");
            let (positions, uvs, colors, indices) = super::cube();
            let mesh = source.allocate_unlit_mesh(
                world,
                &key,
                UnlitMeshDesc {
                    positions: &positions,
                    uvs: Some(&uvs),
                    colors: Some(&colors),
                    indices: Some(&indices),
                    ..Default::default()
                },
            );
            let texture = checkerboard(&source.device(world), &source.queue(world), 64);
            let view = source.register_texture_and_default_view(world, texture).1;
            // Linear filtering: the checkerboard is a high-frequency pattern,
            // and point sampling it under minification aliases into moire on
            // the faces the camera sees at a glancing angle.
            let sampler = source.register_sampler(
                world,
                Some(wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    mipmap_filter: wgpu::MipmapFilterMode::Linear,
                    anisotropy_clamp: 4,
                    ..Default::default()
                }),
            );
            let material = source
                .allocate_unlit_material(world, &key, view, sampler)
                .expect("the standard options read a base-color texture");
            (mesh, material)
        })
        .expect("the source entity exists");

    // The camera the frame is viewed from, and the cube itself. A frame with
    // no RenderLoadOps component is opened with the defaults, so the pass
    // clears color and depth on its own.
    let aspect = size.0 as f32 / size.1 as f32;
    let orbit = CameraOrbit {
        azimuth: 0.6,
        elevation: ORBIT_ELEVATION,
    };
    let camera = world.spawn((orbit_camera(aspect, orbit.azimuth, orbit.elevation), orbit));
    // The cube carries its spin and the orbit the panels drive, so one entity
    // owns everything the frame loop and the panels share.
    let cube = world.spawn((
        Transform::default(),
        Spin {
            radians_per_second: SPIN,
            spinning: true,
            angle: 0.0,
        },
        SpinReset(false),
        DragFrom(None),
        FrameCount(0),
        mesh,
        material,
        UnlitPipeline::new(key),
    ));

    if ui {
        world.spawn((UiPanel::new(move |world, _entity, ui| {
            egui::Window::new("unlit3d").show(ui.ctx(), |ui| {
                let frames = world.get::<FrameCount>(cube).map_or(0, |frames| frames.0);
                ui.label(format!("frame {frames}"));
                ui.label("The cube spins behind this panel.");

                let spinning = world.get::<Spin>(cube).is_some_and(|spin| spin.spinning);
                let mut spinning_now = spinning;
                if ui.checkbox(&mut spinning_now, "Spin").changed() {
                    let _ = world.with_mut::<Spin, _>(cube, |spin| spin.spinning = spinning_now);
                }

                let mut speed = world
                    .get::<Spin>(cube)
                    .map_or(SPIN, |spin| spin.radians_per_second);
                if ui
                    .add(egui::Slider::new(&mut speed, 0.0..=4.0).text("rad/s"))
                    .changed()
                {
                    let _ = world.with_mut::<Spin, _>(cube, |spin| spin.radians_per_second = speed);
                }

                if ui.button("Reset the spin").clicked() {
                    let _ = world.with_mut::<SpinReset, _>(cube, |reset| reset.0 = true);
                }
            });
        }),));
    }
    // The second panel shows the world's input state, which is what makes the
    // events visible next to the UI they also drive.
    // A key behaviour: space toggles the spin, and it reads the state the
    // panel's checkbox also writes.
    world.spawn((OnKey::new(move |world, _entity, key| {
        if key.pressed && !key.repeat && key.key == Key::Space {
            let _ = world.with_mut::<Spin, _>(cube, |spin| spin.spinning = !spin.spinning);
        }
    }),));

    // A pointer behaviour: dragging orbits the camera. It is mounted on
    // `OnPointer` rather than `OnMouse`, so the same drag works with a mouse
    // and with a finger on a touch screen.
    world.spawn((OnPointer::new(move |world, _entity, event| {
        let Some(position) = event.position else {
            if matches!(
                event.action,
                PointerAction::Released { .. } | PointerAction::Left
            ) {
                let _ = world.with_mut::<DragFrom, _>(cube, |drag| drag.0 = None);
            }
            return;
        };

        match event.action {
            PointerAction::Pressed => {
                let _ = world.with_mut::<DragFrom, _>(cube, |drag| {
                    drag.0 = Some(PointerContact {
                        kind: event.kind,
                        id: event.id,
                        position,
                    });
                });
            }
            PointerAction::Moved => {
                let previous = world.with_mut::<DragFrom, _>(cube, |drag| {
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
                let _ = world.with_mut::<CameraOrbit, _>(camera, |orbit| {
                    orbit.azimuth -= dx * RADIANS_PER_PIXEL;
                    orbit.elevation = (orbit.elevation + dy * RADIANS_PER_PIXEL).clamp(-1.4, 1.4);
                });
            }
            PointerAction::Released { .. } | PointerAction::Left => {
                let _ = world.with_mut::<DragFrom, _>(cube, |drag| drag.0 = None);
            }
            PointerAction::Zoom(_) | PointerAction::Rotate(_) => {}
        }
    }),));

    if ui {
        world.spawn((UiPanel::new(move |world, _entity, ui| {
            egui::Window::new("input")
                .default_pos([16.0, 300.0])
                .show(ui.ctx(), |ui| {
                    let held = world
                        .query::<&InputState>()
                        .next()
                        .is_some_and(|(_, state)| state.pointer_down);
                    ui.label(if held { "pointer: down" } else { "pointer: up" });
                    match world.get::<CameraOrbit>(camera) {
                        Some(orbit) => ui.label(format!(
                            "azimuth {:.2}, elevation {:.2}",
                            orbit.azimuth, orbit.elevation
                        )),
                        None => ui.label("no camera state"),
                    };
                });
        }),));
    }

    SceneControl {
        advance: Box::new(move |world, _frame, delta_time| {
            // The panel's button leaves its press as state; consume it here,
            // once.
            for (_, mut reset) in world.query::<&mut SpinReset>() {
                if core::mem::take(&mut reset.0) {
                    for (_, mut spin) in world.query::<&mut Spin>() {
                        spin.angle = 0.0;
                    }
                }
            }

            // The behaviour components run first: the world the renderer reads
            // is this frame's.
            for (_, mut frames) in world.query::<&mut FrameCount>() {
                frames.0 += 1;
            }
            for (_, (mut spin, mut transform)) in world.query::<(&mut Spin, &mut Transform)>() {
                if spin.spinning {
                    spin.angle += spin.radians_per_second * delta_time;
                }
                transform.rotation = glam::Quat::from_rotation_y(spin.angle);
            }

            // The camera follows the orbit a slider or a drag set.
            let aspect = size.0 as f32 / size.1 as f32;
            for (_, (orbit, mut camera)) in world.query::<(&CameraOrbit, &mut Camera)>() {
                *camera = orbit_camera(aspect, orbit.azimuth, orbit.elevation);
            }
        }),
        snapshot: Box::new(|frame| (frame == 29).then(|| "example.webp".to_owned())),
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
/// The built-in pipeline compares depth with `Greater` and clears to the far
/// plane, so the projection is reverse-z infinite.
fn orbit_camera(aspect: f32, azimuth: f32, elevation: f32) -> Camera {
    let projection = glam::camera::rh::proj::directx::perspective_infinite_reverse(
        60f32.to_radians(),
        aspect,
        0.1,
    );
    let eye = ORBIT_TARGET
        + ORBIT_RADIUS
            * glam::Vec3::new(
                elevation.cos() * azimuth.sin(),
                elevation.sin(),
                elevation.cos() * azimuth.cos(),
            );
    let view = glam::camera::rh::view::look_at_mat4(eye, ORBIT_TARGET, glam::Vec3::Y);
    Camera {
        clip_from_world: projection * view,
        position: eye,
    }
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
