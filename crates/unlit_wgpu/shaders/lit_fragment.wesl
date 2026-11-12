// lit_fragment -- the fragment stage: the material's color, the cut-off, the
// target's encoding.
//
// The steps are exported one by one so a caller composes its own stage:
// `fragment_color` is the material's color, `discard_transparent` the coverage
// test, `lit_lighting::lit_color` the shading, and `encoded_output` the
// conversion the target asks for. `fs_stage` is those steps in the built-in
// order, which is what `lit.wesl` runs.

import package::lit_bindings;
import package::lit_lighting;
import package::lit_vertex;

/// Convert a channel from sRGB (gamma) to linear light.
///
/// A base-color texture's RGB values are sRGB-encoded, whatever else the
/// variant does with them. A four-channel one is uploaded in an sRGB format
/// and so is decoded by the sampler; a luminance one cannot be, because
/// WebGPU offers the transfer function only for four-channel formats, so the
/// fragment decodes it here instead.
public fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        return channel / 12.92;
    }
    return pow((channel + 0.055) / 1.055, 2.4);
}

/// The color of one fragment, before the target's encoding.
///
/// `in.color` and the base-color texel carry whatever alpha convention the
/// caller's blend state expects — premultiplied for the UI, straight for
/// meshes — and multiplying two RGBA values preserves it, so they combine
/// directly.
///
/// The texel is decoded to linear light before it is combined: `in.color` is
/// a linear factor, and a factor times an sRGB-encoded value would not be
/// either. A luminance alpha is coverage, like any other alpha, so it is
/// taken as it is.
public fn fragment_color(in: lit_vertex::VertexOut) -> vec4<f32> {
    var color = in.color;
    @if(BASE_COLOR_TEXTURE) {
        let texel = textureSample(
            lit_bindings::base_color_tex,
            lit_bindings::base_color_sampler,
            in.uv,
        );
        // A one- or two-channel texture carries its value in `r`, and its
        // alpha — if it has one — in `g`; the channels past the last one read
        // as `0` and `1`, so an expansion has to name the ones it wants.
        @if(BASE_COLOR_LUMINANCE) {
            color = color * vec4<f32>(vec3<f32>(srgb_to_linear(texel.r)), texel.a);
        }
        @if(BASE_COLOR_LUMINANCE_ALPHA) {
            color = color * vec4<f32>(vec3<f32>(srgb_to_linear(texel.r)), texel.g);
        }
        @if(!BASE_COLOR_LUMINANCE && !BASE_COLOR_LUMINANCE_ALPHA) {
            color = color * texel;
        }
    }
    return color;
}

/// Discard a fragment the instance's alpha cutoff rejects.
///
/// A cut-off instance draws binary coverage: it writes depth and color like an
/// opaque one wherever the fragment is opaque enough, and writes nothing at all
/// elsewhere, so it needs no blending and no back-to-front sort. The cutoff is
/// the instance's own, so two instances of one mesh can cut at different alphas.
@if(ALPHA_CUTOFF)
public fn discard_transparent(in: lit_vertex::VertexOut, color: vec4<f32>) {
    if color.a < in.cutoff {
        discard;
    }
}

/// A color as the render target expects to receive it.
///
/// An sRGB-aware target encodes whatever it is given, so hand it linear light
/// and let its own encode produce the color the caller asked for. Only RGB
/// converts: alpha is coverage, not color.
public fn encoded_output(color: vec4<f32>) -> vec4<f32> {
    @if(SRGB_TO_LINEAR_OUTPUT) {
        return vec4<f32>(
            srgb_to_linear(color.r),
            srgb_to_linear(color.g),
            srgb_to_linear(color.b),
            color.a,
        );
    }
    return color;
}

/// The built-in fragment stage: the base color, cut off and lit, then encoded
/// for the target.
///
/// A composed stage runs the same steps in the same order and stands where it
/// likes between them -- after the lighting and before the encoding is where a
/// result is composited over the shaded color.
public fn fs_stage(in: lit_vertex::VertexOut) -> vec4<f32> {
    var color = fragment_color(in);
    // Coverage is decided before the lights run, so a cut fragment costs no
    // shading at all.
    @if(ALPHA_CUTOFF) {
        discard_transparent(in, color);
    }
    // Lighting multiplies the base color and leaves its alpha alone: alpha is
    // coverage, and a surface does not become more transparent in the dark.
    @if(LIT) {
        color = lit_lighting::lit_color(in, color);
    }
    return encoded_output(color);
}
