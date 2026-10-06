//! Which emoji the installed Segoe UI Emoji draws as a single glyph; unsupported ones are hidden by the app.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_LINE_BREAKPOINT, DWRITE_READING_DIRECTION, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWRITE_SCRIPT_ANALYSIS,
    DWRITE_SHAPING_GLYPH_PROPERTIES, DWRITE_SHAPING_TEXT_PROPERTIES, DWriteCreateFactory, IDWriteFactory,
    IDWriteFontFace, IDWriteLocalFontFileLoader, IDWriteNumberSubstitution, IDWriteTextAnalysisSink,
    IDWriteTextAnalysisSink_Impl, IDWriteTextAnalysisSource, IDWriteTextAnalysisSource_Impl, IDWriteTextAnalyzer,
};
use windows::core::{BOOL, ComObject, Interface, OutRef, PCWSTR, Ref, implement, w};

use crate::catalog::{Kind, catalog, emoji_data_version};

const EMOJI_FAMILY: PCWSTR = w!("Segoe UI Emoji");
const LOCALE: PCWSTR = w!("en-us");

/// Per emoji entry: does Segoe UI Emoji have a glyph for the whole sequence?
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    supported: Vec<bool>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    key: String,
    supported: Vec<bool>,
}

impl Coverage {
    /// Shapes every emoji entry with DirectWrite (tens of milliseconds).
    pub fn compute() -> Result<Coverage> {
        Shaper::new()?.measure()
    }

    /// Reads the JSON cache at `cache` when it matches the installed font file (size + last-write time) and the
    /// embedded data; otherwise computes and rewrites it. A cache that cannot be written is only logged.
    pub fn load_or_compute(cache: &Path) -> Result<Coverage> {
        let shaper = Shaper::new()?;
        let key = shaper.cache_key()?;
        if let Some(coverage) = read_cache(cache, &key) {
            return Ok(coverage);
        }
        let coverage = shaper.measure()?;
        if let Err(error) = write_cache(cache, &key, &coverage) {
            log::warn!("could not write the emoji coverage cache: {error:#}");
        }
        Ok(coverage)
    }

    /// Stand-in for when DirectWrite is unavailable: hides nothing.
    pub fn all_supported() -> Coverage {
        Coverage { supported: vec![true; catalog(Kind::Emoji).entries.len()] }
    }

    /// `index` is an emoji entry index; out-of-range indices are unsupported.
    pub fn supports(&self, index: u32) -> bool {
        self.supported.get(index as usize).copied().unwrap_or(false)
    }

    pub fn supported_count(&self) -> usize {
        self.supported.iter().filter(|&&supported| supported).count()
    }

    pub fn unsupported_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.supported.iter().enumerate().filter(|&(_, &supported)| !supported).map(|(index, _)| index as u32)
    }
}

fn read_cache(path: &Path, key: &str) -> Option<Coverage> {
    let file: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let fits_catalog = file.supported.len() == catalog(Kind::Emoji).entries.len();
    (file.key == key && fits_catalog).then_some(Coverage { supported: file.supported })
}

fn write_cache(path: &Path, key: &str, coverage: &Coverage) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = CacheFile { key: key.to_string(), supported: coverage.supported.clone() };
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&file)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

struct Shaper {
    analyzer: IDWriteTextAnalyzer,
    face: IDWriteFontFace,
}

impl Shaper {
    fn new() -> Result<Self> {
        let factory = shared_factory()?;
        let face = system_font_face(&factory, EMOJI_FAMILY)?;
        let analyzer = unsafe { factory.CreateTextAnalyzer() }.context("creating the text analyzer")?;
        Ok(Self { analyzer, face })
    }

    fn measure(&self) -> Result<Coverage> {
        let supported = catalog(Kind::Emoji)
            .entries
            .iter()
            .map(|entry| Ok(is_single_glyph(&self.shape(&entry.text)?)))
            .collect::<Result<_>>()?;
        Ok(Coverage { supported })
    }

    fn cache_key(&self) -> Result<String> {
        let path = font_file_path(&self.face)?;
        let metadata = std::fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        let modified_ns = metadata.modified()?.duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        Ok(format!("data={:016x};size={};modified={modified_ns}", emoji_data_version(), metadata.len()))
    }

    /// Glyph indices of the whole text, one `GetGlyphs` call per script run.
    fn shape(&self, text: &str) -> Result<Vec<u16>> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let source = ComObject::new(AnalysisSource { text: utf16.clone() }).to_interface::<IDWriteTextAnalysisSource>();
        let runs = Rc::new(RefCell::new(Vec::new()));
        let sink = ComObject::new(ScriptCollector { runs: runs.clone() }).to_interface::<IDWriteTextAnalysisSink>();
        unsafe { self.analyzer.AnalyzeScript(&source, 0, utf16.len() as u32, &sink) }.context("analyzing script")?;

