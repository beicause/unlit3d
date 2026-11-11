use super::*;
use crate::util::test::noop_device;

/// The element size of a `mat4x4<f32>`: four texels of `Rgba32Float`.
const MATRIX_ELEMENT: u64 = 64;
/// The element size of a struct-shaped element: three `Rgba32Float` texels,
/// a size that is neither the matrix element above nor a scalar.
const STRUCT_ELEMENT: u64 = 48;
/// The element size of a bare `f32` array.
const SCALAR_ELEMENT: u64 = 4;
/// A texture limit no test array reaches.
const ROOMY: u32 = 2048;

#[test]
fn a_matrix_element_reads_as_four_texels_of_four_lanes() {
    let layout = TexelArrayLayout::new(MATRIX_ELEMENT, 8, ROOMY);

    assert_eq!(layout.texel_format, wgpu::TextureFormat::Rgba32Float);
    assert_eq!(layout.lanes_per_texel, 4);
    assert_eq!(layout.elements_per_row, 4, "a row is 256 bytes");
    assert_eq!(layout.row_bytes(), 256);
    assert_eq!(layout.width, 16, "four elements of four texels each");
    assert_eq!(layout.height, 2);
}

#[test]
fn a_scalar_element_reads_as_one_lane_per_texel() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 128, ROOMY);

    assert_eq!(layout.texel_format, wgpu::TextureFormat::R32Float);
    assert_eq!(layout.lanes_per_texel, 1);
    assert_eq!(layout.elements_per_row, 64, "a row is 256 bytes");
    assert_eq!(layout.width, 64);
    assert_eq!(layout.height, 2);
}

#[test]
fn a_48_byte_element_still_reaches_the_row_alignment() {
    // 48 bytes does not divide 256, so the row takes 16 elements — 768
    // bytes, three alignment units — instead of the 6 that would fit in one.
    let layout = TexelArrayLayout::new(STRUCT_ELEMENT, 32, ROOMY);

    assert_eq!(layout.texel_format, wgpu::TextureFormat::Rgba32Float);
    assert_eq!(layout.elements_per_row, 16);
    assert_eq!(layout.row_bytes(), 768);
    assert!(
        layout
            .row_bytes()
            .is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64)
    );
    assert_eq!(layout.width, 48, "sixteen elements of three texels each");
    assert_eq!(layout.height, 2);
}

#[test]
fn every_element_size_produces_an_aligned_row() {
    for element_size in [4u64, 8, 12, 16, 20, 32, 36, 48, 64, 96, 128, 256] {
        let layout = TexelArrayLayout::new(element_size, 4, ROOMY);
        assert!(
            layout
                .row_bytes()
                .is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64),
            "a {element_size}-byte element gave a {}-byte row",
            layout.row_bytes()
        );
        assert_eq!(
            layout.width % layout.lanes_per_texel,
            0,
            "a {element_size}-byte element's row holds whole texels"
        );
    }
}

#[test]
fn an_element_that_is_not_a_whole_number_of_lanes_panics() {
    let result = std::panic::catch_unwind(|| TexelArrayLayout::new(6, 1, ROOMY));

    assert!(
        result.is_err(),
        "6 bytes is not a whole number of `f32` lanes"
    );
}

#[test]
fn an_element_wider_than_the_texture_limit_panics() {
    // A `Rgba32Float` texel is 16 bytes, so a 2048-texel row holds 32768
    // bytes; one 16-byte lane more than that has no row wide enough.
    let widest = u64::from(ROOMY) * 16;
    assert!(std::panic::catch_unwind(|| TexelArrayLayout::new(widest, 1, ROOMY)).is_ok());

    let result = std::panic::catch_unwind(|| TexelArrayLayout::new(widest + 16, 1, ROOMY));

    assert!(result.is_err());
}

#[test]
fn an_empty_array_still_has_one_row() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 0, ROOMY);

    assert_eq!(layout.height, 1, "a texture cannot have a zero-height axis");
    assert_eq!(layout.capacity(), 64);
}

#[test]
fn a_capacity_beyond_one_column_grows_the_height() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 64 * 8, ROOMY);

    assert_eq!(
        layout.elements_per_row, 64,
        "the row stays at one alignment"
    );
    assert_eq!(layout.width, 64);
    assert_eq!(layout.height, 8, "the rows carry the extra elements");
}

