// lit -- the built-in lit entry shader: one vertex stage, one fragment stage.
//
// This module holds the two entry points and nothing else. The stages are
// composed out of the rest of the package, so each part a caller might replace
// is a module of its own and the caller assembles them rather than text being
// inserted into a source it cannot see:
//
//   lit_bindings    the uniforms and the base-color texture the stages read
//   lit_vertex      VertexInput, VertexOut and the vertex stage's two halves
//   lit_lighting    the shading of a lit fragment
//   lit_fragment    the fragment stage's color, cut-off and encoding
//
// A caller that wants its own geometry or its own shading writes a module that
// imports these and calls the parts it keeps, in the order this file runs them,
// and passes it to LitOptions::shader. The entry-point names and the variant's
// vertex streams and layouts stay the same either way, because they are the
// pipeline's rather than the shader's.
//
// One entry shader covers every built-in variant: the differences are
// orthogonal WESL conditional-compilation flags, so each composed variant
// contains exactly one code path and only the bindings and attributes it
// uses. Locations, binding slots and vertex-buffer slots are stable across
// variants.
//
// Feature flags:
//   VERTEX_POSITION      read a per-vertex position; without it the geometry
//                        is a single point at the instance origin
//   UNCOMPRESSED_POSITION  the position is full precision and needs no decode;
//                        otherwise it is a compressed `vec4<f32>` decoded
//                        through `mesh_meta`
//   PLANAR_POSITION       the position is a full-precision `vec2<f32>`: the
//                        geometry already lies in the target's plane, so the
//                        zero third is supplied here; implies
//                        `UNCOMPRESSED_POSITION`
//   VERTEX_UV            read a per-vertex UV
//   UNCOMPRESSED_UV      the UV is a full-precision `vec2<f32>` and needs no
//                        decode; otherwise it is compressed and remapped by
//                        the mesh metadata
//   VERTEX_COLOR         read a per-vertex color and multiply it into the
//                        base color
//   VERTEX_NORMAL        read a per-vertex normal, octahedral-encoded into
//                        two `snorm16` lanes; a lit variant shades with it,
//                        an expanding one pushes the vertex along it, and the
//                        unlit color does neither
//   BASE_COLOR_TEXTURE   sample the material base-color texture
//   SRGB_TO_LINEAR_OUTPUT  the fragment's color values are sRGB-encoded and
//                        the target expects linear values, so convert the
//                        fragment's RGB from sRGB to linear before writing;
//                        alpha is coverage and never converts
//   VERTEX_JOINTS        read joint indices and weights from the position
//                        stream and deform the vertex by the joint matrices
//   MORPH_POSITIONS      read a per-vertex morph position offset and add it
//                        to the deformed position
//   COMPRESSED_CHANNEL_OR_MORPH  the vertex stage reads the mesh metadata: a
//                        compressed position or UV to decode, or a morph to
//                        address through its vertex offset and target count
//   TEXEL_ARRAY          the arrays that would otherwise be `var<storage,
//                        read>` are read from 2D textures instead, one `f32`
//                        lane per texel component; for devices with no storage
//                        buffers at all, WebGL2 in particular
//   BASE_COLOR_LUMINANCE  the base-color texture stores one luminance channel;
//                        sample it into every color channel and decode the
//                        result from sRGB
//   BASE_COLOR_LUMINANCE_ALPHA  the base-color texture stores luminance and
//                        alpha; sample them into RGB and alpha and decode the
//                        luminance from sRGB
//   ALPHA_CUTOFF         discard a fragment whose alpha is below the
//                        instance's cutoff
//   EXPANDED             push each vertex out along its normal by the
//                        instance's hull width, which is what turns a mesh
//                        into the shell an inverted-hull outline draws behind
//                        it; requires VERTEX_NORMAL
//   LIT                  shade the fragment with the frame's lights: read the
//                        cell the fragment's NDC and view depth identify,
//                        walk its cell and add each light to the ambient term
//
// The bindings and the vertex streams are declared where they are used:
// lit_bindings lists the uniform slots, array_access the frame's shared arrays
// and the accessors that read them, and lit_vertex the stream slots.

import package::lit_fragment;
import package::lit_lighting;
import package::lit_vertex;

@vertex
public fn vs_main(in: lit_vertex::VertexInput, @builtin(vertex_index) vertex_index: u32) -> lit_vertex::VertexOut {
    return lit_vertex::vs_stage(in, vertex_index);
}

@fragment
public fn fs_main(in: lit_vertex::VertexOut) -> @location(0) vec4<f32> {
    return lit_fragment::fs_stage(in);
}
