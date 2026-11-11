use super::*;

/// The per-vertex channels the built-in shader variant reads.
///
/// These are not part of [`LitOptions`]: they are a property of the geometry
/// rather than of anything a caller picks, so a pipeline reads them off the
/// vertex buffers its draws are recorded with — every draw of a mesh carries
/// the channels that mesh's buffers hold, so there is nothing to specialize on
/// and nothing for a caller to get wrong. A family resolves them into
/// [`LitVariant::channels`].
///
/// The two halves are [`crate::mesh`]'s own channel types, so the attributes a
/// pipeline declares and the bytes a packed mesh writes come from one
/// description and cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LitVertexChannels {
    /// The position stream: whether it carries a position at all — and in
    /// which encoding — plus the joint pair that rides along with it.
    ///
    /// A stream with no position leaves the geometry a single point at the
    /// instance origin, which is what point, particle and impostor draws want:
    /// the per-instance data alone places the vertex.
    pub position: PositionStreamChannels,
    /// The secondary stream's channels: a UV in either encoding, a vertex
    /// color, a normal, or any combination.
    pub secondary: SecondaryFlags,
}

impl LitVertexChannels {
    /// The channels of a layout that carries none: no position, no joints, and
    /// an empty secondary stream.
    ///
    /// A variant built from this reads no per-vertex attribute at all — the
    /// geometry is a single point at the instance origin — which is what point,
    /// particle and impostor draws want. The instance record is unconditional,
    /// so a `VertexInput` struct always has members.
    pub const fn empty() -> Self {
        Self {
            position: PositionStreamChannels {
                position: None,
                joints: false,
            },
            secondary: SecondaryFlags::empty(),
        }
    }

    /// The channels one vertex-buffer slot carries.
    ///
    /// A channel is identified by its shader location, which is stable across
    /// variants. Only the built-in pipeline's own slots are read —
    /// [`PRIMARY_SLOT`] and [`SECONDARY_SLOT`] — so any other slot, the
    /// per-instance one included, contributes nothing.
    pub fn for_vertex_buffer(slot: u32, layout: &VertexBufferLayoutDesc) -> Self {
        let mut channels = Self::empty();
        match slot {
            PRIMARY_SLOT => {
                for attribute in &layout.attributes {
                    if attribute.shader_location == location::POSITION {
                        channels.position.position = Some(match attribute.format {
                            wgpu::VertexFormat::Float32x3 => ChannelEncoding::UncompressedPosition,
                            wgpu::VertexFormat::Float32x2 => ChannelEncoding::PlanarPosition,
                            _ => ChannelEncoding::CompressedPosition,
                        });
                    }
                    // The joint pair is part of this stream rather than a
                    // stream of its own: the indices are the evidence, and the
                    // weights follow them.
                    if attribute.shader_location == location::JOINTS {
                        channels.position.joints = true;
                    }
                }
            }
            SECONDARY_SLOT => {
                for attribute in &layout.attributes {
                    match attribute.shader_location {
                        location::UV => {
                            channels.secondary |= SecondaryFlags::UV;
                            if attribute.format == wgpu::VertexFormat::Float32x2 {
                                channels.secondary |= SecondaryFlags::UNCOMPRESSED_UV;
                            }
                        }
                        location::COLOR => channels.secondary |= SecondaryFlags::COLOR,
                        location::NORMAL => channels.secondary |= SecondaryFlags::NORMAL,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        channels
    }

    /// The channels a whole vertex layout carries: the union of
    /// [`Self::for_vertex_buffer`] over `layout`'s slots.
    pub fn for_vertex_layout(layout: &[(u32, VertexBufferLayoutDesc)]) -> Self {
        let mut channels = Self::empty();
        for (slot, slot_layout) in layout {
            let slot_channels = Self::for_vertex_buffer(*slot, slot_layout);
            if slot_channels.position.position.is_some() {
                channels.position.position = slot_channels.position.position;
            }
            channels.position.joints |= slot_channels.position.joints;
            channels.secondary |= slot_channels.secondary;
        }
        channels
    }
}

/// How a sampled base-color texel expands into the fragment's color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BaseColorChannels {
    /// The texel is a color in all four channels.
    #[default]
    Rgba,
    /// The texel is luminance: one channel, expanded into every color channel.
    ///
    /// The sampled value is decoded from sRGB on the way, because a
    /// one-channel image cannot be uploaded in an sRGB format: WebGPU offers
    /// the transfer function only for the four-channel `Rgba8UnormSrgb` and
    /// `Bgra8UnormSrgb`, so a luminance texture carries the encoded value and
    /// the shader decodes it.
    Luminance,
    /// The texel is luminance and alpha: two channels holding `l` and `a`,
    /// sampled into the color and the alpha channel.
    ///
    /// The luminance half decodes from sRGB exactly as [`Self::Luminance`]
    /// does; alpha is coverage, so it never converts.
    LuminanceAlpha,
}
