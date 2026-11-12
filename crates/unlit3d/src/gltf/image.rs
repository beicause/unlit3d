//! Image upload helpers: format selection and texel conversion.
//!
//! These turn a decoded `gltf::image::Data` into the bytes and
//! `wgpu::TextureFormat` a texture upload needs.

use core::mem::size_of;

use zerocopy::byteorder::little_endian::{F32, U16};
use zerocopy::{FromBytes, IntoBytes};

use super::MAX_TEXEL_BYTES;

/// Round `value` up to the next multiple of `alignment`.
pub(super) fn align_up(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

/// The uploaded form of a glTF image: its texture format, and whether a
/// sampler is allowed to filter it.
///
/// glTF images arrive in ten pixel layouts — one to four channels of 8-bit,
/// 16-bit or 32-bit float — and each uploads in the four-channel format of its
/// bit depth, widened where the source has fewer channels. Widening is what
/// lets the sampler decode an 8-bit upload; see the format picker below.
///
/// `filtering` is what the material's sampler and bind-group layout are built
/// around: a 32-bit float texture is an `unfilterable-float` unless the device
/// has `Features::FLOAT32_FILTERABLE`, and an unfilterable texture may only be
/// bound to a non-filtering sampler, which is a different bind-group layout
/// from a filtering one.
#[derive(Clone, Copy, Debug)]
pub(super) struct ImageFormat {
    pub(super) format: wgpu::TextureFormat,
    pub(super) filtering: bool,
}

/// The bytes one texel of an upload format occupies.
pub(super) fn texel_bytes(format: wgpu::TextureFormat) -> u32 {
    format
        .block_copy_size(Some(wgpu::TextureAspect::All))
        .expect("a sampled color format has a texel block size")
}

/// Pick the upload format for a decoded glTF image.
///
/// Every layout uploads in the four-channel format of its bit depth: an 8-bit
/// one lands in `Rgba8UnormSrgb`, a 16-bit one in `Rgba16Float`, and a 32-bit
/// float one in `Rgba32Float`. The widening is what makes an 8-bit upload
/// correct — WebGPU attaches the sRGB transfer function only to four-channel
/// formats, so a one- or two-channel image cannot decode itself, but
/// `encode_texel` fans its luma out to RGB and the sampler decodes the texel
/// as it would an RGB one. The float formats are linear data, which the glTF
/// spec asks no transfer function of, and none of them has an sRGB variant to
/// need.
///
/// Half float needs no device feature at all — unlike `R16Unorm` and friends,
/// which need `Features::TEXTURE_FORMAT_16BIT_NORM` — so an image never has to
/// fall back to a narrower format. `Rgba32Float` is the only upload whose
/// filtering depends on the device.
pub(super) fn image_format(data: &gltf::image::Data, device: &wgpu::Device) -> ImageFormat {
    use gltf::image::Format as F;
    use wgpu::TextureFormat as T;
    let format = match data.format {
        F::R8 | F::R8G8 | F::R8G8B8 | F::R8G8B8A8 => T::Rgba8UnormSrgb,
        F::R16 | F::R16G16 | F::R16G16B16 | F::R16G16B16A16 => T::Rgba16Float,
        F::R32G32B32FLOAT | F::R32G32B32A32FLOAT => T::Rgba32Float,
    };
    let filtering = matches!(
        format.sample_type(Some(wgpu::TextureAspect::All), Some(device.features())),
        Some(wgpu::TextureSampleType::Float { filterable: true })
    );
    ImageFormat { format, filtering }
}

/// How much of a mip chain an uploaded texture carries.
///
/// [`Self::Full`] is the default: a minified texture with one level aliases
/// whatever the sampler is set to, so every upload builds the chain down to
/// one texel. [`Self::Off`] uploads the base level only, and [`Self::Levels`]
/// caps the chain — for a caller that knows how far its textures are minified,
/// or wants the upload itself cheap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MipChain {
    /// Every level, down to one texel.
    #[default]
    Full,
    /// The base level only.
    Off,
    /// At most this many levels, base level included. A chain is never
    /// shorter than its base level, so zero still uploads one.
    Levels(u32),
}

/// One level of a texture's mip chain: its size, and the padded rows a
/// `Queue::write_texture` of that level takes.
pub(super) struct EncodedMip {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes_per_row: u32,
    pub(super) rows: Vec<u8>,
}

