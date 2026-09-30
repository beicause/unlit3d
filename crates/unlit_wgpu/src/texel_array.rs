//! Texture-backed arrays, for devices whose shaders cannot read storage
//! buffers.
//!
//! WebGL2 has no storage buffers at all: a device created with WebGL2's limits
//! reports `max_storage_buffers_per_shader_stage == 0`, and a bind-group layout
//! naming one is rejected outright. An array that would otherwise live in a
//! `var<storage, read>` still has to be readable per vertex, and the one
//! facility WebGL2 does have for that is a texture read — `textureLoad` is
//! available in every stage, and a float texture's texels can be read as the
//! `f32` values the array held.
//!
//! A [`TexelArray`] is that translation: a flat array of fixed-size elements
//! stored in a 2D texture, addressed by element index, with the element's bytes
//! laid out exactly as the storage buffer laid them out. Because the byte
//! layout is the same, the CPU packing is shared: the same element bytes are
//! uploaded whether the shader reads them as a buffer or as texels.
//!
//! # Layout
//!
//! The texture holds `f32` lanes in texel order. An element occupies a whole
//! number of texels, and a row holds a whole number of elements, so an element
//! never straddles a row boundary. The element index therefore maps to a texel
//! index by a single multiplication, and the flat index maps to a texel
//! coordinate by dividing by the row width — which is the texture's own width,
//! so the shader reads it back with `textureDimensions` rather than being told
//! it as a constant:
//!
//! ```wgsl
//! let width = textureDimensions(tex).x;
//! let texel = element * TEXELS_PER_ELEMENT;
//! let value = textureLoad(tex, vec2<i32>(i32(texel % width), i32(texel / width)), 0);
//! ```
//!
//! # Texel format
//!
//! An element whose size is a whole number of 16-byte rows is stored in
//! `Rgba32Float`, four `f32` lanes per texel — a `mat4x4<f32>`, for instance,
//! is four texels, one per column. Any other element size is stored one `f32`
//! per texel in `R32Float`. Both are readable in every stage without the
//! `FLOAT32_FILTERABLE` feature: filtering is what that feature gates, and
//! `textureLoad` does not filter.
//!
//! Neither format needs a bitcast of the values. They are read as the `f32`s
//! they are, which is why the formats are float rather than the `u32` a storage
//! buffer would have held: a shader reading a `u32` texel back would have to
//! bitcast every lane, and the lanes are the same bits either way. Only a
//! `u32` *member* of an element needs one, and that is the shader's concern.
//!
//! # Rows and padding
//!
//! A row is always a whole number of [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`]
//! bytes, which is what a buffer-to-texture copy demands, so an upload can be
//! staged and recorded into the frame's own encoder rather than costing a
//! submission of its own. The row width is in *elements*, not bytes: the row is
//! exactly `elements_per_row * element_size` bytes, so the only padding is the
//! unused tail of the last populated row. An element never straddles a row, so
//! growing the row width never splits one.

use crate::resources::{TextureExt, TextureView};
use crate::staging::StagingBuffer;

/// A flat array of fixed-size elements, as a bindable resource.
///
/// A device with storage buffers holds an array in one; a device without them —
/// WebGL2 — holds the same bytes in a [`TexelArray`] the shader reads with
/// `textureLoad`. A caller that has to work on both builds its bind group from
/// this handle rather than from a buffer, so the two paths differ in how the
/// array is uploaded and in nothing else.
///
/// See the [module docs](self) for how the texel path lays the bytes out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrayHandle {
    /// A storage buffer holding the whole array.
    Buffer(wgpu::Buffer),
    /// A texture holding the whole array.
    Texel(TextureView),
}

