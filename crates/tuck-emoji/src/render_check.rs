//! Test-only: finds characters that neither Segoe UI nor DirectWrite's system font fallback can draw.

use anyhow::{Context, Result};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, IDWriteFactory2,
    IDWriteFontCollection, IDWriteFontFallback, IDWriteTextAnalysisSource,
};
use windows::core::{ComObject, Interface, PCWSTR, w};

use crate::coverage::{AnalysisSource, shared_factory};

const BASE_FAMILY: PCWSTR = w!("Segoe UI");

pub(crate) struct FallbackChecker {
    fallback: IDWriteFontFallback,
}

impl FallbackChecker {
    pub(crate) fn new() -> Result<Self> {
        let factory: IDWriteFactory2 = shared_factory()?.cast().context("DirectWrite 2 is not available")?;
        let fallback = unsafe { factory.GetSystemFontFallback() }.context("system font fallback")?;
        Ok(Self { fallback })
    }

    pub(crate) fn undrawable_chars(&self, text: &str) -> Result<Vec<char>> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let source = ComObject::new(AnalysisSource { text: utf16.clone() }).to_interface::<IDWriteTextAnalysisSource>();
        let mut missing = Vec::new();
        let mut position = 0u32;
        while (position as usize) < utf16.len() {
            let (mut mapped_length, mut mapped_font, mut scale) = (0u32, None, 0.0f32);
            unsafe {
                self.fallback.MapCharacters(
                    &source,
                    position,
                    utf16.len() as u32 - position,
                    None::<&IDWriteFontCollection>,
                    BASE_FAMILY,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    &mut mapped_length,
                    &mut mapped_font,
                    &mut scale,
                )
            }
            .context("mapping characters to fonts")?;
            let mapped_length = mapped_length.max(1);
            if mapped_font.is_none() {
                missing.extend(chars_in_utf16_range(text, position, position + mapped_length));
            }
            position += mapped_length;
        }
        Ok(missing)
    }
}

fn chars_in_utf16_range(text: &str, start: u32, end: u32) -> Vec<char> {
    let mut offset = 0u32;
    let mut chars = Vec::new();
    for c in text.chars() {
        if (start..end).contains(&offset) {
            chars.push(c);
        }
        offset += c.len_utf16() as u32;
    }
    chars
}
