// lit_bindings -- the uniforms and the material texture the lit stages read.
//
// Each part of the lit stages lives in a module of its own, so that a caller can
// import the parts it keeps: this one declares what the stages bind,
// `lit_vertex` places and emits the vertex, `lit_lighting` shades a fragment,
// `lit_fragment` colors and encodes it, and `lit.wesl` calls them in the order
// the stages need.
//
// The variables are `public` because a caller's module reads them: a vertex
// stage that reshapes geometry still needs the camera, and a fragment stage that
// draws the cluster grid reads the cluster uniform and the frame's lights
// itself.
//
// The frame's shared arrays -- the mesh metadata, the joint matrices, the morph
// weights and displacements, the lights, the light cells and the polygon
// vertices -- belong to `array_access`, which declares them and the `load_*`
// accessors that read them.

import package::camera as camera_uniform;
import package::globals;
import package::light;

/// The camera the frame draws with: the clip transform the vertex stage ends on,
/// and the view transform and eye position a lit fragment shades from.
@group(0) @binding(0) public var<uniform> camera: camera_uniform::Camera;

/// The frame's own uniform, which the lit stages do not read. It is declared for
/// the slot's sake: the group layout is the same for every built-in pipeline, so
/// a shader that leaves it out would not match the layout it is bound with.
@group(0) @binding(1) public var<uniform> frame: globals::Globals;

/// The frame's light grid: its dimensions, its depth slices and the parameters
/// that map a view depth into one.
@if(LIT) @group(0) @binding(10) public var<uniform> cluster: light::ClusterUniform;

/// The frame's non-clustered lighting: the ambient term and the directional
/// lights, which reach every fragment alike.
@if(LIT) @group(0) @binding(11) public var<uniform> lighting: light::LightingUniform;

/// The material's base color, sampled at the fragment's UV.
@if(BASE_COLOR_TEXTURE) @group(1) @binding(0) public var base_color_tex: texture_2d<f32>;

/// The sampler the base-color texture is read through.
@if(BASE_COLOR_TEXTURE) @group(1) @binding(1) public var base_color_sampler: sampler;