#[test]
fn a_capacity_beyond_the_texture_limit_widens_the_row() {
    // 2048 rows of 64 elements is the limit; one more element forces the
    // row to widen instead of the height to grow past the limit.
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 64 * u64::from(ROOMY) + 1, ROOMY);

    assert_eq!(layout.elements_per_row, 128, "the row doubled");
    assert_eq!(layout.width, 128);
    assert_eq!(layout.height, 1025, "and the elements now fit");
    assert!(layout.height <= ROOMY);
}

#[test]
fn a_capacity_that_cannot_fit_at_all_panics() {
    // One scalar per texel caps the array at 2048 rows of 2048 elements,
    // so one element more than that has nowhere to go.
    let fit = u64::from(ROOMY) * u64::from(ROOMY);
    assert!(std::panic::catch_unwind(|| TexelArrayLayout::new(SCALAR_ELEMENT, fit, ROOMY)).is_ok());

    let result = std::panic::catch_unwind(|| TexelArrayLayout::new(SCALAR_ELEMENT, fit + 1, ROOMY));

    assert!(result.is_err());
}

#[test]
fn packing_pads_only_to_the_last_populated_row() {
    let layout = TexelArrayLayout::new(STRUCT_ELEMENT, 32, ROOMY);
    let elements: Vec<u8> = (0..6 * STRUCT_ELEMENT).map(|i| i as u8).collect();
    let mut out = Vec::new();

    layout.pack(&elements, &mut out);

    assert_eq!(
        out.len() as u64,
        layout.row_bytes(),
        "six of the sixteen elements in one row need one row's bytes"
    );
    assert_eq!(
        &out[..elements.len()],
        &elements[..],
        "the elements are contiguous from the start"
    );
    assert!(
        out[elements.len()..].iter().all(|byte| *byte == 0),
        "the unused tail of the last row is zeroed"
    );
}

#[test]
fn a_partial_upload_copies_only_its_populated_rows() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 256, ROOMY);
    let elements = [7u8; 65 * SCALAR_ELEMENT as usize];
    let mut scratch = Vec::new();
    layout.pack(&elements, &mut scratch);
    let rows = layout.rows_for(elements.len() as u64 / SCALAR_ELEMENT);

    assert_eq!(rows, 2);
    assert_eq!(scratch.len() as u64, u64::from(rows) * layout.row_bytes());
    assert!(u64::from(rows) < u64::from(layout.height));
}

#[test]
fn packing_a_full_row_writes_exactly_one_row() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 128, ROOMY);

    let mut out = Vec::new();
    layout.pack(&vec![7; 64 * SCALAR_ELEMENT as usize], &mut out);

    assert_eq!(out.len() as u64, layout.row_bytes());
    assert_eq!(out.len(), 256);
}

#[test]
fn packing_more_than_one_row_spans_the_rows_it_needs() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 128, ROOMY);

    let mut out = Vec::new();
    layout.pack(&vec![7; 65 * SCALAR_ELEMENT as usize], &mut out);

    assert_eq!(out.len(), 512, "the 65th element starts a second row");
}

#[test]
fn packing_again_shorter_clears_the_previous_tail() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 128, ROOMY);
    let mut out = Vec::new();
    layout.pack(&[0xff; 64 * SCALAR_ELEMENT as usize], &mut out);

    layout.pack(&[1; 2 * SCALAR_ELEMENT as usize], &mut out);

    assert!(
        out[2 * SCALAR_ELEMENT as usize..]
            .iter()
            .all(|byte| *byte == 0),
        "an element the shorter upload does not reach is cleared"
    );
}

#[test]
fn packing_reuses_the_same_allocation() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 16, ROOMY);
    let mut out = Vec::new();
    layout.pack(&[1; 16], &mut out);
    let capacity = out.capacity();

    layout.pack(&[2; 16], &mut out);

    assert_eq!(out.capacity(), capacity, "the scratch buffer is reused");
}

#[test]
#[should_panic(expected = "not a whole number")]
fn packing_a_partial_element_panics() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 16, ROOMY);
    layout.pack(&[0; 6], &mut Vec::new());
}

#[test]
#[should_panic(expected = "room for")]
fn packing_more_than_capacity_panics() {
    let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 4, ROOMY);
    let over = (layout.capacity() + 1) * SCALAR_ELEMENT;
    layout.pack(&vec![0; over as usize], &mut Vec::new());
}