impl ArrayHandle {
    /// The bind-group resource that binds this array.
    pub fn binding_resource(&self) -> wgpu::BindingResource<'_> {
        match self {
            Self::Buffer(buffer) => buffer.as_entire_binding(),
            Self::Texel(view) => wgpu::BindingResource::TextureView(view.view()),
        }
    }

    /// The layout entry binding an array of `element_size`-byte elements held
    /// this way.
    ///
    /// This is the Rust half of the shader's interface: the composed variant
    /// declares a storage buffer on the buffer path and a non-filterable float
    /// texture on the texel path, and the two have to agree.
    pub fn layout_entry(
        &self,
        binding: u32,
        visibility: wgpu::ShaderStages,
        element_size: u64,
    ) -> wgpu::BindGroupLayoutEntry {
        match self {
            Self::Buffer(_) => wgpu::BindGroupLayoutEntry {
                binding,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: Some(core::num::NonZeroU64::new(element_size).expect(
                        "an element has a non-zero size, which is a valid binding minimum",
                    )),
                },
                count: None,
            },
            Self::Texel(_) => TexelArrayLayout::binding(binding, visibility),
        }
    }
}

/// Where an [`Array`]'s elements live.
enum ArrayStorage {
    /// A storage buffer, with the staging buffer uploads reach it through.
    Buffer {
        /// The buffer holding every element.
        buffer: wgpu::Buffer,
        /// The staging buffer the bytes are written into before the copy.
        staging: StagingBuffer,
    },
    /// A texture, which carries its own staging and row padding.
    ///
    /// Boxed because a [`TexelArray`] holds a texture, its view and a scratch
    /// row buffer, and an [`Array`] is stored inline by its owner: the pointer
    /// keeps the buffer path from paying for the texel path's footprint.
    Texel(Box<TexelArray>),
}

/// A growable flat array of fixed-size elements, held in whichever resource the
/// device can read.
///
/// This is the device-independent front end to the [module](self): a caller
/// sizes an array by its element count, uploads the same packed bytes on either
/// path, and binds it through [`Self::handle`] without knowing which resource
/// it landed in. Pass `Some(max_texture_dimension_2d)` to put it in a texture
/// and `None` to put it in a storage buffer.
pub struct Array {
    /// The resource the elements live in.
    storage: ArrayStorage,
    /// Bytes per element.
    element_size: u64,
    /// The texture limit on the texel path, `None` on the buffer path.
    max_dimension: Option<u32>,
}

impl Array {
    /// Allocate an array with room for `capacity` elements of `element_size`
    /// bytes.
    ///
    /// `max_dimension` is `Some` of the device's
    /// [`wgpu::Limits::max_texture_dimension_2d`] to hold the elements in a
    /// texture, or `None` to hold them in a storage buffer.
    ///
    /// # Panics
    ///
    /// Under the conditions [`TexelArray::new`] panics, on the texel path.
    pub fn new(
        device: &wgpu::Device,
        label: Option<&str>,
        element_size: u64,
        capacity: u64,
        max_dimension: Option<u32>,
    ) -> Self {
        let capacity = capacity.max(1);
        let storage = match max_dimension {
            Some(max_dimension) => ArrayStorage::Texel(Box::new(TexelArray::new(
                device,
                label,
                element_size,
                capacity,
                max_dimension,
            ))),
            None => ArrayStorage::Buffer {
                buffer: create_array_buffer(device, label, element_size, capacity),
                staging: StagingBuffer::new(),
            },
        };
        Self {
            storage,
            element_size,
            max_dimension,
        }
    }

    /// Bytes per element.
    pub fn element_size(&self) -> u64 {
        self.element_size
    }

    /// How many elements fit without growing.
    pub fn capacity(&self) -> u64 {
        match &self.storage {
            ArrayStorage::Buffer { buffer, .. } => buffer.size() / self.element_size,
            ArrayStorage::Texel(texel) => texel.layout().capacity(),
        }
    }

    /// Whether the elements are held in a texture rather than a buffer.
    pub fn is_texel(&self) -> bool {
        matches!(self.storage, ArrayStorage::Texel(_))
    }

