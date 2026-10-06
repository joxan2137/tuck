//! Pure conversions between clipboard bytes and Tuck's types: DIB/DIBV5, PNG, HDROP, CF_HTML and text encodings.

use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

use anyhow::{Context, Result, bail, ensure};
use image::{
    ExtendedColorType, ImageEncoder, ImageFormat,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use tuck_core::Image;
use windows::{
    Win32::Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar, WideCharToMultiByte},
    core::PCSTR,
};

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;
const BITMAPINFOHEADER_LEN: usize = 40;
const BITMAPV3HEADER_LEN: usize = 56;
const BITMAPV5HEADER_LEN: usize = 124;
const LCS_SRGB: u32 = 0x7352_4742;
const LCS_GM_IMAGES: u32 = 4;
const DROPFILES_LEN: usize = 20;
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
const START_FRAGMENT_MARKER: &str = "<!--StartFragment-->";
const END_FRAGMENT_MARKER: &str = "<!--EndFragment-->";

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn i32_at(bytes: &[u8], offset: usize) -> Option<i32> {
    u32_at(bytes, offset).map(|value| value as i32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelMasks {
    red: u32,
    green: u32,
    blue: u32,
    alpha: u32,
}

const BGRA_MASKS: ChannelMasks =
    ChannelMasks { red: 0x00ff_0000, green: 0x0000_ff00, blue: 0x0000_00ff, alpha: 0xff00_0000 };

impl ChannelMasks {
    fn bgra(&self, pixel: u32) -> [u8; 4] {
        let alpha = if self.alpha == 0 { 255 } else { scale_channel(pixel, self.alpha) };
        [scale_channel(pixel, self.blue), scale_channel(pixel, self.green), scale_channel(pixel, self.red), alpha]
    }
}

fn scale_channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let value = u64::from((pixel & mask) >> shift);
    ((value * 255 + max / 2) / max) as u8
}

struct DibHeader {
    width: u32,
    height: u32,
    top_down: bool,
    bit_count: u16,
    masks: ChannelMasks,
    pixel_offset: usize,
}

impl DibHeader {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let header_len = u32_at(bytes, 0).context("DIB is empty")? as usize;
        ensure!(header_len >= BITMAPINFOHEADER_LEN, "Unsupported DIB header size {header_len}");
        ensure!(bytes.len() >= header_len, "DIB header is truncated");
        let field = |offset| u32_at(bytes, offset).unwrap_or_default();
        let width = i32_at(bytes, 4).unwrap_or_default();
        let height = i32_at(bytes, 8).unwrap_or_default();
        let bit_count = u16_at(bytes, 14).unwrap_or_default();
        let compression = field(16);
        let colors_used = field(32) as usize;
        ensure!(width > 0 && height != 0 && height != i32::MIN, "DIB has no pixels");

        let (masks, masks_after_header) = match (bit_count, compression) {
            (24 | 32, BI_RGB) => (BGRA_MASKS, 0),
            (32, BI_BITFIELDS | BI_ALPHABITFIELDS) if header_len == BITMAPINFOHEADER_LEN => {
                let explicit_alpha = compression == BI_ALPHABITFIELDS;
                let mask =
                    |index: usize| u32_at(bytes, header_len + index * 4).context("DIB color masks are truncated");
                let (red, green, blue) = (mask(0)?, mask(1)?, mask(2)?);
                let alpha = if explicit_alpha { mask(3)? } else { !(red | green | blue) };
                (ChannelMasks { red, green, blue, alpha }, if explicit_alpha { 16 } else { 12 })
            }
            (32, BI_BITFIELDS | BI_ALPHABITFIELDS) => {
                let (red, green, blue) = (field(40), field(44), field(48));
                let alpha = if header_len >= BITMAPV3HEADER_LEN { field(52) } else { !(red | green | blue) };
                (ChannelMasks { red, green, blue, alpha }, 0)
            }
            _ => bail!("Unsupported DIB format: {bit_count} bpp, compression {compression}"),
        };
        let pixel_offset = colors_used
            .checked_mul(4)
            .and_then(|palette| palette.checked_add(header_len + masks_after_header))
            .context("DIB color table is too large")?;
        Ok(Self {
            width: width as u32,
            height: height.unsigned_abs(),
            top_down: height < 0,
            bit_count,
            masks,
            pixel_offset,
        })
    }
}

