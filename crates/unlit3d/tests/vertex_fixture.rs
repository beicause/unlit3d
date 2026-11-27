//! The vertex the custom-family tests draw with.
//!
//! Both cases hand a caller's pipeline an interleaved `position + colour`
//! vertex, so the record, the layout that describes it and the box it spans are
//! stated once here rather than in each case.

use unlit3d::prelude::*;

/// An interleaved `position + colour` vertex, matching `ATTRIBUTES` and the
/// WGSL the cases compile.
#[repr(C)]
#[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Vertex {
    /// Where the vertex sits, in the space its shader reads.
    pub position: [f32; 3],
    /// The tint the fragment stage writes.
    pub color: [f32; 4],
}

/// The locations the two fields take in a pipeline's vertex layout.
const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

/// The vertex-step layout a shader reading [`Vertex`] declares.
pub fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

/// A [VertexBufferDesc] for the mesh's own vertices in slot 0.
pub fn vertex_buffer(buffer: wgpu::Buffer) -> VertexBufferDesc {
    let layout = vertex_layout();
    VertexBufferDesc {
        slot: 0,
        buffer,
        array_stride: layout.array_stride,
        step_mode: layout.step_mode,
        attributes: layout.attributes.into(),
    }
}

/// The axis-aligned box the vertices actually span.
pub fn aabb_of(vertices: &[Vertex]) -> Aabb {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    Aabb::new(min.into(), max.into())
}
