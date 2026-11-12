// line_bindings -- the uniforms a stroke's stages read.
//
// Each part of the line stages lives in a module of its own, so that a caller can
// import the parts it keeps: this one declares what the stages bind,
// `line_vertex` builds the quad one segment covers, `line_fragment` turns a
// pixel into coverage, and `line.wesl` calls them in the order the stages need.
//
// The variables are `public` because a caller's module reads them: a vertex stage
// that routes segments itself still projects through the camera, and a fragment
// stage that measures in pixels still needs the viewport.
//
// The segment record is not here: the instance stream is the pipeline's, and
// `line_vertex::VertexInput` mirrors it.

import package::camera as camera_uniform;
import package::view;

/// The camera the frame draws with. A segment's ends are projected through its
/// clip transform into the pixel space the quad is built in.
@group(0) @binding(0) public var<uniform> camera: camera_uniform::Camera;

/// The pass's viewport. A stroke's width is a pixel count, so this is the one
/// uniform besides the camera that the vertex stage reads.
@group(0) @binding(2) public var<uniform> viewport: view::View;