/// Width and height from a CF_DIB / CF_DIBV5 header Tuck can decode, without touching the pixels.
pub fn dib_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    DibHeader::parse(bytes).ok().map(|header| (header.width, header.height))
}

/// CF_DIB / CF_DIBV5 bytes (24 or 32 bpp, BI_RGB or BI_BITFIELDS, either row order) to a straight-alpha image.
/// A 32 bpp DIB whose alpha channel is all zero is opaque.
pub fn dib_to_image(bytes: &[u8]) -> Result<Image> {
    let header = DibHeader::parse(bytes)?;
    let (width, height) = (header.width as usize, header.height as usize);
    let bytes_per_pixel = usize::from(header.bit_count / 8);
    let stride = (width * usize::from(header.bit_count)).div_ceil(32) * 4;
    let pixels_len = stride.checked_mul(height).context("DIB is too large")?;
    let pixels = bytes
        .get(header.pixel_offset..)
        .and_then(|pixels| pixels.get(..pixels_len))
        .context("DIB pixel data is truncated")?;

    let mut data = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let source_row = if header.top_down { y } else { height - 1 - y };
        let row = &pixels[source_row * stride..][..width * bytes_per_pixel];
        if header.bit_count == 24 {
            for bgr in row.chunks_exact(3) {
                data.extend_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
            }
        } else {
            for pixel in row.chunks_exact(4) {
                data.extend_from_slice(
                    &header.masks.bgra(u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]])),
                );
            }
        }
    }
    if data.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        data.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
    }
    Ok(Image::from_bgra(header.width, header.height, data))
}

/// CF_DIBV5 bytes: BITMAPV5HEADER, 32 bpp BI_BITFIELDS with an alpha mask, sRGB, bottom-up rows, straight alpha.
pub fn image_to_dibv5(image: &Image) -> Vec<u8> {
    let mut bytes = vec![0u8; BITMAPV5HEADER_LEN];
    let fields = [
        (0, BITMAPV5HEADER_LEN as u32),
        (4, image.width),
        (8, image.height),
        (16, BI_BITFIELDS),
        (20, image.data.len() as u32),
        (40, BGRA_MASKS.red),
        (44, BGRA_MASKS.green),
        (48, BGRA_MASKS.blue),
        (52, BGRA_MASKS.alpha),
        (56, LCS_SRGB),
        (108, LCS_GM_IMAGES),
    ];
    for (offset, value) in fields {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
    if image.width > 0 {
        for row in image.data.chunks_exact(image.stride()).rev() {
            bytes.extend_from_slice(row);
        }
    }
    bytes
}

/// Width and height from a PNG's IHDR chunk, without decoding.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != PNG_SIGNATURE || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let big_endian = |offset: usize| Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?));
    Some((big_endian(16)?, big_endian(20)?))
}

pub fn decode_png(bytes: &[u8]) -> Result<Image> {
    let rgba = image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut data = rgba.into_raw();
    swap_red_blue(&mut data);
    Ok(Image::from_bgra(width, height, data))
}

/// Fast-compressed PNG, keeping alpha.
pub fn encode_png(image: &Image) -> Result<Vec<u8>> {
    let mut rgba = image.data.clone();
    swap_red_blue(&mut rgba);
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive).write_image(
        &rgba,
        image.width,
        image.height,
        ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}

fn swap_red_blue(pixels: &mut [u8]) {
    pixels.chunks_exact_mut(4).for_each(|pixel| pixel.swap(0, 2));
}