    /// The bindable handle to the current resource.
    ///
    /// Cloned rather than borrowed so a caller can move it into the resource
    /// graph, which is what makes a replaced array mark the bind groups built
    /// from it dirty.
    pub fn handle(&self) -> ArrayHandle {
        match &self.storage {
            ArrayStorage::Buffer { buffer, .. } => ArrayHandle::Buffer(buffer.clone()),
            ArrayStorage::Texel(texel) => ArrayHandle::Texel(texel.view().clone()),
        }
    }

    /// Grow to hold at least `capacity` elements, returning whether the
    /// resource was replaced.
    ///
    /// A replacement is what a caller has to publish: every bind group built
    /// from the old handle is stale and has to be rebuilt. A no-op when the
    /// array already has the room.
    ///
    /// # Panics
    ///
    /// Under the conditions [`TexelArray::new`] panics, on the texel path.
    pub fn grow_to(&mut self, device: &wgpu::Device, label: Option<&str>, capacity: u64) -> bool {
        if capacity <= self.capacity() {
            return false;
        }
        self.storage = match self.max_dimension {
            Some(max_dimension) => ArrayStorage::Texel(Box::new(TexelArray::new(
                device,
                label,
                self.element_size,
                capacity,
                max_dimension,
            ))),
            None => ArrayStorage::Buffer {
                buffer: create_array_buffer(device, label, self.element_size, capacity),
                staging: StagingBuffer::new(),
            },
        };
        true
    }

    /// Upload `elements` — whole elements, laid out as the storage buffer would
    /// have held them — recording the copy into `encoder`.
    ///
    /// # Panics
    ///
    /// If `elements` is not a whole number of elements, or holds more than
    /// [`Self::capacity`] of them.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        elements: &[u8],
    ) {
        match &mut self.storage {
            ArrayStorage::Buffer { buffer, staging } => {
                staging.write(device, encoder, buffer, 0, elements);
            }
            ArrayStorage::Texel(texel) => texel.upload(device, encoder, elements),
        }
    }

    /// Upload `elements` immediately, for an array written once rather than
    /// every frame.
    ///
    /// This mirrors [`wgpu::Queue::write_buffer`] and
    /// [`wgpu::Queue::write_texture`]: the bytes reach the resource through a
    /// temporary buffer wgpu allocates and submits for the caller, so there is
    /// no encoder to batch into. Prefer [`Self::upload`] for data that changes
    /// every frame, where a reused staging buffer costs nothing per frame.
    ///
    /// # Panics
    ///
    /// If `elements` is not a whole number of elements, or holds more than
    /// [`Self::capacity`] of them.
    pub fn write(&mut self, queue: &wgpu::Queue, elements: &[u8]) {
        match &mut self.storage {
            ArrayStorage::Buffer { buffer, .. } => queue.write_buffer(buffer, 0, elements),
            ArrayStorage::Texel(texel) => texel.write(queue, elements),
        }
    }
}

/// Create the storage buffer an array's elements live in.
fn create_array_buffer(
    device: &wgpu::Device,
    label: Option<&str>,
    element_size: u64,
    capacity: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label,
        size: capacity.max(1) * element_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A texel array's shape and texel format.
///
/// Derived from an element size and a capacity; see the [module
/// docs](self) for how the row width follows from the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexelArrayLayout {
    /// The format the texture is created with.
    texel_format: wgpu::TextureFormat,
    /// `f32` lanes per texel: 1 for `R32Float`, 4 for `Rgba32Float`.
    lanes_per_texel: u32,
    /// Bytes per element.
    element_size: u64,
    /// How many elements one row holds.
    elements_per_row: u32,
    /// The texture's width in texels.
    width: u32,
    /// The texture's height in texels.
    height: u32,
}

