//! A reused vertex buffer holding one family's per-frame instance records.
//!
//! A pipeline family that draws with instance-stepped attributes owns one
//! [`InstanceBuffer`]: every frame it packs one record per visible instance and
//! uploads them through the frame's encoder. Keeping the stream per family is
//! what lets a caller's family carry its own per-instance data without sharing
//! a record layout or a buffer with the built-in unlit family.

use crate::staging::StagingBuffer;

/// Where a family's instance stream binds and how wide one record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceStreamDesc {
    /// The vertex-buffer slot the stream binds at.
    pub slot: u32,
    /// Bytes per instance record. Zero means the family binds no instance
    /// stream at all.
    pub array_stride: u32,
}

impl InstanceStreamDesc {
    /// The description of a family that binds no instance stream.
    pub const NONE: Self = Self {
        slot: 0,
        array_stride: 0,
    };
}

/// A reused vertex buffer holding one frame's packed instance records.
///
/// The buffer grows geometrically and is reused between frames, so a steady
/// scene allocates nothing. Bytes reach it through a [`StagingBuffer`], the
/// same path the camera and frame globals take.
pub struct InstanceBuffer {
    /// Where the stream binds and how wide one record is.
    desc: InstanceStreamDesc,
    /// The buffer holding the last upload, allocated on the first one.
    buffer: Option<wgpu::Buffer>,
    /// How many records the buffer can hold.
    capacity: u32,
    /// The staging buffers the uploads pass through.
    staging: StagingBuffer,
}

impl InstanceBuffer {
    /// Create an empty stream for `desc`, allocating no GPU buffer until the
    /// first upload.
    ///
    /// # Panics
    ///
    /// If `desc` names a stride that is not a multiple of
    /// [`wgpu::VERTEX_ALIGNMENT`], which a vertex-buffer layout cannot declare.
    pub fn new(desc: InstanceStreamDesc) -> Self {
        assert!(
            desc.array_stride == 0
                || desc
                    .array_stride
                    .is_multiple_of(wgpu::VERTEX_ALIGNMENT as u32),
            "an instance record's stride must be a multiple of `VERTEX_ALIGNMENT`"
        );
        Self {
            desc,
            buffer: None,
            capacity: 0,
            staging: StagingBuffer::new(),
        }
    }

    /// Where the stream binds and how wide one record is.
    pub fn desc(&self) -> InstanceStreamDesc {
        self.desc
    }

    /// The buffer holding the last upload, or `None` before the first one.
    pub fn buffer(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }

    /// Upload `records` as this frame's instance stream.
    ///
    /// A stream with no stride uploads nothing, and so does an empty one: a
    /// family with no visible instances binds no buffer, exactly as one that
    /// declares no stream does.
    ///
    /// # Panics
    ///
    /// If `records.len()` is not a whole number of records.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        records: &[u8],
    ) {
        let stride = self.desc.array_stride;
        if stride == 0 || records.is_empty() {
            return;
        }
        assert_eq!(
            records.len() % stride as usize,
            0,
            "instance records are whole strides"
        );
        let count = (records.len() / stride as usize) as u32;
        self.reserve(device, count);
        let buffer = self.buffer.as_ref().expect("the buffer was just reserved");
        self.staging.write(device, encoder, buffer, 0, records);
    }

    /// Grow the buffer geometrically so it can hold `count` records.
    fn reserve(&mut self, device: &wgpu::Device, count: u32) {
        if count <= self.capacity {
            return;
        }
        // Grow by a factor rather than exactly, so a scene that adds one
        // instance per frame does not reallocate every frame.
        let new_cap = count.max(self.capacity + self.capacity / 2).max(1);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unlit_wgpu::instance_stream"),
            size: u64::from(new_cap) * u64::from(self.desc.array_stride),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.buffer = Some(buffer);
        self.capacity = new_cap;
    }
}
