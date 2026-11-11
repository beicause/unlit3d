use super::*;

#[test]
fn default_instance_color_is_white() {
    assert_eq!(InstanceColor::default().color, glam::Vec4::ONE);
}

#[test]
fn a_strip_pipeline_declares_the_mesh_index_width() {
    use wgpu::{IndexFormat, PrimitiveTopology as Topology};
    // A strip's pipeline must declare the width its draw binds, so the
    // mesh's format reaches it whichever width the source picked.
    assert_eq!(
        strip_index_format(Topology::LineStrip, Some(IndexFormat::Uint16)),
        Some(IndexFormat::Uint16)
    );
    assert_eq!(
        strip_index_format(Topology::TriangleStrip, Some(IndexFormat::Uint32)),
        Some(IndexFormat::Uint32)
    );
    // A non-indexed strip binds no index buffer, so it declares none.
    assert_eq!(strip_index_format(Topology::LineStrip, None), None);
    assert_eq!(strip_index_format(Topology::TriangleStrip, None), None);
}

#[test]
fn a_non_strip_pipeline_declares_no_index_width() {
    use wgpu::{IndexFormat, PrimitiveTopology as Topology};
    // `wgpu` rejects a strip index format on a non-strip topology, so an
    // indexed mesh of one must not leak its format into the pipeline.
    for topology in [
        Topology::PointList,
        Topology::LineList,
        Topology::TriangleList,
    ] {
        assert_eq!(
            strip_index_format(topology, Some(IndexFormat::Uint16)),
            None
        );
        assert_eq!(
            strip_index_format(topology, Some(IndexFormat::Uint32)),
            None
        );
    }
}

#[test]
fn a_device_with_base_vertex_puts_the_offset_on_the_draw() {
    let offset = IndexOffset::new(true, 7);

    assert_eq!(offset.draw_base_vertex, 7, "the draw carries the offset");
    let (format, data) = offset.pack(&[0, 1, 2]);
    assert_eq!(
        format,
        wgpu::IndexFormat::Uint16,
        "the indices stay mesh-local, so they keep their width"
    );
    assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
}

#[test]
fn a_device_without_base_vertex_bakes_the_offset_into_the_indices() {
    // WebGL2's GLES 3.0 has no `base_vertex`, and the backend would panic
    // rather than emulate it, so the offset has to be in the indices.
    let offset = IndexOffset::new(false, 7);

    assert_eq!(offset.draw_base_vertex, 0, "the draw must not name one");
    let (format, data) = offset.pack(&[0, 1, 2]);
    assert_eq!(format, wgpu::IndexFormat::Uint16);
    assert_eq!(packed_indices(format, &data, 3), vec![7, 8, 9]);
}

#[test]
fn a_mesh_at_the_start_of_the_pool_needs_no_offset_either_way() {
    // The common case, and the one where both paths agree: nothing is
    // baked and the draw's `base_vertex` is zero.
    for supports_base_vertex in [true, false] {
        let offset = IndexOffset::new(supports_base_vertex, 0);
        assert_eq!(offset.baked, 0);
        assert_eq!(offset.draw_base_vertex, 0);
        let (format, data) = offset.pack(&[0, 1, 2]);
        assert_eq!(format, wgpu::IndexFormat::Uint16);
        assert_eq!(packed_indices(format, &data, 3), vec![0, 1, 2]);
    }
}

#[test]
fn a_baked_offset_can_widen_the_indices() {
    // Baking is what forces the wider format: an index that fitted `u16`
    // before the offset may not after it, and the compression is decided on
    // the baked values.
    let indices = [0u32, 1, 2];
    let offset = IndexOffset::new(false, u32::from(u16::MAX));
    let (format, data) = offset.pack(&indices);
    assert_eq!(
        format,
        wgpu::IndexFormat::Uint32,
        "an index past `u16::MAX` cannot be narrowed"
    );
    assert_eq!(packed_indices(format, &data, 3), vec![65535, 65536, 65537]);
}

#[test]
fn packed_indices_are_padded_to_the_pool_alignment() {
    // The pool hands ranges out at `COPY_BUFFER_ALIGNMENT`, so `pack` pads
    // what it writes and its length is the size the pool is asked for.
    let offset = IndexOffset::new(true, 0);

    // Three `Uint16` indices are 6 bytes, rounded up to the alignment.
    let (format, data) = offset.pack(&[0, 1, 2]);
    assert_eq!(format, wgpu::IndexFormat::Uint16);
    assert_eq!(
        data.len(),
        (3 * size_of::<u16>()).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
    );
    assert!(
        data[3 * size_of::<u16>()..].iter().all(|&byte| byte == 0),
        "the padding is zeroed"
    );

    // Many indices round up by that alignment without a whole extra block
    // when they already land on one.
    let indices: Vec<u32> = (0..(u16::MAX as u32 - 1)).collect();
    let (format, data) = offset.pack(&indices);
    assert_eq!(format, wgpu::IndexFormat::Uint16);
    assert_eq!(
        data.len(),
        (indices.len() * size_of::<u16>()).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize)
    );
}

/// The first `count` indices [`IndexOffset::pack`] wrote, as `u32`s.
///
/// The bytes past them are the pool-alignment padding, which the caller
/// sizes to but does not read.
fn packed_indices(format: wgpu::IndexFormat, bytes: &[u8], count: usize) -> Vec<u32> {
    match format {
        wgpu::IndexFormat::Uint16 => bytes[..count * size_of::<u16>()]
            .as_chunks::<{ size_of::<u16>() }>()
            .0
            .iter()
            .map(|index| u32::from(U16::from(*index).get()))
            .collect(),
        wgpu::IndexFormat::Uint32 => bytes[..count * size_of::<u32>()]
            .as_chunks::<{ size_of::<u32>() }>()
            .0
            .iter()
            .map(|index| U32::from(*index).get())
            .collect(),
    }
}