/// The paths in a CF_HDROP (`DROPFILES` followed by a double-NUL-terminated list, wide or ANSI).
pub fn parse_hdrop(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    let files_offset = u32_at(bytes, 0).context("DROPFILES header is truncated")? as usize;
    let wide = u32_at(bytes, 16).context("DROPFILES header is truncated")? != 0;
    ensure!(files_offset >= DROPFILES_LEN, "DROPFILES file list overlaps its header");
    let list = bytes.get(files_offset..).context("DROPFILES file list is out of range")?;
    let paths: Vec<PathBuf> = if wide {
        let units: Vec<u16> = list.chunks_exact(2).map(|unit| u16::from_le_bytes([unit[0], unit[1]])).collect();
        units
            .split(|&unit| unit == 0)
            .take_while(|name| !name.is_empty())
            .map(|name| OsString::from_wide(name).into())
            .collect()
    } else {
        list.split(|&byte| byte == 0)
            .take_while(|name| !name.is_empty())
            .map(|name| OsString::from_wide(&ansi_to_wide(name)).into())
            .collect()
    };
    Ok(paths)
}

/// A wide CF_HDROP for `paths` (which should be absolute).
pub fn build_hdrop(paths: &[PathBuf]) -> Vec<u8> {
    let mut bytes = vec![0u8; DROPFILES_LEN];
    bytes[..4].copy_from_slice(&(DROPFILES_LEN as u32).to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        bytes.extend(path.as_os_str().encode_wide().chain(Some(0)).flat_map(u16::to_le_bytes));
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

fn cf_html_header(start_html: usize, end_html: usize, start_fragment: usize, end_fragment: usize) -> String {
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    )
}

/// A complete CF_HTML ("HTML Format") payload wrapping `fragment`; offsets are UTF-8 byte offsets.
pub fn cf_html_from_fragment(fragment: &str) -> String {
    let prefix = format!("<html><body>\r\n{START_FRAGMENT_MARKER}");
    let suffix = format!("{END_FRAGMENT_MARKER}\r\n</body></html>");
    let start_html = cf_html_header(0, 0, 0, 0).len();
    let start_fragment = start_html + prefix.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + suffix.len();
    format!("{}{prefix}{fragment}{suffix}", cf_html_header(start_html, end_html, start_fragment, end_fragment))
}

fn cf_html_offset(cf_html: &str, key: &str) -> Option<usize> {
    cf_html
        .lines()
        .take_while(|line| !line.starts_with('<'))
        .find_map(|line| line.strip_prefix(key)?.trim().parse().ok())
}

/// The fragment of a CF_HTML payload: by its header offsets, else between the fragment comments.
pub fn cf_html_fragment(cf_html: &str) -> Option<&str> {
    let start = cf_html_offset(cf_html, "StartFragment:");
    let end = cf_html_offset(cf_html, "EndFragment:");
    if let (Some(start), Some(end)) = (start, end)
        && let Some(fragment) = cf_html.get(start..end)
    {
        return Some(fragment);
    }
    let after_marker = cf_html.find(START_FRAGMENT_MARKER)? + START_FRAGMENT_MARKER.len();
    let fragment_len = cf_html[after_marker..].find(END_FRAGMENT_MARKER)?;
    Some(&cf_html[after_marker..after_marker + fragment_len])
}

/// The bytes of NUL-terminated UTF-16LE text before its terminator.
pub fn utf16_until_nul(bytes: &[u8]) -> &[u8] {
    let units = bytes.chunks_exact(2).position(|unit| unit == [0, 0]).unwrap_or(bytes.len() / 2);
    &bytes[..units * 2]
}

/// The bytes of NUL-terminated 8-bit text (UTF-8 or ANSI) before its terminator.
pub fn bytes_until_nul(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len())]
}

