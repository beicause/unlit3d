//! Material helpers: the keys, samplers and alpha rules a lit pipeline
//! derives from a glTF material or primitive.

use super::DEFAULT_ALPHA_CUTOFF;
use crate::lit::LitPipelineKey;
use unlit_wgpu::pipeline::LitOptions;

/// The key a material bind group is built against.
///
/// Only `texture_filtering` shapes the material group's layout, so any key
/// carrying it works; this is the one a base-color-textured primitive's
/// [`LitGltf::pipeline_key`](super::LitGltf::pipeline_key) derives to. The
/// per-vertex channels a draw adds do not enter the material group, and wgpu
/// deduplicates identical layout descriptors, so the group fits the pipeline
/// whatever else its key reads.
pub(super) fn textured_key(filtering: bool) -> LitPipelineKey {
    LitPipelineKey::new(LitOptions {
        unlit: true,
        texture_filtering: filtering,
        ..LitOptions::standard()
    })
}

/// Whether a primitive samples its material's base-color texture.
///
/// A texture is sampled with the per-vertex UV, so a primitive that carries no
/// `TEXCOORD_0` cannot sample one and is drawn untextured even when its
/// material declares a texture. The answer is a property of the draw — the
/// material it names and the attributes it carries — rather than of anything a
/// caller picks, which is why it is derived here rather than stored in the key.
pub(super) fn samples_base_color_texture(primitive: &gltf::Primitive<'_>) -> bool {
    primitive.get(&gltf::Semantic::TexCoords(0)).is_some()
        && primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_texture()
            .is_some()
}

/// How many of a primitive's morph targets displace positions.
///
/// A target may displace only normals or tangents; those are ignored here, and
/// a target that carries no positions at all would contribute nothing to a
/// displacement array, so it is not counted and the mesh is not uploaded as
/// morphing. The count is therefore of the targets that *do* displace, in the
/// order they appear — which is the order the shader's weights are indexed in,
/// not the document's own target indices.
pub(super) fn morph_target_count(primitive: &gltf::Primitive<'_>) -> usize {
    primitive
        .morph_targets()
        .filter(|target| target.positions().is_some())
        .count()
}

/// The alpha a `MASK` material's fragments are cut off at.
///
/// Returns `None` for every other `alphaMode`, for which there is nothing to
/// cut. The value is the material's own `alphaCutoff`, or the spec's default
/// when the material leaves it out.
pub(super) fn alpha_cutoff(material: &gltf::Material<'_>) -> Option<f32> {
    (material.alpha_mode() == gltf::material::AlphaMode::Mask)
        .then(|| material.alpha_cutoff().unwrap_or(DEFAULT_ALPHA_CUTOFF))
}

/// Translate a glTF sampler to wgpu terms.
///
/// Unspecified filters default to `Linear`, unspecified wrap modes default to
/// `Repeat` — the glTF spec defaults.
pub(super) fn sampler_descriptor(
    sampler: &gltf::texture::Sampler<'_>,
) -> wgpu::SamplerDescriptor<'static> {
    use gltf::texture::{MagFilter, MinFilter};
    let (min_filter, mipmap_filter) = match sampler.min_filter() {
        Some(MinFilter::Nearest) | Some(MinFilter::NearestMipmapNearest) => {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest)
        }
        Some(MinFilter::NearestMipmapLinear) => {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Linear)
        }
        Some(MinFilter::LinearMipmapNearest) => {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Nearest)
        }
        Some(MinFilter::Linear) | Some(MinFilter::LinearMipmapLinear) | None => {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear)
        }
    };
    let mag_filter = match sampler.mag_filter() {
        Some(MagFilter::Nearest) => wgpu::FilterMode::Nearest,
        Some(MagFilter::Linear) | None => wgpu::FilterMode::Linear,
    };
    wgpu::SamplerDescriptor {
        label: None,
        address_mode_u: address_mode(sampler.wrap_s()),
        address_mode_v: address_mode(sampler.wrap_t()),
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter,
        min_filter,
        mipmap_filter,
        ..wgpu::SamplerDescriptor::default()
    }
}

fn address_mode(mode: gltf::texture::WrappingMode) -> wgpu::AddressMode {
    use gltf::texture::WrappingMode;
    match mode {
        WrappingMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        WrappingMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        WrappingMode::Repeat => wgpu::AddressMode::Repeat,
    }
}
