use anyhow::{Context, Result, bail};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, ImageFormat};
use tuck_core::Image;

/// Encodes BGRA pixels as PNG; opaque images are stored without an alpha channel.
pub fn encode_png(image: &Image) -> Result<Vec<u8>> {
    if image.width == 0 || image.height == 0 {
        bail!("cannot store an empty image");
    }
    let (channels, color) =
        if image.is_opaque() { (3, ExtendedColorType::Rgb8) } else { (4, ExtendedColorType::Rgba8) };
    let mut samples = Vec::with_capacity(image.data.len() / 4 * channels);
    for pixel in image.data.chunks_exact(4) {
        samples.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        if channels == 4 {
            samples.push(pixel[3]);
        }
    }
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&samples, image.width, image.height, color)
        .context("PNG encoding failed")?;
    Ok(png)
}

pub fn decode_png(png: &[u8]) -> Result<Image> {
    let decoded = image::load_from_memory_with_format(png, ImageFormat::Png).context("PNG decoding failed")?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut data = rgba.into_raw();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Image::from_bgra(width, height, data))
}
