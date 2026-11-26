use super::MAX_TEXEL_BYTES;
use super::{EncodedMip, MipChain, encode_mip_chain, encode_texel, mip_level_count, texel_bytes};

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
fn a_float_image_keeps_its_high_dynamic_range() {
    use gltf::image::Format as F;
    // A 32-bit float texture is linear data a shader may shade with, so a
    // value above one is a highlight rather than an error. Only NaN is
    // folded, and negative light goes to zero because a texture holds no
    // such sample.
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
    assert_eq!(channels, vec![-1.0, 0.5, 2.0, 0.0]);
}

#[test]
fn a_normalized_image_clamps_into_its_range() {
    use gltf::image::Format as F;
    // The 8-bit upload has nowhere to put a value outside `0..=1`, so the
    // clamp is the format's own rather than the loader's.
    let data = image(F::R8G8B8, vec![0, 128, 255]);
    assert_eq!(
        encode(wgpu::TextureFormat::Rgba8UnormSrgb, &data),
        vec![0, 128, 255, 255]
    );
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
    let chain = encode_mip_chain(
        &data,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        MipChain::Levels(2),
    );
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
