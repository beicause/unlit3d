// blit_fragment -- the fragment stage of a screen composite: one texture
// sampled over the target, converted as the target asks.
//
// The matching vertex stage is `fullscreen_vertex.wesl`. The two are compiled
// as separate main modules, because the vertex keeps its own name only while it
// is the main module of its compile; a composite therefore pairs that module's
// vertex with this module's fragment across two shader modules.
//
// Feature flags:
//   SRGB_TO_LINEAR_OUTPUT  the sampled texture stores sRGB-encoded values and
//                          the target expects linear ones, so convert the
//                          fragment's RGB from sRGB to linear before writing;
//                          alpha is coverage and never converts

import package::lit_fragment;

@group(0) @binding(0)
var blit_texture: texture_2d<f32>;

@group(0) @binding(1)
var blit_sampler: sampler;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return lit_fragment::encoded_output(textureSample(blit_texture, blit_sampler, uv));
}