impl TexelArrayLayout {
    /// Lay out `capacity` elements of `element_size` bytes in a texture no
    /// larger than `max_dimension` on either axis.
    ///
    /// `max_dimension` is the device's
    /// [`wgpu::Limits::max_texture_dimension_2d`].
    ///
    /// # Panics
    ///
    /// If `element_size` is not a whole number of `f32` lanes, or if `capacity`
    /// elements of that size cannot be stored in a texture within
    /// `max_dimension`.
    pub fn new(element_size: u64, capacity: u64, max_dimension: u32) -> Self {
        assert!(
            element_size > 0 && element_size.is_multiple_of(size_of::<f32>() as u64),
            "a texel array element of {element_size} bytes is not a whole number of `f32` lanes"
        );
        // Four lanes per texel whenever the element is a whole number of
        // 16-byte rows, so a `mat4x4<f32>` or a 48-byte struct reads as a few
        // whole texels instead of one texel per scalar.
        let lanes_per_texel = if element_size.is_multiple_of(16) {
            4
        } else {
            1
        };
        let texel_format = match lanes_per_texel {
            4 => wgpu::TextureFormat::Rgba32Float,
            _ => wgpu::TextureFormat::R32Float,
        };
        let texel_bytes = u64::from(lanes_per_texel) * size_of::<f32>() as u64;
        let texels_per_element = element_size / texel_bytes;
        // One element has to fit in one row, so an element wider than the
        // texture limit cannot be stored at all: the row-width search below
        // could only clamp to one element per row and overshoot the limit.
        assert!(
            texels_per_element <= u64::from(max_dimension),
            "a {element_size}-byte element spans {texels_per_element} texels, \
             wider than the {max_dimension}-texel texture limit"
        );

        let capacity = capacity.max(1);
        let elements_per_row =
            row_elements(element_size, texels_per_element, capacity, max_dimension);
        let width = u32::try_from(u64::from(elements_per_row) * texels_per_element)
            .expect("a row width within `max_dimension` fits a `u32`");
        let height = u32::try_from(capacity.div_ceil(u64::from(elements_per_row)))
            .expect("a height within `max_dimension` fits a `u32`")
            .max(1);

        Self {
            texel_format,
            lanes_per_texel,
            element_size,
            elements_per_row,
            width,
            height,
        }
    }

    /// The format the texture is created with.
    pub fn texel_format(&self) -> wgpu::TextureFormat {
        self.texel_format
    }

    /// `f32` lanes one texel holds.
    pub fn lanes_per_texel(&self) -> u32 {
        self.lanes_per_texel
    }

    /// Bytes per element.
    pub fn element_size(&self) -> u64 {
        self.element_size
    }

    /// How many elements one row holds.
    pub fn elements_per_row(&self) -> u32 {
        self.elements_per_row
    }

    /// The texture's width in texels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The texture's height in texels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes one row spans, padding included.
    ///
    /// Always a whole number of [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] bytes.
    pub fn row_bytes(&self) -> u64 {
        u64::from(self.elements_per_row) * self.element_size
    }

    /// How many elements fit without growing the texture.
    pub fn capacity(&self) -> u64 {
        u64::from(self.elements_per_row) * u64::from(self.height)
    }

    /// The texture size to create.
    pub fn texture_size(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        }
    }

    /// How many rows `element_count` elements occupy.
    fn rows_for(&self, element_count: u64) -> u32 {
        u32::try_from(element_count.div_ceil(u64::from(self.elements_per_row)))
            .expect("no more rows than the texture has")
            .max(1)
    }

    /// Write `elements` into `out` as the row-padded image the texture holds.
    ///
    /// `elements` is the packed element bytes — the same bytes a storage buffer
    /// would have been uploaded, and the same ones [`Self::element_size`]
    /// describes one of. `out` is reused across calls, so a steady frame
    /// reallocates nothing. Only the rows the elements reach are written; the
    /// rest of the texture is left as it was, which is sound because the shader
    /// addresses nothing past the element count the caller uploaded.
    ///
    /// # Panics
    ///
    /// If `elements` is not a whole number of elements, or holds more than
    /// [`Self::capacity`] of them.
    pub fn pack(&self, elements: &[u8], out: &mut Vec<u8>) {
        assert!(
            (elements.len() as u64).is_multiple_of(self.element_size),
            "{} bytes are not a whole number of {}-byte elements",
            elements.len(),
            self.element_size
        );
        assert!(
            elements.len() as u64 <= self.capacity() * self.element_size,
            "{} bytes hold more than the {} elements the array has room for",
            elements.len(),
            self.capacity()
        );
        let element_count = elements.len() as u64 / self.element_size;
        let used = u64::from(self.rows_for(element_count)) * self.row_bytes();
        out.clear();
        out.extend_from_slice(elements);
        out.resize(used as usize, 0);
    }

    /// The bind-group layout entry that binds one of these as a texture.
    pub fn binding(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                // Neither `R32Float` nor `Rgba32Float` is filterable without
                // `FLOAT32_FILTERABLE`, and `textureLoad` never filters.
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }
    }
}

