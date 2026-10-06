use crate::util::{from_wide, wide};
use std::{
    collections::HashMap,
    hash::Hash,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tuck_core::Image;
use windows::{
    Win32::{
        Foundation::S_OK,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
            GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ,
        },
        Storage::FileSystem::{
            FILE_FLAGS_AND_ATTRIBUTES, GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
        },
        UI::{
            Shell::{SHDefExtractIconW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW},
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO},
        },
    },
    core::PCWSTR,
};

/// Translations tried when the version resource's table has no usable FileDescription.
const FALLBACK_TRANSLATIONS: [(u16, u16); 3] = [(0x0409, 0x04b0), (0x0409, 0x04e4), (0x0000, 0x04b0)];
const MAX_ICON_PX: u32 = 256;

fn cached<K: Eq + Hash + Clone, V: Clone>(
    cache: &'static OnceLock<Mutex<HashMap<K, V>>>,
    key: K,
    compute: impl FnOnce() -> V,
) -> V {
    let map = cache.get_or_init(Default::default);
    if let Some(value) = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&key) {
        return value.clone();
    }
    let value = compute();
    map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(key, value.clone());
    value
}

/// The app's FileDescription (e.g. "Google Chrome"), else the file stem. Cached per path.
pub fn display_name(exe: &Path) -> String {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    cached(&CACHE, exe.to_path_buf(), || file_description(exe).unwrap_or_else(|| file_stem(exe)))
}

fn file_stem(exe: &Path) -> String {
    exe.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_description(exe: &Path) -> Option<String> {
    let block = version_block(exe)?;
    translations(&block).into_iter().chain(FALLBACK_TRANSLATIONS).find_map(|(language, codepage)| {
        query_string(&block, &format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\FileDescription"))
    })
}

fn version_block(exe: &Path) -> Option<Vec<u8>> {
    let path = wide(exe);
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    if size == 0 {
        return None;
    }
    let mut block = vec![0u8; size as usize];
    unsafe { GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, block.as_mut_ptr().cast()) }.ok()?;
    Some(block)
}

/// Pointer and length (bytes for binary values, characters for strings) of a value inside `block`.
fn query_value(block: &[u8], sub_block: &str) -> Option<(*const u8, usize)> {
    let sub_block = wide(sub_block);
    let mut value = std::ptr::null_mut();
    let mut length = 0u32;
    let found =
        unsafe { VerQueryValueW(block.as_ptr().cast(), PCWSTR(sub_block.as_ptr()), &mut value, &mut length) }.as_bool();
    (found && !value.is_null() && length > 0).then_some((value.cast_const().cast(), length as usize))
}

fn translations(block: &[u8]) -> Vec<(u16, u16)> {
    let Some((value, bytes)) = query_value(block, "\\VarFileInfo\\Translation") else {
        return Vec::new();
    };
    let table = unsafe { std::slice::from_raw_parts(value, bytes) };
    table
        .chunks_exact(4)
        .map(|entry| (u16::from_le_bytes([entry[0], entry[1]]), u16::from_le_bytes([entry[2], entry[3]])))
        .collect()
}

fn query_string(block: &[u8], sub_block: &str) -> Option<String> {
    let (value, characters) = query_value(block, sub_block)?;
    let text = from_wide(unsafe { std::slice::from_raw_parts(value.cast::<u16>(), characters) });
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The shell icon of `exe` as straight-alpha BGRA, `size_px` square when the file allows it. Cached per exe and size.
/// Call where COM is initialized (the shell fallback needs it).
pub fn icon(exe: &Path, size_px: u32) -> Option<Image> {
    type IconsBySize = HashMap<(PathBuf, u32), Option<Image>>;
    static CACHE: OnceLock<Mutex<IconsBySize>> = OnceLock::new();
    cached(&CACHE, (exe.to_path_buf(), size_px), || icon_image(&extract_icon(exe, size_px)?))
}

struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

struct OwnedBitmap(HBITMAP);
impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }
}

struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

fn extract_icon(exe: &Path, size_px: u32) -> Option<OwnedIcon> {
    let path = wide(exe);
    let mut icon = HICON::default();
    let size = size_px.clamp(1, MAX_ICON_PX);
    let extracted = unsafe { SHDefExtractIconW(PCWSTR(path.as_ptr()), 0, 0, Some(&mut icon), None, size) };
    if extracted == S_OK && !icon.is_invalid() {
        return Some(OwnedIcon(icon));
    }
    let mut info = SHFILEINFOW::default();
    let found = unsafe {
        SHGetFileInfoW(
            PCWSTR(path.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    (found != 0 && !info.hIcon.is_invalid()).then_some(OwnedIcon(info.hIcon))
}

fn icon_image(icon: &OwnedIcon) -> Option<Image> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon.0, &mut info) }.ok()?;
    let color = OwnedBitmap(info.hbmColor);
    let mask = OwnedBitmap(info.hbmMask);
    if color.0.is_invalid() {
        return None;
    }
    let (width, height) = bitmap_size(&color)?;
    let mut pixels = bitmap_pixels(&color, width, height)?;
    if pixels.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        apply_mask(&mut pixels, &bitmap_pixels(&mask, width, height)?);
    }
    Some(Image::from_bgra(width, height, pixels))
}

fn bitmap_size(bitmap: &OwnedBitmap) -> Option<(u32, u32)> {
    let mut info = BITMAP::default();
    let written =
        unsafe { GetObjectW(HGDIOBJ(bitmap.0.0), size_of::<BITMAP>() as i32, Some((&mut info as *mut BITMAP).cast())) };
    (written > 0 && info.bmWidth > 0 && info.bmHeight > 0).then_some((info.bmWidth as u32, info.bmHeight as u32))
}

/// Top-down 32-bit BGRA rows of `bitmap`; monochrome masks come out black (opaque) and white (transparent).
fn bitmap_pixels(bitmap: &OwnedBitmap, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let dc = MemoryDc(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return None;
    }
    let lines =
        unsafe { GetDIBits(dc.0, bitmap.0, 0, height, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) };
    (lines == height as i32).then_some(pixels)
}

/// Icons without an alpha channel: AND-mask black = opaque, white = transparent.
fn apply_mask(pixels: &mut [u8], mask: &[u8]) {
    for (pixel, mask) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        pixel[3] = if mask[0] == 0 { 255 } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_supplies_alpha() {
        let mut pixels = vec![10, 20, 30, 0, 40, 50, 60, 0];
        apply_mask(&mut pixels, &[0, 0, 0, 0, 255, 255, 255, 0]);
        assert_eq!(pixels, [10, 20, 30, 255, 40, 50, 60, 0]);
    }

    #[test]
    fn names_fall_back_to_the_file_stem() {
        let missing = Path::new(r"C:\definitely\not\here\Some App.exe");
        assert_eq!(display_name(missing), "Some App");
        assert_eq!(file_stem(Path::new("")), "");
    }

    #[test]
    fn system_executables_have_descriptions_and_icons() {
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let cmd = PathBuf::from(system_root).join(r"System32\cmd.exe");
        if !cmd.exists() {
            return;
        }
        assert!(!display_name(&cmd).eq_ignore_ascii_case("cmd"));
        let image = icon(&cmd, 32).expect("cmd icon");
        assert!(image.width > 0 && image.height > 0);
        assert!(image.data.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }
}
