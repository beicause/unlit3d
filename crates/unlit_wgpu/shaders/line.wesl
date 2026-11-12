// line -- the built-in stroke entry shader: one vertex stage, one fragment stage.
//
// This module holds the two entry points and nothing else. The stages are
// composed out of the rest of the package, so each part a caller might replace
// is a module of its own and the caller assembles them rather than text being
// inserted into a source it cannot see:
//
//   line_bindings   the camera and the viewport, the two uniforms the stages read
//   line_vertex     VertexInput, VertexOutput and the quad one segment covers
//   line_fragment   the coverage of a pixel, and what the stroke writes there
//
// A caller that wants its own geometry or its own compositing writes a module
// that imports these and calls the parts it keeps, in the order this file runs
// them, and passes it to LineOptions::shader. The entry-point names, the
// instance record and the layouts stay the same either way, because they are the
// pipeline's rather than the shader's.
//
// One segment is one instance: the vertex stage derives the quad that covers it
// from `@builtin(vertex_index)`, so the stream carries segments rather than
// vertices and a caller never expands a segment on the CPU. Both ends are
// projected and measured in pixels against the pass's viewport, which is what
// makes the width a pixel count rather than a world distance: a stroke keeps its
// thickness at any distance from the camera and across any viewport size.
//
// Feature flags:
//   HARD_EDGE  apply the coverage as a threshold instead of a blend; see
//              line_fragment, and the variant, which sets it only on a
//              single-sampled target

import package::line_fragment;
import package::line_vertex;

@vertex
public fn vs_main(input: line_vertex::VertexInput, @builtin(vertex_index) index: u32) -> line_vertex::VertexOutput {
    return line_vertex::vertex_out(input, index);
}

@fragment
public fn fs_main(in: line_vertex::VertexOutput) -> @location(0) vec4<f32> {
    return line_fragment::fs_stage(in);
}
