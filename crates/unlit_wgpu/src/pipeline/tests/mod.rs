//! The pipeline's unit tests, grouped by what they check.

use super::variant::compose_shader;
use super::*;
use crate::compose::{FRAGMENT_HOOK, MODULE_HOOK, VERTEX_HOOK};
use crate::specialize::Variants;

mod compose;
mod instance;
mod slots;
mod variants;

/// The non-blank lines of a composed module, in a fixed order.
///
/// The order is not the module's: the composed declarations come out of a
/// hash map, so two compilations of the same source can list them
/// differently. Line sets compare what a snippet adds or takes away, which
/// is the question here.
fn code_lines(wgsl: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = wgsl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.sort_unstable();
    lines
}

/// The render target the test variants are built for.
fn surface() -> SurfaceKey {
    SurfaceKey {
        color_format: wgpu::TextureFormat::Rgba8UnormSrgb,
        depth_stencil_format: Some(wgpu::TextureFormat::Depth24PlusStencil8),
        sample_count: 4,
    }
}

/// A variant from its caller policy and the facts a draw carries.
fn variant(
    mut options: LitOptions,
    channels: LitVertexChannels,
    base_color_texture: bool,
    morph: bool,
) -> LitVariant {
    // These variants draw without lights: their global group carries the
    // camera, the frame clock and the mesh metadata, and a lit one would
    // also demand the light arrays and the cluster. The lighting dimension
    // is enumerated on its own where a test composes both.
    options.unlit = true;
    LitVariant {
        options: Hashed::new(options),
        surface: surface(),
        channels,
        base_color_texture,
        morph,
        strip_index_format: None,
    }
}

/// The vertex channels with a compressed position and nothing else.
fn channels(
    position: Option<ChannelEncoding>,
    joints: bool,
    secondary: SecondaryFlags,
) -> LitVertexChannels {
    LitVertexChannels {
        position: PositionStreamChannels { position, joints },
        secondary,
    }
}

/// Every vertex-channel and policy combination that composes.
///
/// A sampled base-color texture implies a UV, and the joint and morph
/// channels imply the position they deform, so the invalid combinations are
/// skipped. Each surviving combination is then enumerated once per luminance
/// layout, because those describe the sampled texel rather than the vertex
/// layout and so are orthogonal to everything above — but only where a texel
/// is sampled at all.
///
/// The instance stream is not a dimension: every built-in variant binds one,
/// so a per-instance field never has to be declared separately. The array path
/// is not one either: it is the device's answer and is passed to
/// [`compose_shader`] rather than carried by the variant.
fn all_variants() -> Vec<LitVariant> {
    // A position is absent — leaving the geometry a point at the instance
    // origin — or present in one of the three encodings.
    let positions: &[Option<ChannelEncoding>] = &[
        None,
        Some(ChannelEncoding::CompressedPosition),
        Some(ChannelEncoding::UncompressedPosition),
        Some(ChannelEncoding::PlanarPosition),
    ];
    // The secondary stream carries a UV in either encoding, a color, a
    // normal, or any combination; an uncompressed UV is still a UV.
    let secondaries: &[SecondaryFlags] = &[
        SecondaryFlags::empty(),
        SecondaryFlags::UV,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV,
        SecondaryFlags::COLOR,
        SecondaryFlags::UV | SecondaryFlags::COLOR,
        SecondaryFlags::UV | SecondaryFlags::UNCOMPRESSED_UV | SecondaryFlags::COLOR,
        SecondaryFlags::NORMAL,
        SecondaryFlags::UV | SecondaryFlags::NORMAL,
        SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
        SecondaryFlags::UV | SecondaryFlags::COLOR | SecondaryFlags::NORMAL,
    ];
    let mut variants = Vec::new();
    for position in positions {
        for joints in [false, true] {
            for secondary in secondaries {
                for base_color_texture in [false, true] {
                    for morph in [false, true] {
                        // The joint stream is part of the position stream,
                        // and a morph target displaces the position.
                        if (joints || morph) && position.is_none() {
                            continue;
                        }
                        // The base-color texture is sampled with the
                        // per-vertex UV.
                        if base_color_texture && !secondary.contains(SecondaryFlags::UV) {
                            continue;
                        }
                        let base = variant(
                            LitOptions::standard(),
                            channels(*position, joints, *secondary),
                            base_color_texture,
                            morph,
                        );
                        // A texel is expanded by one of the two luminance
                        // layouts, or not at all, and only a sampled texel
                        // can be expanded.
                        let luminance_layouts: &[BaseColorChannels] = if base_color_texture {
                            &[
                                BaseColorChannels::Rgba,
                                BaseColorChannels::Luminance,
                                BaseColorChannels::LuminanceAlpha,
                            ]
                        } else {
                            &[BaseColorChannels::Rgba]
                        };
                        for base_color in luminance_layouts {
                            // A cutoff compares its instance's alpha against
                            // the instance's own cutoff, so any variant can
                            // cut — textured or not.
                            for alpha_cutoff in [false, true] {
                                let mut candidate = base.clone();
                                candidate.options.update(|options| {
                                    options.base_color = *base_color;
                                    options.alpha_cutoff = alpha_cutoff;
                                });
                                variants.push(candidate);
                            }
                        }
                    }
                }
            }
        }
    }
    variants
}
