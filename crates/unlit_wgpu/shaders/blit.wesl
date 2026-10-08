// The fragment stage of a blit: sample one texture over the target.
//
// The matching vertex stage is `fullscreen_vertex.wesl`. The two are compiled
// as separate main modules, because the vertex has to keep its own name for the
// pipeline to address it; a blit therefore pairs this module's fragment with
// that module's vertex across two shader modules.

@group(0) @binding(0)
var blit_texture: texture_2d<f32>;

@group(0) @binding(1)
var blit_sampler: sampler;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return textureSample(blit_texture, blit_sampler, uv);
}
