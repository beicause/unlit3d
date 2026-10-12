//! Render-world helpers: an asset server over entities, and the frame
//! behaviour that applies transform snapshots to render entities.
//!
//! The server helpers follow one rule: the world is the registry. A mesh or
//! a material is allocated inside the render world and left there as the
//! [`GpuMesh`] or [`GpuMaterial`] component of its own asset entity — the
//! GPU handles inside carry `Rc`, so they cannot cross a thread, but the
//! entity's bits can. A simulation world requests an allocation with
//! [`create_mesh`] or [`create_material`], holds the entity bits that come
//! back, and names them again in [`spawn_instance`] to place a rendered
//! instance of the assets.

use glam::{Quat, Vec3};
use unlit_ecs::{Entity, World};
use unlit_wgpu::pipeline::UnlitOptions;
use unlit3d::components::{GpuMaterial, GpuMesh, Transform};
use unlit3d::frame::{Frame, FrameBehaviourOrder, OnFrame};
use unlit3d::mesh_source::MeshSource;
use unlit3d::source::Source;
use unlit3d::unlit::{MeshSourceUnlitExt, UnlitMeshDesc, UnlitPipeline, UnlitPipelineKey};

use crate::channel::SnapshotReader;
use crate::job::Job;

/// The frame-behaviour order the transform applier runs at: before anything
/// that positions a camera relative to moved objects.
pub const APPLY_TRANSFORMS_ORDER: i32 = i32::MIN;

/// One frame's worth of transforms, keyed by render entity bits.
#[derive(Clone, Debug, Default)]
pub struct TransformFrame {
    /// Entity bits with the transform each render entity should take.
    pub transforms: Vec<(u64, Vec3, Quat)>,
}

/// The frame behaviour that applies the newest [`TransformFrame`] from
/// `reader` to the render world's entities.
///
/// Entities whose bits name an entity without a `Transform` are skipped:
/// an instance not yet spawned is not an error.
#[must_use]
pub fn apply_transform_frames(reader: SnapshotReader<TransformFrame>) -> OnFrame {
    OnFrame::new(
        FrameBehaviourOrder(APPLY_TRANSFORMS_ORDER),
        move |world, _entity, _frame: &mut Frame| {
            let Some(frame) = reader.latest() else {
                return;
            };
            for (bits, translation, rotation) in &frame.transforms {
                let entity = Entity::from_bits(*bits);
                if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                    transform.translation = *translation;
                    transform.rotation = *rotation;
                }
            }
        },
    )
}

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

/// The render world's mesh source, with the standard unlit pipeline key
/// built from its device.
///
/// Every server job derives the key this way: the caller never needs a
/// device, and the whole world renders through one pipeline.
fn mesh_source(world: &World) -> Option<(Entity, UnlitPipelineKey)> {
    let (entity, source) = world
        .query::<&Source>()
        .find(|(_, source)| source.as_ref::<MeshSource>().is_some())?;
    let mesh_source = source.as_ref::<MeshSource>()?;
    let key = UnlitPipelineKey::new(UnlitOptions::standard(&mesh_source.device(world)));
    Some((entity, key))
}

/// A job that allocates a mesh in the render world and answers with the
/// bits of its asset entity, the one carrying the [`GpuMesh`].
///
/// Panics inside the render world when it carries no `MeshSource`.
pub fn create_mesh(desc: OwnedUnlitMeshDesc) -> Job<u64> {
    Box::new(move |world: &mut World| {
        let Some((source_entity, key)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let mesh = world
            .with_mut::<Source, _>(source_entity, |source| {
                source
                    .as_mut::<MeshSource>()
                    .expect("the source carries a MeshSource")
                    .allocate_unlit_mesh(world, &key, desc.borrowed())
            })
            .expect("the mesh source entity exists");
        world.spawn((mesh,)).to_bits()
    })
}

/// A job that allocates one solid-color material in the render world and
/// answers with the bits of its asset entity, the one carrying the
/// [`GpuMaterial`].
///
/// The base-color texture is a single pixel of `color`; the sampler is
/// created for this material alone and descriptor describes it.
/// Panics inside the render world when it carries no `MeshSource`.
pub fn create_material(color: [u8; 4], descriptor: wgpu::SamplerDescriptor<'static>) -> Job<u64> {
    Box::new(move |world: &mut World| {
        let Some((source_entity, key)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let material = world
            .with_mut::<Source, _>(source_entity, |source| {
                let source = source
                    .as_mut::<MeshSource>()
                    .expect("the source carries a MeshSource");
                let texture =
                    solid_color_texture(&source.device(world), &source.queue(world), color);
                let view = source.register_texture_and_default_view(world, texture).1;
                let sampler = source.register_sampler(world, Some(descriptor));
                source.allocate_unlit_material(world, &key, view, sampler)
            })
            .expect("the mesh source entity exists");
        world.spawn((material,)).to_bits()
    })
}

/// A job that allocates one material whose base-color texture is the
/// `size.0` by `size.1` RGBA image `pixels`, and answers with the bits of
/// its asset entity, the one carrying the [`GpuMaterial`].
///
/// The sampler is created for this material alone and `descriptor`
/// describes it. Panics inside the render world when it carries no
/// `MeshSource`.
///
/// # Panics
/// Panics when `pixels` does not hold exactly `size.0 * size.1` pixels.
pub fn create_material_from_texture(
    size: (u32, u32),
    pixels: Vec<[u8; 4]>,
    descriptor: wgpu::SamplerDescriptor<'static>,
) -> Job<u64> {
    assert_eq!(
        pixels.len(),
        size.0 as usize * size.1 as usize,
        "the pixels do not fill the texture"
    );
    Box::new(move |world: &mut World| {
        let Some((source_entity, key)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let material = world
            .with_mut::<Source, _>(source_entity, |source| {
                let source = source
                    .as_mut::<MeshSource>()
                    .expect("the source carries a MeshSource");
                let texture =
                    rgba_texture(&source.device(world), &source.queue(world), size, &pixels);
                let view = source.register_texture_and_default_view(world, texture).1;
                let sampler = source.register_sampler(world, Some(descriptor));
                source.allocate_unlit_material(world, &key, view, sampler)
            })
            .expect("the mesh source entity exists");
        world.spawn((material,)).to_bits()
    })
}

/// A job that spawns one render entity from asset entities: a [`GpuMesh`]
/// cloned from `mesh`, a [`GpuMaterial`] cloned from `material`, and the
/// `transform` it starts at. Answers with the new entity's bits.
///
/// The assets are named by the bits [`create_mesh`] and [`create_material`]
/// answered. Panics inside the render world when it carries no
/// `MeshSource`, or when an asset entity is gone.
pub fn spawn_instance(mesh: u64, material: u64, transform: Transform) -> Job<u64> {
    Box::new(move |world: &mut World| {
        let Some((_, key)) = mesh_source(world) else {
            panic!("the render world carries no MeshSource");
        };
        let mesh = world
            .get::<GpuMesh>(Entity::from_bits(mesh))
            .expect("the mesh asset entity exists")
            .clone();
        let material = world
            .get::<GpuMaterial>(Entity::from_bits(material))
            .expect("the material asset entity exists")
            .clone();
        world
            .spawn((transform, mesh, material, UnlitPipeline::new(key)))
            .to_bits()
    })
}

/// An RGBA texture of one solid color, for materials that only carry a
/// tint.
#[must_use]
pub fn solid_color_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color: [u8; 4],
) -> wgpu::Texture {
    rgba_texture(device, queue, (1, 1), &[color])
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
