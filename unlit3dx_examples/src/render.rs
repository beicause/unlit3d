//! The demo's render bridge: an asset server over entities in the render
//! sub-world.
//!
//! `unlit3dx` leaves the render bridge to its callers; this module is the
//! demo's own, built on the runtime's core and on `unlit3d`'s generic
//! `MeshSource` registry. It is a template for a caller's bridge: register a
//! pipeline family and allocate through the mesh source, and the world is the
//! same registry either way.
//!
//! The server helpers follow one rule: the world is the registry. A mesh or
//! a material is allocated inside the render world and left there as the
//! [`GpuMesh`] or [`GpuMaterial`] component of its own asset entity — the
//! GPU handles inside carry `Rc`, so they cannot cross a thread, but the
//! entity can. A simulation world builds through a job that names the assets
//! by entity, or asks for an entity it does not have yet with an
//! [`ask`](unlit3dx::prelude::JobSender::ask): the answer comes back as a
//! snapshot the world published, and the next job names it.

use glam::{Affine3A, Vec3};
use unlit_ecs::{Entity, Scope, World};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit3d::bounds::FrustumPlanes;
use unlit3d::components::{Camera, GpuMaterial, GpuMesh, InstanceVisibility, Transform};
use unlit3d::culling::is_culled;
use unlit3d::mesh_source::MeshSource;
use unlit3d::source::Source;
use unlit3d::unlit::{MeshSourceUnlitExt, UnlitMeshDesc, UnlitPipeline, UnlitPipelineKey};

use unlit_ecs::bundle;
use unlit3dx::job::Job;
use unlit3dx::prelude::Stream;

/// A mesh description that owns its vertex data, so it can cross a thread
/// inside a job; [`UnlitMeshDesc`] is what the render world finally sees.
#[derive(Clone, Debug, Default)]
pub struct OwnedUnlitMeshDesc {
    /// Per-vertex positions, in the mesh's local space.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex UVs, for a variant that reads them.
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Per-vertex linear RGBA colors, for a variant that reads them.
    pub colors: Option<Vec<[u8; 4]>>,
    /// Triangle indices, for an indexed draw.
    pub indices: Option<Vec<u32>>,
}

impl OwnedUnlitMeshDesc {
    /// The borrowed view the allocation API takes.
    #[must_use]
    fn borrowed(&self) -> UnlitMeshDesc<'_> {
        UnlitMeshDesc {
            positions: &self.positions,
            uvs: self.uvs.as_deref(),
            colors: self.colors.as_deref(),
            indices: self.indices.as_deref(),
            ..Default::default()
        }
    }
}

/// The render world's mesh source and the pipeline key its server jobs
/// use.
///
/// The key is the one installed in the render world: a caller that spawns
/// an [`UnlitPipelineKey`] component chooses the options. Without one the
/// standard unlit options are built from the mesh source's device, so a
/// caller that only wants the built-in look never has to touch a device.
///
/// For a pipeline of the caller's own, register a family and allocate
/// through `unlit3d`'s [`MeshSource`] directly; the world is the same
/// registry either way.
pub(crate) fn mesh_source(world: &World) -> Option<(Entity, UnlitPipelineKey)> {
    let (source, entity) = world
        .root_scope()
        .query::<(&Source, Entity)>()
        .find(|(source, _)| source.as_ref::<MeshSource>().is_some())?;
    let mesh_source = source.as_ref::<MeshSource>()?;
    let key = world
        .root_scope()
        .query::<&UnlitPipelineKey>()
        .next()
        .map(|unlit_pipeline_key| unlit_pipeline_key.clone())
        .unwrap_or_else(|| {
            UnlitPipelineKey::new(UnlitOptions::standard(
                &mesh_source.device(&world.root_scope()),
            ))
        });
    Some((entity, key))
}

/// Allocate a mesh in the render world, answering its asset entity.
///
/// This is the mesh allocation a job that builds several things in one step
/// makes for itself. Panics when the world carries no `MeshSource`.
/// several things in one step. Panics when the world carries no
/// `MeshSource`.
pub(crate) fn allocate_mesh(world: &mut World, desc: &OwnedUnlitMeshDesc) -> Entity {
    let Some((source_entity, key)) = mesh_source(world) else {
        panic!("the render world carries no MeshSource");
    };
    let root = world.root_scope();
    let mesh = root
        .query_one::<&mut Source>(source_entity)
        .map(|mut source| {
            source
                .as_mut::<MeshSource>()
                .expect("the source carries a MeshSource")
                .allocate_unlit_mesh(&root, &key, desc.borrowed())
        })
        .expect("the mesh source entity exists");
    // The asset hangs under the source that draws it, so the world holds
    // one subtree per source rather than asset entities at its root.
    world
        .scope_mut(source_entity)
        .expect("the mesh source entity exists")
        .spawn(bundle![mesh])
}