#[test]
fn an_upload_records_the_copy_it_was_given() {
    // The noop device validates nothing, so what this holds is that each path
    // records its copy without panicking: two elements on the texel path, none
    // at all, and then the buffer path for the same bytes.
    let (device, _queue) = noop_device();
    let mut encoder = device.create_command_encoder(&Default::default());

    let mut texel = TexelArray::new(&device, Some("test.texel"), MATRIX_ELEMENT, 4, ROOMY);
    texel.upload(&device, &mut encoder, &[0; 2 * MATRIX_ELEMENT as usize]);
    let mut empty = TexelArray::new(&device, Some("test.texel"), MATRIX_ELEMENT, 4, ROOMY);
    empty.upload(&device, &mut encoder, &[]);

    let elements = [1u8; 2 * SCALAR_ELEMENT as usize];
    let mut buffered = Array::new(&device, Some("test.buffer"), SCALAR_ELEMENT, 4, None);
    buffered.upload(&device, &mut encoder, &elements);

    let _ = encoder.finish();
}

#[test]
fn an_array_holds_a_buffer_or_a_texture_by_its_limit() {
    let (device, _queue) = noop_device();

    let buffered = Array::new(&device, Some("test"), SCALAR_ELEMENT, 4, None);
    assert!(!buffered.is_texel());
    assert_eq!(buffered.capacity(), 4);
    assert!(matches!(buffered.handle(), ArrayHandle::Buffer(_)));

    let texel = Array::new(&device, Some("test"), SCALAR_ELEMENT, 4, Some(ROOMY));
    assert!(texel.is_texel());
    assert_eq!(texel.capacity(), 64, "a row of 64 holds the four elements");
    assert!(matches!(texel.handle(), ArrayHandle::Texel(_)));
}

#[test]
fn growing_an_array_replaces_its_resource_only_when_it_is_too_small() {
    let (device, _queue) = noop_device();
    let mut array = Array::new(&device, Some("test"), SCALAR_ELEMENT, 4, None);

    assert!(
        !array.grow_to(&device, Some("test"), 4),
        "already has the room"
    );
    assert!(
        array.grow_to(&device, Some("test"), 8),
        "the array outgrew it"
    );
    assert_eq!(array.capacity(), 8);
}

#[test]
fn growing_a_texel_array_stays_on_the_texel_path() {
    let (device, _queue) = noop_device();
    let mut array = Array::new(&device, Some("test"), MATRIX_ELEMENT, 4, Some(ROOMY));
    assert!(array.is_texel());

    assert!(array.grow_to(&device, Some("test"), 64));

    assert!(
        array.is_texel(),
        "a grown array keeps the resource it can read"
    );
    assert_eq!(array.capacity(), 64);
}

#[test]
fn a_layout_entry_follows_the_resource_the_array_holds() {
    let (device, _queue) = noop_device();
    let buffered = Array::new(&device, Some("test"), MATRIX_ELEMENT, 4, None);
    let texel = Array::new(&device, Some("test"), MATRIX_ELEMENT, 4, Some(ROOMY));

    let buffer_entry =
        buffered
            .handle()
            .layout_entry(2, wgpu::ShaderStages::VERTEX, MATRIX_ELEMENT);
    assert!(matches!(buffer_entry.ty, wgpu::BindingType::Buffer { .. }));

    let texel_entry = texel
        .handle()
        .layout_entry(2, wgpu::ShaderStages::VERTEX, MATRIX_ELEMENT);
    assert!(matches!(texel_entry.ty, wgpu::BindingType::Texture { .. }));
}

#[test]
fn a_bind_group_accepts_an_array_of_either_path() {
    // Both constructors hand back a resource a bind group takes: a buffer on
    // the storage path, a texture on the texel path, and the texel array's own
    // entry on the path it chose for itself.
    let (device, _queue) = noop_device();
    let texel = TexelArray::new(&device, Some("test.texel_array"), MATRIX_ELEMENT, 4, ROOMY);
    let texel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("test.texel_array"),
        entries: &[TexelArrayLayout::binding(0, wgpu::ShaderStages::VERTEX)],
    });
    let _ = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test.texel_array"),
        layout: &texel_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: texel.binding_resource(),
        }],
    });

    for max_dimension in [None, Some(ROOMY)] {
        let array = Array::new(
            &device,
            Some("test.array"),
            MATRIX_ELEMENT,
            4,
            max_dimension,
        );
        let entry = array
            .handle()
            .layout_entry(0, wgpu::ShaderStages::VERTEX, MATRIX_ELEMENT);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test.array"),
            entries: &[entry],
        });
        let _ = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test.array"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: array.handle().binding_resource(),
            }],
        });
    }
}