        let mut glyphs = Vec::new();
        for run in runs.borrow().iter() {
            glyphs.extend(self.shape_run(&utf16[run.start..run.start + run.length], run.analysis)?);
        }
        Ok(glyphs)
    }

    fn shape_run(&self, text: &[u16], analysis: DWRITE_SCRIPT_ANALYSIS) -> Result<Vec<u16>> {
        let max_glyphs = text.len() * 3 / 2 + 16;
        let mut cluster_map = vec![0u16; text.len()];
        let mut text_properties = vec![DWRITE_SHAPING_TEXT_PROPERTIES::default(); text.len()];
        let mut glyph_indices = vec![0u16; max_glyphs];
        let mut glyph_properties = vec![DWRITE_SHAPING_GLYPH_PROPERTIES::default(); max_glyphs];
        let mut glyph_count = 0u32;
        unsafe {
            self.analyzer.GetGlyphs(
                PCWSTR(text.as_ptr()),
                text.len() as u32,
                &self.face,
                false,
                false,
                &analysis,
                LOCALE,
                None::<&IDWriteNumberSubstitution>,
                None,
                None,
                0,
                max_glyphs as u32,
                cluster_map.as_mut_ptr(),
                text_properties.as_mut_ptr(),
                glyph_indices.as_mut_ptr(),
                glyph_properties.as_mut_ptr(),
                &mut glyph_count,
            )
        }
        .context("shaping")?;
        glyph_indices.truncate(glyph_count as usize);
        Ok(glyph_indices)
    }
}

fn is_single_glyph(glyphs: &[u16]) -> bool {
    matches!(glyphs, [glyph] if *glyph != 0)
}

pub(crate) fn shared_factory() -> Result<IDWriteFactory> {
    unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.context("creating the DirectWrite factory")
}

fn system_font_face(factory: &IDWriteFactory, family_name: PCWSTR) -> Result<IDWriteFontFace> {
    unsafe {
        let mut collection = None;
        factory.GetSystemFontCollection(&mut collection, false)?;
        let collection = collection.context("no system font collection")?;
        let (mut index, mut exists) = (0u32, BOOL(0));
        collection.FindFamilyName(family_name, &mut index, &mut exists)?;
        if !exists.as_bool() {
            anyhow::bail!("font family {} is not installed", family_name.display());
        }
        let font = collection.GetFontFamily(index)?.GetFirstMatchingFont(
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )?;
        Ok(font.CreateFontFace()?)
    }
}

fn font_file_path(face: &IDWriteFontFace) -> Result<PathBuf> {
    unsafe {
        let mut file_count = 0u32;
        face.GetFiles(&mut file_count, None)?;
        let mut files = vec![None; file_count as usize];
        face.GetFiles(&mut file_count, Some(files.as_mut_ptr()))?;
        let file = files.into_iter().flatten().next().context("the font face has no file")?;

        let (mut key, mut key_size) = (std::ptr::null_mut(), 0u32);
        file.GetReferenceKey(&mut key, &mut key_size)?;
        let loader: IDWriteLocalFontFileLoader = file.GetLoader()?.cast().context("the font is not a local file")?;
        let length = loader.GetFilePathLengthFromKey(key, key_size)? as usize;
        let mut path = vec![0u16; length + 1];
        loader.GetFilePathFromKey(key, key_size, &mut path)?;
        Ok(PathBuf::from(OsString::from_wide(&path[..length])))
    }
}

#[derive(Clone, Copy)]
struct ScriptRun {
    start: usize,
    length: usize,
    analysis: DWRITE_SCRIPT_ANALYSIS,
}

#[implement(IDWriteTextAnalysisSink)]
struct ScriptCollector {
    runs: Rc<RefCell<Vec<ScriptRun>>>,
}

impl IDWriteTextAnalysisSink_Impl for ScriptCollector_Impl {
    fn SetScriptAnalysis(
        &self,
        textposition: u32,
        textlength: u32,
        scriptanalysis: *const DWRITE_SCRIPT_ANALYSIS,
    ) -> windows::core::Result<()> {
        let analysis = unsafe { scriptanalysis.read() };
        self.runs.borrow_mut().push(ScriptRun { start: textposition as usize, length: textlength as usize, analysis });
        Ok(())
    }

    fn SetLineBreakpoints(&self, _: u32, _: u32, _: *const DWRITE_LINE_BREAKPOINT) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetBidiLevel(&self, _: u32, _: u32, _: u8, _: u8) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetNumberSubstitution(&self, _: u32, _: u32, _: Ref<IDWriteNumberSubstitution>) -> windows::core::Result<()> {
        Ok(())
    }
}

/// One paragraph of left-to-right text in a single locale, as DirectWrite's analyzers expect it.
#[implement(IDWriteTextAnalysisSource)]
pub(crate) struct AnalysisSource {
    pub(crate) text: Vec<u16>,
}

impl AnalysisSource_Impl {
    fn remaining_from(&self, position: u32) -> (usize, u32) {
        let start = (position as usize).min(self.text.len());
        (start, (self.text.len() - start) as u32)
    }
}

impl IDWriteTextAnalysisSource_Impl for AnalysisSource_Impl {
    fn GetTextAtPosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let (start, remaining) = self.remaining_from(textposition);
        unsafe {
            *textstring = self.text.as_ptr().add(start).cast_mut();
            *textlength = remaining;
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let (start, _) = self.remaining_from(textposition);
        unsafe {
            *textstring = self.text.as_ptr().cast_mut();
            *textlength = start as u32;
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        textposition: u32,
        textlength: *mut u32,
        localename: *mut *mut u16,
    ) -> windows::core::Result<()> {
        let (_, remaining) = self.remaining_from(textposition);
        unsafe {
            *textlength = remaining;
            *localename = LOCALE.as_ptr().cast_mut();
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        textposition: u32,
        textlength: *mut u32,
        numbersubstitution: OutRef<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        let (_, remaining) = self.remaining_from(textposition);
        unsafe { *textlength = remaining };
        numbersubstitution.write(None)
    }
}