/// The number of elements a row holds: enough for a row to reach the copy
/// alignment, then doubled until the elements fit within `max_dimension` rows.
fn row_elements(
    element_size: u64,
    texels_per_element: u64,
    capacity: u64,
    max_dimension: u32,
) -> u32 {
    let alignment = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    // The fewest elements whose bytes make up whole alignment units, so the row
    // needs no padding between it and the next and a buffer-to-texture copy
    // accepts it as it stands.
    let base = alignment / gcd(element_size, alignment);
    // The width is `elements_per_row * texels_per_element`, so the row cannot
    // hold more elements than that many texels allow.
    let max_elements_per_row = (u64::from(max_dimension) / texels_per_element).max(1);
    assert!(
        base <= max_elements_per_row,
        "a {element_size}-byte element needs a {}-texel row, wider than the \
         {max_dimension}-texel texture limit",
        base * texels_per_element
    );

    let mut elements_per_row = base;
    while capacity.div_ceil(elements_per_row) > u64::from(max_dimension) {
        assert!(
            elements_per_row < max_elements_per_row,
            "{capacity} elements of {element_size} bytes do not fit in a \
             {max_dimension}x{max_dimension} texture"
        );
        // Doubling keeps the row a whole number of alignment units.
        elements_per_row = (elements_per_row * 2).min(max_elements_per_row);
    }
    u32::try_from(elements_per_row).expect("a row within `max_dimension` fits a `u32`")
}

/// The greatest common divisor of `a` and `b`.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Create the texture a layout describes.
fn create_texture(
    device: &wgpu::Device,
    label: Option<&str>,
    layout: &TexelArrayLayout,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label,
        size: layout.texture_size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.texel_format(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A flat array of fixed-size elements stored in a texture.
///
/// See the [module docs](self) for what it is for and how it is laid out.
pub struct TexelArray {
    /// The texture the elements live in.
    texture: wgpu::Texture,
    /// The whole texture as one view, which is what a bind group binds.
    view: TextureView,
    /// The array's shape and format.
    layout: TexelArrayLayout,
    /// The staging buffer the row-padded image reaches the texture through,
    /// reused across frames like every other upload.
    staging: StagingBuffer,
    /// The row-padded image the last upload was assembled in, kept so a steady
    /// frame reallocates nothing.
    scratch: Vec<u8>,
}

impl TexelArray {
    /// Allocate an array with room for `capacity` elements of `element_size`
    /// bytes.
    ///
    /// `max_dimension` is the device's
    /// [`wgpu::Limits::max_texture_dimension_2d`]; the texture never exceeds it
    /// on either axis.
    ///
    /// # Panics
    ///
    /// Under the conditions [`TexelArrayLayout::new`] panics.
    pub fn new(
        device: &wgpu::Device,
        label: Option<&str>,
        element_size: u64,
        capacity: u64,
        max_dimension: u32,
    ) -> Self {
        let layout = TexelArrayLayout::new(element_size, capacity, max_dimension);
        let texture = create_texture(device, label, &layout);
        let view = TextureExt::create_view(&texture, &wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            layout,
            staging: StagingBuffer::new(),
            scratch: Vec::new(),
        }
    }

    /// The texture the elements live in.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// The view a bind group binds.
    pub fn view(&self) -> &TextureView {
        &self.view
    }

    /// The array's shape and format.
    pub fn layout(&self) -> &TexelArrayLayout {
        &self.layout
    }

    /// The bind-group resource that binds this array.
    pub fn binding_resource(&self) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::TextureView(self.view.view())
    }

    /// Upload `elements` — whole elements, laid out as the storage buffer would
    /// have held them — into the texture, recording the copy into `encoder`.
    ///
    /// The bytes go through a reused staging buffer, so a steady frame
    /// allocates nothing and the copy is batched with the rest of the frame's
    /// work rather than costing a submission of its own.
    ///
    /// # Panics
    ///
    /// Under the conditions [`TexelArrayLayout::pack`] panics.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        elements: &[u8],
    ) {
        self.layout.pack(elements, &mut self.scratch);
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(
                u32::try_from(self.layout.row_bytes())
                    .expect("a row no wider than the texture limit fits a `u32`"),
            ),
            rows_per_image: Some(self.layout.height),
        };
        self.staging.write_texture(
            device,
            encoder,
            &self.texture,
            layout,
            self.layout.texture_size(),
            &self.scratch,
        );
    }

    /// Upload `elements` through the queue, for data written once.
    ///
    /// The rows are re-aligned by [`wgpu::Queue::write_texture`], so unlike
    /// [`Self::upload`] this needs no copy alignment — but it also costs a
    /// submission of its own.
    ///
    /// # Panics
    ///
    /// Under the conditions [`TexelArrayLayout::pack`] panics.
    pub fn write(&mut self, queue: &wgpu::Queue, elements: &[u8]) {
        self.layout.pack(elements, &mut self.scratch);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.scratch,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(
                    u32::try_from(self.layout.row_bytes())
                        .expect("a row no wider than the texture limit fits a `u32`"),
                ),
                rows_per_image: Some(self.layout.height),
            },
            self.layout.texture_size(),
        );
    }
}