/// Allocate a material over the RGBA image `pixels`, answering its asset
/// entity.
///
/// This is the material a job that builds several things in one step
/// allocates for itself.
///
/// # Panics
/// Panics when `pixels` does not hold exactly `size.0 * size.1` pixels, or
/// when the world carries no `MeshSource`.
pub(crate) fn allocate_rgba_material(
    world: &mut World,
    size: (u32, u32),
    pixels: &[[u8; 4]],
    descriptor: wgpu::SamplerDescriptor<'static>,
) -> Entity {
    assert_eq!(
        pixels.len(),
        size.0 as usize * size.1 as usize,
        "the pixels do not fill the texture"
    );
    allocate_material(world, descriptor, |source, world| {
        rgba_texture(&source.device(world), &source.queue(world), size, pixels)
    })
}

/// Allocate one material in the render world from a texture built on its
/// device, and answer its asset entity.
///
/// It builds a texture, registers it with a default view and a sampler,
/// then allocates the material. Panics inside the render world when it
/// carries no `MeshSource`.
fn allocate_material(
    world: &mut World,
    descriptor: wgpu::SamplerDescriptor<'static>,
    build_texture: impl FnOnce(&MeshSource, &Scope<'_>) -> wgpu::Texture,
) -> Entity {
    let Some((source_entity, key)) = mesh_source(world) else {
        panic!("the render world carries no MeshSource");
    };
    let root = world.root_scope();
    let material = root
        .query_one::<&mut Source>(source_entity)
        .map(|mut source| {
            let source = source
                .as_mut::<MeshSource>()
                .expect("the source carries a MeshSource");
            let texture = build_texture(source, &root);
            let view = source.register_texture_and_default_view(&root, texture).1;
            let sampler = source.register_sampler(&root, Some(descriptor));
            source.allocate_unlit_material(&root, &key, view, sampler)
        })
        .expect("the mesh source entity exists");
    world
        .scope_mut(source_entity)
        .expect("the mesh source entity exists")
        .spawn(bundle![material])
}

/// Spawn one rendered instance from asset entities, answering the new
/// entity.
///
/// This is the instance a job that builds several things in one step spawns
/// for itself. Panics when the world carries no `MeshSource`, or when an
/// asset entity is gone.
pub(crate) fn instance_of(
    world: &mut World,
    mesh: Entity,
    material: Entity,
    transform: Transform,
) -> Entity {
    let Some((source_entity, key)) = mesh_source(world) else {
        panic!("the render world carries no MeshSource");
    };
    let mesh = world
        .root_scope()
        .query_one::<&GpuMesh>(mesh)
        .expect("the mesh asset entity exists")
        .clone();
    let material = world
        .root_scope()
        .query_one::<&GpuMaterial>(material)
        .expect("the material asset entity exists")
        .clone();
    // The instance hangs under the source that draws it: a source draws
    // the subtree it owns, so an instance spawned beside it is never seen.
    world
        .scope_mut(source_entity)
        .expect("the mesh source entity exists")
        .spawn(bundle![
            transform,
            mesh,
            material,
            UnlitPipeline::new(key),
            InstanceVisibility::default(),
        ])
}

/// Spawn one rendered instance that starts hidden, until its body is ready.
pub(crate) fn hidden_instance_of(
    world: &mut World,
    mesh: Entity,
    material: Entity,
    transform: Transform,
) -> Entity {
    let instance = instance_of(world, mesh, material, transform);
    let Some((source, _)) = mesh_source(world) else {
        return instance;
    };
    let Some(scope) = world.scope(source) else {
        return instance;
    };
    let _ = scope
        .query_one::<&mut InstanceVisibility>(instance)
        .map(|mut visibility| visibility.visible = false);
    instance
}

/// A job that despawns the instances the camera cannot see, reporting each
/// one back over `retired`.
///
/// The render world owns the camera and the instances, so the visibility test
/// happens here: the gameplay world only names the live instances and reacts
/// to the answer by despawning their bodies. An instance that is already gone
/// is skipped.
pub(crate) fn sweep_instances_job(instances: Vec<Entity>, retired: Stream<Entity>) -> Job {
    Box::new(move |world: &mut World| {
        let Some(camera) = world
            .root_scope()
            .query::<&Camera>()
            .find(|camera| camera.active)
        else {
            return;
        };
        let frustum = FrustumPlanes::from_clip_from_world(camera.clip_from_world());
        drop(camera);
        let Some((source, _)) = mesh_source(world) else {
            return;
        };
        let mut outside = instances;
        {
            let Some(scope) = world.scope(source) else {
                return;
            };
            // The list is reused as the outside set: every instance that is
            // still in view is dropped from it.
            outside.retain(|instance| {
                let Some(mesh) = scope.query_one::<&GpuMesh>(*instance) else {
                    return false;
                };
                let model = scope
                    .query_one::<&Transform>(*instance)
                    .map_or(Affine3A::IDENTITY, |transform| transform.compute_matrix());
                is_culled(mesh.aabb, &model, &frustum)
            });
        }
        if outside.is_empty() {
            return;
        }
        if let Some(mut scope) = world.scope_mut(source) {
            for instance in &outside {
                let _ = scope.despawn(*instance);
            }
        }
        for instance in outside {
            let _ = retired.publish(instance);
        }
    })
}

/// A job that despawns the instances it names.
pub(crate) fn despawn_instances_job(instances: Vec<Entity>) -> Job {
    Box::new(move |world: &mut World| {
        let Some((source, _)) = mesh_source(world) else {
            return;
        };
        let Some(mut scope) = world.scope_mut(source) else {
            return;
        };
        for instance in instances {
            let _ = scope.despawn(instance);
        }
    })
}

/// A job that points one instance at a mesh and material and shows it at
/// `transform`.
pub(crate) fn show_instance_job(
    instance: Entity,
    mesh: Entity,
    material: Entity,
    transform: Transform,
) -> Job {
    Box::new(move |world: &mut World| {
        let Some((source, _)) = mesh_source(world) else {
            return;
        };
        let Some(scope) = world.scope(source) else {
            return;
        };
        let Some(mesh) = scope
            .query_one::<&GpuMesh>(mesh)
            .map(|mesh| (*mesh).clone())
        else {
            return;
        };
        let Some(material) = scope
            .query_one::<&GpuMaterial>(material)
            .map(|material| (*material).clone())
        else {
            return;
        };
        let _ = scope
            .query_one::<(
                &mut GpuMesh,
                &mut GpuMaterial,
                &mut Transform,
                &mut InstanceVisibility,
            )>(instance)
            .map(
                |(
                    mut instance_mesh,
                    mut instance_material,
                    mut instance_transform,
                    mut visibility,
                )| {
                    *instance_mesh = mesh;
                    *instance_material = material;
                    instance_transform.clone_from(&transform);
                    visibility.visible = true;
                },
            );
    })
}

/// A texture of `size.0` by `size.1` RGBA pixels.
///
/// # Panics
/// Panics when `pixels` does not hold exactly `size.0 * size.1` pixels.
#[must_use]
pub fn rgba_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: (u32, u32),
    pixels: &[[u8; 4]],
) -> wgpu::Texture {
    assert_eq!(
        pixels.len(),
        size.0 as usize * size.1 as usize,
        "the pixels do not fill the texture"
    );
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rgba_texture"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
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
        pixels.as_flattened(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * core::mem::size_of::<[u8; 4]>() as u32),
            rows_per_image: Some(size.1),
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// The opaque white every generated mesh vertex carries; materials tint
/// via textures, so the geometry itself stays colorless.
pub const WHITE_COLOR: [u8; 4] = [255, 255, 255, 255];

/// The sampler most textured materials want: linear filtering keeps flat
/// colors clean under minification.
#[must_use]
pub fn linear_sampler() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    }
}