impl EncodedMip {
    /// Pad `work`, the tightly packed `width` by `height` image in its working
    /// form, to the copy stride a buffer-to-texture copy asks for.
    fn from_work(
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        texel: usize,
        work_texel: usize,
        work: &[u8],
    ) -> Self {
        let texels = width as usize * height as usize;
        let encoded = encode_work(format, texels, work_texel, work);
        Self::new(width, height, texel, &encoded)
    }

    /// Pad `texels`, the tightly packed `width` by `height` image, to the copy
    /// stride a buffer-to-texture copy asks for.
    fn new(width: u32, height: u32, texel: usize, texels: &[u8]) -> Self {
        let bytes_per_row = align_up(width * texel as u32, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let row_bytes = width as usize * texel;
        let stride = bytes_per_row as usize;
        let mut rows = vec![0u8; stride * height as usize];
        for (packed, row) in texels
            .chunks_exact(row_bytes)
            .zip(rows.chunks_exact_mut(stride))
        {
            row[..row_bytes].copy_from_slice(packed);
        }
        Self {
            width,
            height,
            bytes_per_row,
            rows,
        }
    }
}

/// How many levels a `width` by `height` mip chain has: down to one texel, as
/// far as `mips` allows.
pub(super) fn mip_level_count(width: u32, height: u32, mips: MipChain) -> u32 {
    let natural = (32 - width.max(height).leading_zeros()).max(1);
    match mips {
        MipChain::Full => natural,
        MipChain::Off => 1,
        MipChain::Levels(levels) => natural.min(levels.max(1)),
    }
}

/// The mip chain of `data` in `format`, level zero first.
///
/// Level zero is the image itself. Every level below it is an average of the
/// level above, which is the level a minified texture is sampled from: without
/// the chain, a texture covering fewer pixels than it has samples aliases,
/// whatever the sampler is set to.
///
/// Each average is a box filter taken in the representation that makes
/// averaging mean what it does to light — an sRGB image averages in linear
/// light, a half float one in single precision, and the rest in their own
/// texels. Averaging a stored encoding instead would darken or brighten the
/// level, which is the error the chain exists to avoid. The chain runs as far
/// as `mips` allows — see [`MipChain`].
pub(super) fn encode_mip_chain(
    data: &gltf::image::Data,
    format: wgpu::TextureFormat,
    mips: MipChain,
) -> Vec<EncodedMip> {
    let texel = texel_bytes(format) as usize;
    let level_count = mip_level_count(data.width, data.height, mips);
    let base = encode_compact(data, format);
    let mut chain = Vec::with_capacity(level_count as usize);
    chain.push(EncodedMip::new(data.width, data.height, texel, &base));
    if level_count == 1 {
        return chain;
    }

    let pixel_type = work_pixel_type(format);
    let mut work = decode_work(format, base);
    let mut resizer = fast_image_resize::Resizer::new();
    let options = fast_image_resize::ResizeOptions {
        algorithm: fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Box),
        // A base color texture is not premultiplied by its alpha, so an average
        // must not weight the color channels by it.
        mul_div_alpha: false,
        ..Default::default()
    };

    // Two buffers alternate between the level being averaged and the level
    // being written, so the chain allocates once per image rather than once
    // per level.
    let work_texel = pixel_type.size();
    let (mut width, mut height) = (data.width, data.height);
    let mut level = Vec::new();
    for _ in 1..level_count {
        let (next_width, next_height) = ((width / 2).max(1), (height / 2).max(1));
        let next_size = next_width as usize * next_height as usize * work_texel;
        if level.len() < next_size {
            level.resize(next_size, 0);
        }

        let work_size = width as usize * height as usize * work_texel;
        let source =
            fast_image_resize::images::ImageRef::new(width, height, &work[..work_size], pixel_type)
                .expect("the working buffer holds the level above");
        let mut destination = fast_image_resize::images::Image::from_slice_u8(
            next_width,
            next_height,
            &mut level[..next_size],
            pixel_type,
        )
        .expect("the level buffer holds the level it is resized into");
        resizer
            .resize(&source, &mut destination, &options)
            .expect("a box filter resizes an image of its own pixel type");

        std::mem::swap(&mut work, &mut level);
        (width, height) = (next_width, next_height);
        chain.push(EncodedMip::from_work(
            format, width, height, texel, work_texel, &work,
        ));
    }
    chain
}