/// CF_UNICODETEXT bytes to text, up to the first NUL; unpaired surrogates become U+FFFD.
pub fn decode_utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> =
        utf16_until_nul(bytes).chunks_exact(2).map(|unit| u16::from_le_bytes([unit[0], unit[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// CF_UNICODETEXT bytes: UTF-16LE with a NUL terminator.
pub fn encode_utf16_text(text: &str) -> Vec<u8> {
    text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// UTF-8 clipboard text (CF_HTML) up to the first NUL; invalid sequences become U+FFFD.
pub fn decode_utf8_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes_until_nul(bytes)).into_owned()
}

pub fn encode_utf8_text(text: &str) -> Vec<u8> {
    text.bytes().chain(Some(0)).collect()
}

/// 8-bit clipboard text in the system ANSI code page (RTF) up to the first NUL.
pub fn decode_ansi_text(bytes: &[u8]) -> String {
    String::from_utf16_lossy(&ansi_to_wide(bytes_until_nul(bytes)))
}

/// `text` in the system ANSI code page with a NUL terminator, the inverse of `decode_ansi_text`.
pub fn encode_ansi_text(text: &str) -> Vec<u8> {
    let mut bytes = wide_to_ansi(&text.encode_utf16().collect::<Vec<_>>());
    bytes.push(0);
    bytes
}

fn ansi_to_wide(bytes: &[u8]) -> Vec<u16> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS::default();
    let len = unsafe { MultiByteToWideChar(CP_ACP, flags, bytes, None) };
    let mut wide = vec![0; len.max(0) as usize];
    let written = unsafe { MultiByteToWideChar(CP_ACP, flags, bytes, Some(&mut wide)) };
    wide.truncate(written.max(0) as usize);
    wide
}

fn wide_to_ansi(wide: &[u16]) -> Vec<u8> {
    if wide.is_empty() {
        return Vec::new();
    }
    let len = unsafe { WideCharToMultiByte(CP_ACP, 0, wide, None, PCSTR::null(), None) };
    let mut bytes = vec![0; len.max(0) as usize];
    let written = unsafe { WideCharToMultiByte(CP_ACP, 0, wide, Some(&mut bytes), PCSTR::null(), None) };
    bytes.truncate(written.max(0) as usize);
    bytes
}

/// What an image costs against `max_bytes`: `width * height * 4`, as `ClipItem::bytes` counts it.
pub fn image_bytes(width: u32, height: u32) -> u64 {
    (u64::from(width) * u64::from(height)).saturating_mul(4)
}

/// What a file list costs against `max_bytes`: the path bytes, as `ClipItem::bytes` counts it.
pub fn files_bytes(paths: &[PathBuf]) -> u64 {
    paths.iter().map(|path| path.as_os_str().len() as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_header(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; BITMAPINFOHEADER_LEN];
        bytes[0..4].copy_from_slice(&(BITMAPINFOHEADER_LEN as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&width.to_le_bytes());
        bytes[8..12].copy_from_slice(&height.to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&bit_count.to_le_bytes());
        bytes[16..20].copy_from_slice(&compression.to_le_bytes());
        bytes
    }

    fn le_words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn decodes_24_bpp_bottom_up_with_row_padding() {
        let mut dib = info_header(2, 2, 24, BI_RGB);
        dib.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        dib.extend_from_slice(&[7, 8, 9, 10, 11, 12, 0, 0]);
        let image = dib_to_image(&dib).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.data, [7, 8, 9, 255, 10, 11, 12, 255, 1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(dib_dimensions(&dib), Some((2, 2)));
    }

    #[test]
    fn decodes_32_bpp_top_down_and_keeps_straight_alpha() {
        let mut dib = info_header(1, -2, 32, BI_RGB);
        dib.extend_from_slice(&[10, 20, 30, 64, 40, 50, 60, 128]);
        let image = dib_to_image(&dib).unwrap();
        assert_eq!(image.data, [10, 20, 30, 64, 40, 50, 60, 128]);
    }

    #[test]
    fn all_zero_alpha_means_opaque() {
        let mut dib = info_header(2, 1, 32, BI_RGB);
        dib.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        let image = dib_to_image(&dib).unwrap();
        assert!(image.is_opaque());
        assert_eq!(image.pixel(1, 0), [40, 50, 60, 255]);
    }

    #[test]
    fn decodes_bitfields_masks_after_info_header() {
        let mut dib = info_header(1, 1, 32, BI_BITFIELDS);
        dib.extend(le_words(&[0x0000_00ff, 0x0000_ff00, 0x00ff_0000]));
        dib.extend_from_slice(&[1, 2, 3, 200]);
        let image = dib_to_image(&dib).unwrap();
        assert_eq!(image.data, [3, 2, 1, 200]);
    }

    #[test]
    fn decodes_bitfields_with_a_palette_table_after_the_masks() {
        let mut dib = info_header(1, 1, 32, BI_BITFIELDS);
        dib[32..36].copy_from_slice(&1u32.to_le_bytes());
        dib.extend(le_words(&[0x00ff_0000, 0x0000_ff00, 0x0000_00ff]));
        dib.extend_from_slice(&[9, 9, 9, 9]);
        dib.extend_from_slice(&[1, 2, 3, 255]);
        assert_eq!(dib_to_image(&dib).unwrap().data, [1, 2, 3, 255]);
    }

    #[test]
    fn scales_narrow_bitfield_channels_to_8_bits() {
        let mut dib = info_header(1, 1, 32, BI_ALPHABITFIELDS);
        dib.extend(le_words(&[0x3ff0_0000, 0x000f_fc00, 0x0000_03ff, 0xc000_0000]));
        dib.extend(le_words(&[0xc000_03ff | (0x200 << 10)]));
        assert_eq!(dib_to_image(&dib).unwrap().data, [255, 128, 0, 255]);
    }

    #[test]
    fn dibv5_header_and_straight_bgra_bottom_up() {
        let image = Image::from_bgra(1, 2, vec![10, 20, 30, 64, 40, 50, 60, 128]);
        let dib = image_to_dibv5(&image);
        let word = |at| u32_at(&dib, at).unwrap();
        assert_eq!((word(0), word(4), word(8), word(16), word(20)), (124, 1, 2, 3, 8));
        assert_eq!(&dib[12..16], &[1, 0, 32, 0]);
        assert_eq!((word(40), word(44), word(48), word(52)), (0xff0000, 0xff00, 0xff, 0xff000000));
        assert_eq!(&dib[124..], &[40, 50, 60, 128, 10, 20, 30, 64]);
    }

    #[test]
    fn dibv5_round_trips() {
        let image = Image::from_bgra(3, 2, (0..24).map(|i| i * 10).collect());
        assert_eq!(dib_to_image(&image_to_dibv5(&image)).unwrap(), image);
    }

    #[test]
    fn dibv5_without_alpha_mask_is_opaque() {
        let mut dib = image_to_dibv5(&Image::from_bgra(1, 1, vec![1, 2, 3, 77]));
        dib[52..56].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(dib_to_image(&dib).unwrap().data, [1, 2, 3, 255]);
    }

    #[test]
    fn rejects_broken_dibs() {
        assert!(dib_to_image(&[]).is_err());
        assert!(dib_to_image(&info_header(0, 1, 32, BI_RGB)).is_err());
        assert!(dib_to_image(&info_header(1, 1, 8, BI_RGB)).is_err());
        let mut truncated = info_header(2, 2, 32, BI_RGB);
        truncated.extend_from_slice(&[0; 12]);
        assert!(dib_to_image(&truncated).is_err());
        let mut huge_palette = info_header(1, 1, 32, BI_RGB);
        huge_palette[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(dib_to_image(&huge_palette).is_err());
        assert_eq!(dib_dimensions(&info_header(1, 1, 16, BI_RGB)), None);
    }

    #[test]
    fn png_round_trips_with_alpha() {
        let image = Image::from_bgra(2, 1, vec![10, 20, 30, 40, 50, 60, 70, 255]);
        let png = encode_png(&image).unwrap();
        assert_eq!(png_dimensions(&png), Some((2, 1)));
        assert_eq!(decode_png(&png).unwrap(), image);
        assert!(decode_png(b"not a png").is_err());
        assert_eq!(png_dimensions(b"not a png at all, really"), None);
    }

    #[test]
    fn hdrop_round_trips_wide_paths() {
        let paths = vec![PathBuf::from(r"C:\Users\you\a.txt"), PathBuf::from(r"D:\Ünïcödé\b 1.png")];
        let bytes = build_hdrop(&paths);
        assert_eq!(u32_at(&bytes, 0), Some(20));
        assert_eq!(u32_at(&bytes, 16), Some(1));
        assert_eq!(parse_hdrop(&bytes).unwrap(), paths);
        assert!(parse_hdrop(&build_hdrop(&[])).unwrap().is_empty());
    }

    #[test]
    fn parses_ansi_hdrop_and_ignores_trailing_bytes() {
        let mut bytes = vec![0u8; DROPFILES_LEN];
        bytes[..4].copy_from_slice(&20u32.to_le_bytes());
        bytes.extend_from_slice(b"C:\\a.txt\0C:\\dir\\b.txt\0\0garbage");
        assert_eq!(parse_hdrop(&bytes).unwrap(), [PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\dir\b.txt")]);
    }

    #[test]
    fn rejects_broken_hdrop() {
        assert!(parse_hdrop(&[1, 2, 3]).is_err());
        let mut bytes = vec![0u8; DROPFILES_LEN];
        bytes[..4].copy_from_slice(&400u32.to_le_bytes());
        assert!(parse_hdrop(&bytes).is_err());
        bytes[..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(parse_hdrop(&bytes).is_err());
    }

    #[test]
    fn cf_html_offsets_are_utf8_byte_offsets() {
        let fragment = "<b>héllo ✓</b>";
        let html = cf_html_from_fragment(fragment);
        let offset = |key| cf_html_offset(&html, key).unwrap();
        assert_eq!(offset("StartHTML:"), 105);
        assert!(html[offset("StartHTML:")..].starts_with("<html>"));
        assert_eq!(offset("EndHTML:"), html.len());
        assert_eq!(&html[offset("StartFragment:")..offset("EndFragment:")], fragment);
        assert_eq!(cf_html_fragment(&html), Some(fragment));
    }

    #[test]
    fn cf_html_fragment_falls_back_to_markers() {
        let wrong_offsets = "Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\n\
                             StartFragment:0000009999\r\nEndFragment:0000010000\r\n\
                             <html><body><!--StartFragment--><i>x</i><!--EndFragment--></body></html>";
        assert_eq!(cf_html_fragment(wrong_offsets), Some("<i>x</i>"));
        assert_eq!(cf_html_fragment("<html><!--StartFragment-->y<!--EndFragment--></html>"), Some("y"));
        assert_eq!(cf_html_fragment("<html>no fragment</html>"), None);
    }

    #[test]
    fn text_encodings_round_trip_and_stop_at_nul() {
        let text = "héllo 👋\r\nworld";
        let utf16 = encode_utf16_text(text);
        assert_eq!(&utf16[utf16.len() - 2..], &[0, 0]);
        assert_eq!(decode_utf16_text(&utf16), text);
        assert_eq!(decode_utf16_text(&[b'a', 0, 0, 0, b'b', 0]), "a");
        assert_eq!(utf16_until_nul(&[b'a', 0, 0, 0, b'b', 0]), [b'a', 0]);
        assert_eq!(decode_utf8_text(&encode_utf8_text(text)), text);
        assert_eq!(decode_utf8_text(b"ab\0cd"), "ab");
        let rtf = r"{\rtf1\ansi Hello}";
        assert_eq!(encode_ansi_text(rtf), [rtf.as_bytes(), &[0]].concat());
        assert_eq!(decode_ansi_text(&encode_ansi_text(rtf)), rtf);
        assert_eq!(decode_ansi_text(b""), "");
    }

    #[test]
    fn size_estimates_match_clip_item_bytes() {
        assert_eq!(image_bytes(1920, 1080), 1920 * 1080 * 4);
        assert_eq!(image_bytes(u32::MAX, u32::MAX), u64::MAX);
        assert_eq!(files_bytes(&[PathBuf::from(r"C:\a"), PathBuf::from(r"C:\bc")]), 9);
    }
}
