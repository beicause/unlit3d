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
/// 16-bit or 32-bit float — and there is a GPU format of nearly the same shape
/// for each, so an image keeps its channels and its precision instead of being
/// widened to RGBA8.
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

impl ImageFormat {
    /// The bytes one texel of the format occupies.
    pub(super) fn texel_bytes(self) -> u32 {
        texel_bytes(self.format)
    }
}

/// The bytes one texel of an upload format occupies.
pub(super) fn texel_bytes(format: wgpu::TextureFormat) -> u32 {
    format
        .block_copy_size(Some(wgpu::TextureAspect::All))
        .expect("a sampled color format has a texel block size")
}

/// Pick the upload format for a decoded glTF image.
///
/// The 8-bit layouts keep their channel count, and their pixels are uploaded
/// in the sRGB format of that width so sampling decodes them — which is what
/// the glTF spec asks of a base-color texture. `R8G8B8` widens to
/// `Rgba8UnormSrgb` because there is no three-channel sRGB format; `R8G8`
/// stays two channels, which is the luma/alpha pair the glTF loader decodes a
/// grayscale-alpha PNG into.
///
/// The 16-bit and 32-bit layouts upload as half and single precision floats.
/// Half float needs no device feature at all — unlike `R16Unorm` and friends,
/// which need `Features::TEXTURE_FORMAT_16BIT_NORM` — so an image never has to
/// fall back to a narrower format. `Rgba32Float` is the only upload whose
/// filtering depends on the device.
pub(super) fn image_format(data: &gltf::image::Data, device: &wgpu::Device) -> ImageFormat {
    use gltf::image::Format as F;
    use wgpu::TextureFormat as T;
    let format = match data.format {
        F::R8 => T::R8Unorm,
        F::R8G8 => T::Rg8Unorm,
        // There is no three-channel sRGB format, so the three-channel layouts
        // widen by one opaque channel.
        F::R8G8B8 => T::Rgba8UnormSrgb,
        F::R8G8B8A8 => T::Rgba8UnormSrgb,
        F::R16 => T::R16Float,
        F::R16G16 => T::Rg16Float,
        // Nor a three-channel float one.
        F::R16G16B16 => T::Rgba16Float,
        F::R16G16B16A16 => T::Rgba16Float,
        F::R32G32B32FLOAT => T::Rgba32Float,
        F::R32G32B32A32FLOAT => T::Rgba32Float,
    };
    let filtering = matches!(
        format.sample_type(Some(wgpu::TextureAspect::All), Some(device.features())),
        Some(wgpu::TextureSampleType::Float { filterable: true })
    );
    ImageFormat { format, filtering }
}

/// Encode `data`'s pixels as the rows of a `format` texture.
///
/// A channel the source does not carry — the alpha of a three-channel image,
/// say — is filled with an opaque one, and every value is clamped to `0..=1`
/// on the way in, which is all the wider float formats need. Each row is
/// padded to the 256-byte stride a buffer-to-texture copy asks for.
pub(super) fn encode_rows(data: &gltf::image::Data, format: wgpu::TextureFormat) -> Vec<u8> {
    let (width, height) = (data.width as usize, data.height as usize);
    let channels = source_channels(data.format);
    let texel = texel_bytes(format) as usize;
    let bytes_per_row = align_up(
        width as u32 * texel as u32,
        wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
    ) as usize;
    let mut rows = vec![0u8; bytes_per_row * height];
    let mut scratch = [0u8; MAX_TEXEL_BYTES];

    for (y, out) in rows.chunks_exact_mut(bytes_per_row).enumerate() {
        for x in 0..width {
            let pixel = &data.pixels[(y * width + x) * channels..];
            let bytes = encode_texel(data.format, format, pixel, &mut scratch);
            out[x * texel..][..texel].copy_from_slice(bytes);
        }
    }
    rows
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
        wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R16Float => 1,
        wgpu::TextureFormat::Rg8Unorm | wgpu::TextureFormat::Rg16Float => 2,
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
    let available = source_channels(source);
    for slot in 0..wanted {
        // A channel the source does not carry is an opaque one.
        let value = if slot < available {
            read_channel(source, &pixel[slot * channel..])
        } else {
            1.0
        };
        write_channel(format, slot, value, scratch);
    }
    &scratch[..texel_bytes(format) as usize]
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
        wgpu::TextureFormat::R8Unorm
        | wgpu::TextureFormat::Rg8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb => {
            scratch[slot] = (clamped * f32::from(u8::MAX)).round() as u8;
        }
        wgpu::TextureFormat::R16Float
        | wgpu::TextureFormat::Rg16Float
        | wgpu::TextureFormat::Rgba16Float => {
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
    use super::{encode_rows, encode_texel, texel_bytes};

    /// A one-by-one image of `pixels` in the given format.
    fn image(format: gltf::image::Format, pixels: Vec<u8>) -> gltf::image::Data {
        gltf::image::Data {
            pixels,
            format,
            width: 1,
            height: 1,
        }
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
    fn a_luma_alpha_image_keeps_luma_in_every_channel_slot_it_has() {
        use gltf::image::Format as F;
        // The loader decodes a grayscale-alpha PNG into two channels, luma then
        // alpha, and `Rg8Unorm` keeps both — the bug this replaces wrote them
        // out as red and green.
        let data = image(F::R8G8, vec![0x40, 0x80]);
        assert_eq!(
            encode(wgpu::TextureFormat::Rg8Unorm, &data),
            vec![0x40, 0x80]
        );
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
        let data = gltf::image::Data {
            pixels: vec![7; 2 * 4],
            format: F::R8G8B8A8,
            width: 2,
            height: 1,
        };
        let rows = encode_rows(&data, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(rows.len(), wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        assert_eq!(&rows[..8], &[7; 8]);
        assert_eq!(&rows[8..], &vec![0; 256 - 8][..]);
    }
}