/// Encode `data`'s pixels as a tightly packed `format` image: one texel after
/// another, with no row padding.
///
/// A channel the source does not carry — the alpha of a three-channel image,
/// say — is filled with an opaque one, and every value is clamped to `0..=1`
/// on the way in, which is all the wider float formats need.
fn encode_compact(data: &gltf::image::Data, format: wgpu::TextureFormat) -> Vec<u8> {
    let (width, height) = (data.width as usize, data.height as usize);
    let pixel_bytes = source_channel_bytes(data.format) * source_channels(data.format);
    let texel = texel_bytes(format) as usize;
    let mut texels = vec![0u8; width * height * texel];
    let mut scratch = [0u8; MAX_TEXEL_BYTES];

    for (index, out) in texels.chunks_exact_mut(texel).enumerate() {
        let pixel = &data.pixels[index * pixel_bytes..];
        out.copy_from_slice(encode_texel(data.format, format, pixel, &mut scratch));
    }
    texels
}

/// The pixel type a format's levels are averaged in.
///
/// An sRGB format widens to 16 bits: averaging sRGB texels takes a decode to
/// linear light first, and 16 bits hold that light finely enough that encoding
/// it back never moves a channel by more than one step of the 8-bit result. A
/// half float format widens to single precision for the reason the sRGB one
/// widens at all — an average of many half floats is not itself a half float —
/// and a single precision format is already there.
fn work_pixel_type(format: wgpu::TextureFormat) -> fast_image_resize::PixelType {
    use fast_image_resize::PixelType as P;
    match format {
        wgpu::TextureFormat::Rgba8UnormSrgb => P::U16x4,
        wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float => P::F32x4,
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// The working bytes of an upload image: what the levels below it are averaged
/// from.
///
/// Takes the packed level by value, so that a format whose upload encoding is
/// already its working form moves its bytes instead of copying them.
fn decode_work(format: wgpu::TextureFormat, texels: Vec<u8>) -> Vec<u8> {
    match format {
        wgpu::TextureFormat::Rgba8UnormSrgb => srgb_to_linear_texels(&texels),
        wgpu::TextureFormat::Rgba16Float => halves_to_floats(&texels),
        wgpu::TextureFormat::Rgba32Float => texels,
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// The upload texels of a level held in `work`, its working form.
///
/// `texels` and `work_texel` say how much of `work` is the level: a working
/// buffer outlives the level it was sized for, so it may hold more bytes than
/// the level they are read as.
fn encode_work(
    format: wgpu::TextureFormat,
    texels: usize,
    work_texel: usize,
    work: &[u8],
) -> Vec<u8> {
    let work = &work[..texels * work_texel];
    match format {
        wgpu::TextureFormat::Rgba8UnormSrgb => linear_to_srgb_texels(work),
        wgpu::TextureFormat::Rgba16Float => floats_to_halves(work),
        wgpu::TextureFormat::Rgba32Float => work.to_vec(),
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// Widen half float texels to single precision.
fn halves_to_floats(texels: &[u8]) -> Vec<u8> {
    let (halves, _) = texels.as_chunks::<2>();
    let mut floats = Vec::with_capacity(halves.len() * size_of::<f32>());
    for half in halves {
        let value = half::f16::from_bits(u16::from_le_bytes(*half)).to_f32();
        floats.extend_from_slice(&value.to_le_bytes());
    }
    floats
}

/// Narrow single precision texels to half float.
fn floats_to_halves(floats: &[u8]) -> Vec<u8> {
    let (floats, _) = floats.as_chunks::<4>();
    let mut halves = Vec::with_capacity(floats.len() * size_of::<u16>());
    for value in floats {
        let bits = half::f16::from_f32(f32::from_le_bytes(*value)).to_bits();
        halves.extend_from_slice(&bits.to_le_bytes());
    }
    halves
}

/// Decode four-channel sRGB texels into 16-bit linear light.
///
/// Alpha is a coverage, not a color, so it is widened without being decoded.
fn srgb_to_linear_texels(texels: &[u8]) -> Vec<u8> {
    let mut linear = Vec::with_capacity(texels.len() * size_of::<u16>());
    for texel in texels.as_chunks::<4>().0 {
        for (slot, byte) in texel.iter().enumerate() {
            let value = if slot == 3 {
                widen(*byte)
            } else {
                quantize_linear(srgb_to_linear(f32::from(*byte) / f32::from(u8::MAX)))
            };
            linear.extend_from_slice(&value.to_le_bytes());
        }
    }
    linear
}

/// Encode 16-bit linear light back into four-channel sRGB texels.
fn linear_to_srgb_texels(linear: &[u8]) -> Vec<u8> {
    let mut texels = Vec::with_capacity(linear.len() / size_of::<u16>());
    for texel in linear.as_chunks::<{ 4 * size_of::<u16>() }>().0 {
        for (slot, value) in texel.as_chunks::<2>().0.iter().enumerate() {
            let value = f32::from(u16::from_le_bytes(*value)) / f32::from(u16::MAX);
            let encoded = if slot == 3 {
                value
            } else {
                linear_to_srgb(value)
            };
            texels.push((encoded.clamp(0.0, 1.0) * f32::from(u8::MAX)).round() as u8);
        }
    }
    texels
}

/// The 16-bit value a `0..=1` number takes.
fn quantize_linear(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

/// Widen an 8-bit channel to 16 bits, keeping both end points exact.
fn widen(value: u8) -> u16 {
    u16::from(value) * (u16::MAX / u16::from(u8::MAX))
}

/// The linear light a `0..=1` sRGB channel stands for.
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The `0..=1` sRGB channel a linear light value encodes as.
fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// The number of bytes one channel of a glTF pixel format occupies.
fn source_channel_bytes(format: gltf::image::Format) -> usize {
    match format {
        gltf::image::Format::R8
        | gltf::image::Format::R8G8
        | gltf::image::Format::R8G8B8
        | gltf::image::Format::R8G8B8A8 => 1,
        gltf::image::Format::R16
        | gltf::image::Format::R16G16
        | gltf::image::Format::R16G16B16
        | gltf::image::Format::R16G16B16A16 => 2,
        gltf::image::Format::R32G32B32FLOAT | gltf::image::Format::R32G32B32A32FLOAT => 4,
    }
}

/// The number of channels a glTF pixel format stores per texel.
fn source_channels(format: gltf::image::Format) -> usize {
    match format {
        gltf::image::Format::R8 | gltf::image::Format::R16 => 1,
        gltf::image::Format::R8G8 | gltf::image::Format::R16G16 => 2,
        gltf::image::Format::R8G8B8 | gltf::image::Format::R16G16B16 => 3,
        gltf::image::Format::R8G8B8A8 | gltf::image::Format::R16G16B16A16 => 4,
        gltf::image::Format::R32G32B32FLOAT => 3,
        gltf::image::Format::R32G32B32A32FLOAT => 4,
    }
}

/// The number of channels an upload format stores per texel.
fn format_channels(format: wgpu::TextureFormat) -> usize {
    match format {
        wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Rgba16Float
        | wgpu::TextureFormat::Rgba32Float => 4,
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

/// Encode one source texel into `format`, returning its bytes.
///
/// `scratch` backs the result, so a caller can reuse it across texels.
pub(super) fn encode_texel<'a>(
    source: gltf::image::Format,
    format: wgpu::TextureFormat,
    pixel: &[u8],
    scratch: &'a mut [u8; MAX_TEXEL_BYTES],
) -> &'a [u8] {
    let channel = source_channel_bytes(source);
    let wanted = format_channels(format);
    for slot in 0..wanted {
        write_channel(format, slot, source_slot(source, channel, slot, pixel), scratch);
    }
    &scratch[..texel_bytes(format) as usize]
}

/// The value `source`'s texel carries for `format`'s `slot`.
///
/// A one-channel source is a luma, which every color slot repeats and an
/// alpha slot keeps opaque; a two-channel one is luma and alpha, so its
/// second channel is alpha. Every upload format is four channels wide — see
/// `image_format` — so the slots below always exist.
fn source_slot(source: gltf::image::Format, channel: usize, slot: usize, pixel: &[u8]) -> f32 {
    use gltf::image::Format as F;
    match (source, slot) {
        (F::R8 | F::R16, 3) => 1.0,
        (F::R8G8 | F::R16G16, 3) => read_channel(source, &pixel[channel..]),
        (F::R8 | F::R16 | F::R8G8 | F::R16G16, _) => read_channel(source, pixel),
        (_, slot) if slot < source_channels(source) => {
            read_channel(source, &pixel[slot * channel..])
        }
        // A channel the source does not carry is an opaque one.
        _ => 1.0,
    }
}

/// One channel of a glTF pixel, as a `0..=1` float.
fn read_channel(format: gltf::image::Format, bytes: &[u8]) -> f32 {
    match format {
        gltf::image::Format::R8
        | gltf::image::Format::R8G8
        | gltf::image::Format::R8G8B8
        | gltf::image::Format::R8G8B8A8 => f32::from(bytes[0]) / f32::from(u8::MAX),
        gltf::image::Format::R16
        | gltf::image::Format::R16G16
        | gltf::image::Format::R16G16B16
        | gltf::image::Format::R16G16B16A16 => {
            // A glTF texel always carries a whole channel, so the read cannot
            // run short of bytes.
            let (value, _) = U16::read_from_prefix(bytes).expect("a glTF texel holds its channels");
            f32::from(value.get()) / f32::from(u16::MAX)
        }
        gltf::image::Format::R32G32B32FLOAT | gltf::image::Format::R32G32B32A32FLOAT => {
            let (value, _) = F32::read_from_prefix(bytes).expect("a glTF texel holds its channels");
            value.get()
        }
    }
}

/// Write one `0..=1` channel of `format`'s `slot` into `scratch`.
fn write_channel(
    format: wgpu::TextureFormat,
    slot: usize,
    value: f32,
    scratch: &mut [u8; MAX_TEXEL_BYTES],
) {
    // `f32::clamp` leaves NaN alone, and a NaN texel would poison every sample
    // it reaches, so fold it to the bottom of the range instead.
    let clamped = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    };
    match format {
        wgpu::TextureFormat::Rgba8UnormSrgb => {
            scratch[slot] = (clamped * f32::from(u8::MAX)).round() as u8;
        }
        wgpu::TextureFormat::Rgba16Float => {
            let bits = half::f16::from_f32(clamped).to_bits();
            scratch[slot * size_of::<u16>()..][..size_of::<u16>()]
                .copy_from_slice(U16::new(bits).as_bytes());
        }
        wgpu::TextureFormat::Rgba32Float => {
            scratch[slot * size_of::<f32>()..][..size_of::<f32>()]
                .copy_from_slice(F32::new(clamped).as_bytes());
        }
        _ => unreachable!("a glTF image uploads in one of the formats above"),
    }
}

#[cfg(test)]
mod tests {
    use super::MAX_TEXEL_BYTES;
    use super::{
        EncodedMip, MipChain, encode_mip_chain, encode_texel, mip_level_count, texel_bytes,
    };

    /// A one-by-one image of `pixels` in the given format.
    fn image(format: gltf::image::Format, pixels: Vec<u8>) -> gltf::image::Data {
        gltf::image::Data {
            pixels,
            format,
            width: 1,
            height: 1,
        }
    }

    /// A `width` by `height` image of `pixels` in the given format.
    fn sized_image(
        format: gltf::image::Format,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> gltf::image::Data {
        gltf::image::Data {
            pixels,
            format,
            width,
            height,
        }
    }

    /// The texels of one level, with its row padding removed.
    fn texels(mip: &EncodedMip, format: wgpu::TextureFormat) -> Vec<u8> {
        let row_bytes = mip.width as usize * texel_bytes(format) as usize;
        mip.rows
            .chunks_exact(mip.bytes_per_row as usize)
            .flat_map(|row| &row[..row_bytes])
            .copied()
            .collect()
    }

    /// The texel `data` encodes into `format`, without its row padding.
    fn encode(format: wgpu::TextureFormat, data: &gltf::image::Data) -> Vec<u8> {
        let texel = texel_bytes(format) as usize;
        let mut out = Vec::new();
        let mut scratch = [0u8; MAX_TEXEL_BYTES];
        out.extend_from_slice(encode_texel(
            data.format,
            format,
            &data.pixels,
            &mut scratch,
        ));
        assert_eq!(out.len(), texel);
        out
    }
    #[test]
    fn a_luma_alpha_image_fans_its_luma_out_and_keeps_its_alpha() {
        use gltf::image::Format as F;
        // The loader decodes a grayscale-alpha PNG into two channels, luma then
        // alpha. The upload widens them into an sRGB RGBA texel whose RGB all
        // carry the luma, so the sampler decodes the color, and whose alpha is
        // the source's second channel.
        let data = image(F::R8G8, vec![0x40, 0x80]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rgba8UnormSrgb, &data),
            vec![0x40, 0x40, 0x40, 0x80]
        );
    }

    #[test]
    fn a_one_channel_image_fans_its_luma_out_and_stays_opaque() {
        use gltf::image::Format as F;
        let data = image(F::R8, vec![0x40]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rgba8UnormSrgb, &data),
            vec![0x40, 0x40, 0x40, 255]
        );
    }

    #[test]
    fn a_sixteen_bit_luma_alpha_image_widens_in_linear_light() {
        use gltf::image::Format as F;
        // A 16-bit texel is linear data — the spec asks no transfer function
        // of a float upload — so the luma goes out to RGB as it is, and the
        // alpha keeps the source's second channel.
        let data = image(F::R16G16, {
            let mut pixels = Vec::new();
            pixels.extend_from_slice(&u16::MAX.to_le_bytes());
            pixels.extend_from_slice(&16384u16.to_le_bytes());
            pixels
        });
        let encoded = encode(wgpu::TextureFormat::Rgba16Float, &data);
        let channels = encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| half::f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect::<Vec<_>>();
        assert_eq!(channels.len(), 4);
        for channel in &channels[..3] {
            assert!((channel - 1.0).abs() < 1e-3, "{channels:?}");
        }
        assert!((channels[3] - 0.25).abs() < 1e-3, "{channels:?}");
    }

    #[test]
    fn a_three_channel_image_gains_an_opaque_alpha() {
        use gltf::image::Format as F;
        let data = image(F::R8G8B8, vec![1, 2, 3]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rgba8UnormSrgb, &data),
            vec![1, 2, 3, 255]
        );
    }

    #[test]
    fn a_sixteen_bit_image_narrows_to_half_float() {
        use gltf::image::Format as F;
        let data = image(F::R16G16B16A16, vec![0x00, 0x80, 0x00, 0x40, 0, 0, 0, 0]);
        let encoded = encode(wgpu::TextureFormat::Rgba16Float, &data);
        // `32768 / 65535` is a half above a half, `16384 / 65535` a quarter,
        // and the trailing zeroes stay zero.
        let channels = encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| half::f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect::<Vec<_>>();
        assert!((channels[0] - 0.5).abs() < 1e-3, "{channels:?}");
        assert!((channels[1] - 0.25).abs() < 1e-3, "{channels:?}");
        assert_eq!(&channels[2..], &[0.0, 0.0]);
    }

    #[test]
    fn every_texel_of_a_wide_image_is_read_from_its_own_bytes() {
        use gltf::image::Format as F;
        // A 16-bit texel holds two bytes per channel, so the second texel of a
        // row starts at byte eight, not at byte four.
        let texel = |red: u16| {
            [red, 0u16, 0u16, u16::MAX]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>()
        };
        let data = sized_image(F::R16G16B16A16, 2, 1, [texel(0), texel(u16::MAX)].concat());
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba16Float, MipChain::Full);
        let level = texels(&chain[0], wgpu::TextureFormat::Rgba16Float);
        let channels = level
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| half::f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect::<Vec<_>>();
        // `0x3c00` is one in half float; a texel read from the wrong byte
        // offset would surface the first texel's zero alpha there instead.
        assert_eq!(&channels[..4], &[0.0, 0.0, 0.0, 1.0], "{channels:?}");
        assert_eq!(&channels[4..], &[1.0, 0.0, 0.0, 1.0], "{channels:?}");
    }
    #[test]
    fn a_float_image_out_of_range_clamps() {
        use gltf::image::Format as F;
        let data = image(F::R32G32B32A32FLOAT, {
            let mut pixels = Vec::new();
            pixels.extend_from_slice(&(-1.0f32).to_le_bytes());
            pixels.extend_from_slice(&0.5f32.to_le_bytes());
            pixels.extend_from_slice(&2.0f32.to_le_bytes());
            pixels.extend_from_slice(&f32::NAN.to_le_bytes());
            pixels
        });
        let encoded = encode(wgpu::TextureFormat::Rgba32Float, &data);
        let channels = encoded
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect::<Vec<_>>();
        assert_eq!(channels, vec![0.0, 0.5, 1.0, 0.0]);
    }

    #[test]
    fn rows_are_padded_to_the_copy_stride() {
        use gltf::image::Format as F;
        let data = sized_image(F::R8G8B8A8, 2, 1, vec![7; 2 * 4]);
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba8UnormSrgb, MipChain::Full);
        let base = &chain[0];
        assert_eq!(base.rows.len(), wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        assert_eq!(&base.rows[..8], &[7; 8]);
        assert_eq!(&base.rows[8..], &vec![0; 256 - 8][..]);
    }

    #[test]
    fn a_chain_descends_to_one_texel() {
        assert_eq!(mip_level_count(1, 1, MipChain::Full), 1);
        assert_eq!(mip_level_count(4, 4, MipChain::Full), 3);
        // The chain follows the longer side down to one texel.
        assert_eq!(mip_level_count(5, 3, MipChain::Full), 3);
        assert_eq!(mip_level_count(1, 8, MipChain::Full), 4);
    }

    #[test]
    fn a_chain_is_only_as_long_as_its_configuration_allows() {
        assert_eq!(mip_level_count(4, 4, MipChain::Off), 1);
        assert_eq!(mip_level_count(8, 8, MipChain::Levels(2)), 2);
        // A cap below two levels is still the base level alone, and a cap
        // above the natural chain is the natural chain.
        assert_eq!(mip_level_count(8, 8, MipChain::Levels(0)), 1);
        assert_eq!(mip_level_count(4, 4, MipChain::Levels(9)), 3);
    }

    #[test]
    fn a_chain_holds_every_level_of_the_image() {
        use gltf::image::Format as F;
        let data = sized_image(F::R8G8B8A8, 5, 3, vec![0x40; 5 * 3 * 4]);
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba8UnormSrgb, MipChain::Full);
        let sizes = chain
            .iter()
            .map(|mip| (mip.width, mip.height))
            .collect::<Vec<_>>();
        // A level is half the one above it, rounded down, and never zero.
        assert_eq!(sizes, vec![(5, 3), (2, 1), (1, 1)]);
    }

    #[test]
    fn an_image_with_one_texel_has_no_level_below_it() {
        use gltf::image::Format as F;
        let data = image(F::R8G8B8A8, vec![9, 8, 7, 255]);
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba8UnormSrgb, MipChain::Full);
        assert_eq!(chain.len(), 1);
        assert_eq!(
            texels(&chain[0], wgpu::TextureFormat::Rgba8UnormSrgb),
            vec![9, 8, 7, 255]
        );
    }

    #[test]
    fn a_capped_chain_carries_only_its_levels() {
        use gltf::image::Format as F;
        let data = sized_image(F::R8G8B8A8, 4, 4, vec![0x40; 4 * 4 * 4]);
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba8UnormSrgb, MipChain::Levels(2));
        assert_eq!(chain.len(), 2);
        let sizes = chain
            .iter()
            .map(|mip| (mip.width, mip.height))
            .collect::<Vec<_>>();
        assert_eq!(sizes, vec![(4, 4), (2, 2)]);
    }

    #[test]
    fn an_srgb_format_averages_in_linear_light() {
        use gltf::image::Format as F;
        // Half black and half white is half the light, which an sRGB encoding
        // of that light puts at 188, not at the 128 a middle of the two
        // encodings would give.
        let data = sized_image(
            F::R8G8B8A8,
            2,
            2,
            vec![
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
        );
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba8UnormSrgb, MipChain::Full);
        assert_eq!(chain.len(), 2);
        let level = texels(&chain[1], wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(level[3], 255, "alpha is a coverage, not a color");
        for channel in &level[..3] {
            assert!((187..=189).contains(channel), "{level:?}");
        }
    }

    #[test]
    fn a_half_float_format_averages_in_single_precision() {
        use gltf::image::Format as F;
        // A glTF 16-bit texel is a normalized `u16`, and the upload widens it
        // to half float. Two white texels and two black ones average to a half
        // in the red channel, which is not a value any of the four holds — so
        // the average cannot be taken in half float and stay exact.
        let texel = |red: u16| {
            [red, 0u16, 0u16, u16::MAX]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>()
        };
        let pixels = [texel(u16::MAX), texel(u16::MAX), texel(0), texel(0)].concat();
        let data = sized_image(F::R16G16B16A16, 2, 2, pixels);
        let chain = encode_mip_chain(&data, wgpu::TextureFormat::Rgba16Float, MipChain::Full);
        let level = texels(&chain[1], wgpu::TextureFormat::Rgba16Float);
        let channels = level
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| half::f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect::<Vec<_>>();
        assert_eq!(channels.len(), 4);
        assert!((channels[0] - 0.5).abs() < 1e-3, "{channels:?}");
        assert_eq!(&channels[1..3], &[0.0, 0.0], "{channels:?}");
        assert!((channels[3] - 1.0).abs() < 1e-3, "{channels:?}");
    }
}