/// A camera at `eye` looking at `target`, with a reverse-z infinite
/// perspective of `fov_y_radians` fitted to `size`.
#[must_use]
pub fn camera_looking_at(
    size: (u32, u32),
    eye: Vec3,
    target: Vec3,
    fov_y_radians: f32,
    near: f32,
) -> Camera {
    let aspect = size.0 as f32 / size.1.max(1) as f32;
    Camera {
        view_from_world: glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        clip_from_view: glam::camera::rh::proj::directx::perspective_infinite_reverse(
            fov_y_radians,
            aspect,
            near,
        ),
        active: true,
    }
}

/// A lat-long sphere: positions, uvs, opaque white colors and indices.
///
/// The triangles are wound CCW as seen from outside, matching the
/// back-face culling of the unlit pipeline: stacks grow downward and
/// slices counterclockwise, so each quad reads (top, top-next, bottom)
/// and (top-next, bottom-next, bottom) against the outward direction.
///
/// # Panics
///
/// Panics when `slices` or `stacks` is zero: the sphere would divide by
/// zero and carry NaN positions.
#[must_use]
pub fn uv_sphere(radius: f32, slices: usize, stacks: usize) -> OwnedUnlitMeshDesc {
    assert!(slices > 0 && stacks > 0, "a sphere needs slices and stacks");
    let count = (slices + 1) * (stacks + 1);
    let mut positions = Vec::with_capacity(count);
    let mut uvs = Vec::with_capacity(count);
    let mut colors = Vec::with_capacity(count);
    for stack in 0..=stacks {
        let v = stack as f32 / stacks as f32;
        let phi = v * core::f32::consts::PI;
        let (sin_phi, cos_phi) = phi.sin_cos();
        for slice in 0..=slices {
            let u = slice as f32 / slices as f32;
            let theta = u * core::f32::consts::TAU;
            let (sin_theta, cos_theta) = theta.sin_cos();
            positions.push([
                radius * sin_phi * cos_theta,
                radius * cos_phi,
                radius * sin_phi * sin_theta,
            ]);
            uvs.push([u, v]);
            colors.push(WHITE_COLOR);
        }
    }
    let mut indices = Vec::with_capacity(slices * stacks * 6);
    for stack in 0..stacks {
        for slice in 0..slices {
            let row = (stack * (slices + 1) + slice) as u32;
            let next = row + slices as u32 + 1;
            indices.extend_from_slice(&[row, row + 1, next, row + 1, next + 1, next]);
        }
    }
    OwnedUnlitMeshDesc {
        positions,
        uvs: Some(uvs),
        colors: Some(colors),
        indices: Some(indices),
    }
}