#[cfg(test)]
mod tests {
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

        assert_eq!(layout.texel_format(), wgpu::TextureFormat::Rgba32Float);
        assert_eq!(layout.lanes_per_texel(), 4);
        assert_eq!(layout.elements_per_row(), 4, "a row is 256 bytes");
        assert_eq!(layout.row_bytes(), 256);
        assert_eq!(layout.width(), 16, "four elements of four texels each");
        assert_eq!(layout.height(), 2);
    }

    #[test]
    fn a_scalar_element_reads_as_one_lane_per_texel() {
        let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 128, ROOMY);

        assert_eq!(layout.texel_format(), wgpu::TextureFormat::R32Float);
        assert_eq!(layout.lanes_per_texel(), 1);
        assert_eq!(layout.elements_per_row(), 64, "a row is 256 bytes");
        assert_eq!(layout.width(), 64);
        assert_eq!(layout.height(), 2);
    }

    #[test]
    fn a_48_byte_element_still_reaches_the_row_alignment() {
        // 48 bytes does not divide 256, so the row takes 16 elements — 768
        // bytes, three alignment units — instead of the 6 that would fit in one.
        let layout = TexelArrayLayout::new(STRUCT_ELEMENT, 32, ROOMY);

        assert_eq!(layout.texel_format(), wgpu::TextureFormat::Rgba32Float);
        assert_eq!(layout.elements_per_row(), 16);
        assert_eq!(layout.row_bytes(), 768);
        assert!(
            layout
                .row_bytes()
                .is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64)
        );
        assert_eq!(layout.width(), 48, "sixteen elements of three texels each");
        assert_eq!(layout.height(), 2);
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
                layout.width() % layout.lanes_per_texel(),
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

        assert_eq!(
            layout.height(),
            1,
            "a texture cannot have a zero-height axis"
        );
        assert_eq!(layout.capacity(), 64);
    }

    #[test]
    fn a_capacity_beyond_one_column_grows_the_height() {
        let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 64 * 8, ROOMY);

        assert_eq!(
            layout.elements_per_row(),
            64,
            "the row stays at one alignment"
        );
        assert_eq!(layout.width(), 64);
        assert_eq!(layout.height(), 8, "the rows carry the extra elements");
    }

    #[test]
    fn a_capacity_beyond_the_texture_limit_widens_the_row() {
        // 2048 rows of 64 elements is the limit; one more element forces the
        // row to widen instead of the height to grow past the limit.
        let layout = TexelArrayLayout::new(SCALAR_ELEMENT, 64 * u64::from(ROOMY) + 1, ROOMY);

        assert_eq!(layout.elements_per_row(), 128, "the row doubled");
        assert_eq!(layout.width(), 128);
        assert_eq!(layout.height(), 1025, "and the elements now fit");
        assert!(layout.height() <= ROOMY);
    }

    #[test]
    fn a_capacity_that_cannot_fit_at_all_panics() {
        // One scalar per texel caps the array at 2048 rows of 2048 elements,
        // so one element more than that has nowhere to go.
        let fit = u64::from(ROOMY) * u64::from(ROOMY);
        assert!(
            std::panic::catch_unwind(|| TexelArrayLayout::new(SCALAR_ELEMENT, fit, ROOMY)).is_ok()
        );

        let result =
            std::panic::catch_unwind(|| TexelArrayLayout::new(SCALAR_ELEMENT, fit + 1, ROOMY));

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
    fn an_array_creates_a_texture_a_bind_group_can_bind() {
        let (device, _queue) = noop_device();
        let array = TexelArray::new(&device, Some("test::texel_array"), MATRIX_ELEMENT, 4, ROOMY);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test::texel_array"),
            entries: &[TexelArrayLayout::binding(0, wgpu::ShaderStages::VERTEX)],
        });
        let _bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test::texel_array"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: array.binding_resource(),
            }],
        });
    }

    #[test]
    fn an_upload_records_a_copy_the_device_accepts() {
        let (device, _queue) = noop_device();
        let mut array =
            TexelArray::new(&device, Some("test::texel_array"), MATRIX_ELEMENT, 4, ROOMY);
        let mut encoder = device.create_command_encoder(&Default::default());

        array.upload(&device, &mut encoder, &[0; 2 * MATRIX_ELEMENT as usize]);

        let _ = encoder.finish();
    }

    #[test]
    fn an_upload_of_an_empty_array_records_a_copy_too() {
        let (device, _queue) = noop_device();
        let mut array =
            TexelArray::new(&device, Some("test::texel_array"), MATRIX_ELEMENT, 4, ROOMY);
        let mut encoder = device.create_command_encoder(&Default::default());

        array.upload(&device, &mut encoder, &[]);

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
    fn an_array_uploads_the_same_bytes_on_either_path() {
        let (device, _queue) = noop_device();
        let elements = [1u8; 2 * SCALAR_ELEMENT as usize];

        let mut buffered = Array::new(&device, Some("test"), SCALAR_ELEMENT, 4, None);
        let mut texel = Array::new(&device, Some("test"), SCALAR_ELEMENT, 4, Some(ROOMY));
        let mut encoder = device.create_command_encoder(&Default::default());
        buffered.upload(&device, &mut encoder, &elements);
        texel.upload(&device, &mut encoder, &elements);
        let _ = encoder.finish();
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

        let texel_entry =
            texel
                .handle()
                .layout_entry(2, wgpu::ShaderStages::VERTEX, MATRIX_ELEMENT);
        assert!(matches!(texel_entry.ty, wgpu::BindingType::Texture { .. }));
    }

    #[test]
    fn a_bind_group_accepts_an_array_of_either_path() {
        let (device, _queue) = noop_device();
        for max_dimension in [None, Some(ROOMY)] {
            let array = Array::new(&device, Some("test"), MATRIX_ELEMENT, 4, max_dimension);
            let entry = array
                .handle()
                .layout_entry(0, wgpu::ShaderStages::VERTEX, MATRIX_ELEMENT);
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("test::array"),
                entries: &[entry],
            });
            let _bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test::array"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: array.handle().binding_resource(),
                }],
            });
        }
    }
}
