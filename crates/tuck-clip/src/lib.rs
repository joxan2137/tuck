//! The clipboard thread: watches copies, reads every format Tuck keeps, writes items back. See DESIGN.md §5.

pub mod formats;
mod service;
mod winrt;

use std::path::{Path, PathBuf};

use tuck_core::{ClipKind, Image, settings::HistorySettings};

pub use service::ClipboardService;
pub use winrt::import_windows_history;

/// One copy. Exactly one of `files`, `text` (with optional `html`/`rtf`) or `image` is set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Captured {
    /// `GetClipboardSequenceNumber` of the copy; 0 for imported Windows history.
    pub sequence: u32,
    pub text: Option<String>,
    /// The full CF_HTML payload ("Version:0.9 ..."); see `formats::cf_html_fragment`.
    pub html: Option<String>,
    pub rtf: Option<String>,
    pub image: Option<Image>,
    pub files: Vec<PathBuf>,
    pub source_exe: Option<PathBuf>,
}

impl Captured {
    pub fn kind(&self) -> Option<ClipKind> {
        if !self.files.is_empty() {
            Some(ClipKind::Files)
        } else if self.text.is_some() {
            Some(ClipKind::Text)
        } else if self.image.is_some() {
            Some(ClipKind::Image)
        } else {
            None
        }
    }

    /// Text + html + rtf bytes, image `width * height * 4`, or path bytes: what `ClipItem::bytes` stores.
    pub fn byte_len(&self) -> u64 {
        let text_bytes =
            [&self.text, &self.html, &self.rtf].into_iter().flatten().map(|text| text.len() as u64).sum::<u64>();
        let image_bytes = self.image.as_ref().map_or(0, |image| formats::image_bytes(image.width, image.height));
        text_bytes + image_bytes + formats::files_bytes(&self.files)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The source asked monitors and history to skip it (password managers).
    Excluded,
    IgnoredApp,
    Paused,
    TooLarge,
    /// No file list, text or image.
    Empty,
    /// The clipboard stayed busy or the data could not be decoded.
    Unreadable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipEvent {
    Captured(Captured),
    /// The clipboard holds Tuck's own `ClipboardService::write` tagged with `token`.
    OwnWrite { token: u64 },
    Skipped(SkipReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFilter {
    pub paused: bool,
    /// Executable file names (`keepass.exe`), matched case-insensitively.
    pub ignored_apps: Vec<String>,
    pub max_bytes: u64,
}

impl CaptureFilter {
    pub fn ignores(&self, exe: &Path) -> bool {
        let Some(name) = exe.file_name().map(|name| name.to_string_lossy().to_lowercase()) else {
            return false;
        };
        self.ignored_apps.iter().any(|app| app.to_lowercase() == name)
    }
}

impl From<&HistorySettings> for CaptureFilter {
    fn from(settings: &HistorySettings) -> Self {
        Self {
            paused: !settings.enabled,
            ignored_apps: settings.ignored_apps.clone(),
            max_bytes: u64::from(settings.max_item_mb) * 1024 * 1024,
        }
    }
}

impl Default for CaptureFilter {
    fn default() -> Self {
        Self::from(&HistorySettings::default())
    }
}

/// What `ClipboardService::write` puts on the clipboard; every present field becomes its format(s).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WritePayload {
    pub text: Option<String>,
    /// A full CF_HTML payload, written as given.
    pub html: Option<String>,
    pub rtf: Option<String>,
    pub image: Option<Image>,
    pub files: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_exe_file_names_case_insensitively() {
        let filter = CaptureFilter { ignored_apps: vec!["KeePass.exe".into()], ..CaptureFilter::default() };
        assert!(filter.ignores(Path::new(r"C:\Program Files\KeePass\keepass.EXE")));
        assert!(!filter.ignores(Path::new(r"C:\Windows\notepad.exe")));
        assert!(!filter.ignores(Path::new("")));
    }

    #[test]
    fn filter_follows_history_settings() {
        let settings = HistorySettings { enabled: false, max_item_mb: 2, ..HistorySettings::default() };
        let filter = CaptureFilter::from(&settings);
        assert!(filter.paused);
        assert_eq!(filter.max_bytes, 2 * 1024 * 1024);
        assert_eq!(CaptureFilter::default().max_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn captured_kind_and_size() {
        let text = Captured { text: Some("abc".into()), html: Some("<b>abc</b>".into()), ..Captured::default() };
        assert_eq!((text.kind(), text.byte_len()), (Some(ClipKind::Text), 13));
        let image = Captured { image: Some(Image::new(3, 2)), ..Captured::default() };
        assert_eq!((image.kind(), image.byte_len()), (Some(ClipKind::Image), 24));
        let files = Captured { files: vec![PathBuf::from(r"C:\a.txt")], ..Captured::default() };
        assert_eq!((files.kind(), files.byte_len()), (Some(ClipKind::Files), 8));
        assert_eq!(Captured::default().kind(), None);
    }
}