/// The unit right and up axes for each cuboid face, with the face's
/// outward normal, so the six faces share one loop.
const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
];

/// An axis-aligned cuboid from its half extents: 24 vertices (four per
/// face, so each face has clean uvs) and 36 indices, wound CCW as seen
/// from outside.
#[must_use]
pub fn cuboid_mesh(half: Vec3) -> OwnedUnlitMeshDesc {
    let mut positions = Vec::with_capacity(24);
    let mut uvs = Vec::with_capacity(24);
    let mut colors = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, up, right) in FACES {
        // The face spans the half extents projected onto its right and
        // up axes; the normal is a unit axis, so its projection is one
        // component.
        let extent_right = right[0] * half.x + right[1] * half.y + right[2] * half.z;
        let extent_up = up[0] * half.x + up[1] * half.y + up[2] * half.z;
        let base = positions.len() as u32;
        let face = |du: f32, dv: f32| -> [f32; 3] {
            [
                normal[0] * half.x + right[0] * du * extent_right + up[0] * dv * extent_up,
                normal[1] * half.y + right[1] * du * extent_right + up[1] * dv * extent_up,
                normal[2] * half.z + right[2] * du * extent_right + up[2] * dv * extent_up,
            ]
        };
        let corners = [
            face(-1.0, -1.0),
            face(1.0, -1.0),
            face(1.0, 1.0),
            face(-1.0, 1.0),
        ];
        let corner_uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (position, uv) in corners.into_iter().zip(corner_uvs) {
            positions.push(position);
            uvs.push(uv);
            colors.push(WHITE_COLOR);
        }
        // Corners sit at (-1, -1) (1, -1) (1, 1) (-1, 1) in the face's
        // (right, up) plane. When right crosses up along the normal that
        // plain order reads CCW from outside; a face whose axes cross
        // against the normal reads CW and takes the reversed order.
        let plain = Vec3::from(right)
            .cross(Vec3::from(up))
            .dot(Vec3::from(normal))
            > 0.0;
        if plain {
            indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
        } else {
            indices.extend_from_slice(&[base + 2, base + 1, base, base, base + 3, base + 2]);
        }
    }
    OwnedUnlitMeshDesc {
        positions,
        uvs: Some(uvs),
        colors: Some(colors),
        indices: Some(indices),
    }
}
