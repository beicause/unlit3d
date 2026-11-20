//! Material helpers: the keys, samplers and alpha rules a lit pipeline
//! derives from a glTF material or primitive.

use super::DEFAULT_ALPHA_CUTOFF;
use crate::lit::LitPipelineKey;
use unlit_wgpu::pipeline::LitOptions;

/// The key a material bind group is built against.
///
/// Only `texture_filtering` shapes the material group's layout, so any key
/// carrying it works, whatever shading mode the primitive's key names. The
/// per-vertex channels a draw adds do not enter the material group, and wgpu
/// deduplicates identical layout descriptors, so the group fits the pipeline
/// whatever else its key reads.
pub(super) fn textured_key(filtering: bool) -> LitPipelineKey {
    LitPipelineKey::new(LitOptions {
        texture_filtering: filtering,
        ..LitOptions::standard()
    })
}

/// The `TEXCOORD_n` set a primitive samples its base-color texture through, or
/// `None` when it samples no texture at all.
///
/// A texture carries its texels in a UV set the material names with
/// `texCoord`, defaulting to the first. A primitive that does not carry that
/// set cannot sample the texture — the UV the draw would interpolate does not
/// exist — so it is drawn untextured rather than bound to a channel it does not
/// have. The set the primitive does name is the one uploaded as its UV stream.
///
/// The answer is a property of the draw — the material it names and the
/// attributes it carries — rather than of anything a caller picks, which is why
/// it is derived here rather than stored in the key.
pub(super) fn base_color_uv_set(primitive: &gltf::Primitive<'_>) -> Option<u32> {
    let set = primitive
        .material()
        .pbr_metallic_roughness()
        .base_color_texture()?
        .tex_coord();
    primitive.get(&gltf::Semantic::TexCoords(set)).map(|_| set)
}

/// How many morph targets a primitive declares.
///
/// The count is the length of the document's `targets` array, not the number of
/// targets that displace positions: a weight is indexed by the target's own slot
/// in that array, so a target that displaces only normals, or nothing at all,
/// still owns a weight. Dropping such a target would shift every later target's
/// weight onto the wrong slot and deform the mesh into the wrong shape. The
/// loader uploads no displacement for it, leaving that slot's displacements at
/// zero.
pub(super) fn morph_target_count(primitive: &gltf::Primitive<'_>) -> usize {
    primitive.morph_targets().len()
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
