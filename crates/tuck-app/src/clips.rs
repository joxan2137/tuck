//! Clipboard history ↔ panel and clipboard: what a copy becomes in the history, the rows the panel draws (with
//! cached display text, thumbnails and app icons), and what a paste puts back on the clipboard.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use tuck_clip::{Captured, WritePayload};
use tuck_core::clip::{files_hash, image_hash, text_hash};
use tuck_core::{ClipContent, ClipId, ClipItem, History, Image, TextKind, classify};
use tuck_panel::ClipRow;
use tuck_ui::Bitmap;

/// Cards show a few lines; the panel never needs more of a huge copy than this.
pub const DISPLAY_TEXT_LIMIT: usize = 4096;
/// Thumbnails are about twice the card's image area (the card's content width × 110 DIP).
pub const THUMBNAIL_MAX: (u32, u32) = (664, 220);
/// App icons in the meta row are 14 DIP; this covers scale 2.
pub const APP_ICON_PX: u32 = 32;

/// A copy as the history stores it; `image` holds the pixels for the blob store.
pub struct Copied {
    pub content: ClipContent,
    pub hash: u64,
    pub image: Option<Image>,
}

/// Files win over text, text over image (DESIGN §5). Images are named `blob`.
pub fn copied(captured: Captured, blob: String) -> Option<Copied> {
    if !captured.files.is_empty() {
        let hash = files_hash(&captured.files);
        return Some(Copied { content: ClipContent::Files { paths: captured.files }, hash, image: None });
    }
    if let Some(text) = captured.text {
        let hash = text_hash(&text);
        let content = ClipContent::Text { text, html: captured.html, rtf: captured.rtf };
        return Some(Copied { content, hash, image: None });
    }
    let image = captured.image?;
    let content = ClipContent::Image { blob, width: image.width, height: image.height };
    Some(Copied { content, hash: image_hash(&image), image: Some(image) })
}

/// What pasting `content` writes. Plain keeps only text (file lists become their paths); images come from the
/// store, so their pixels are passed in.
pub fn payload(content: &ClipContent, plain: bool, image: Option<Image>) -> WritePayload {
    match content {
        ClipContent::Text { text, html, rtf } => WritePayload {
            text: Some(text.clone()),
            html: if plain { None } else { html.clone() },
            rtf: if plain { None } else { rtf.clone() },
            ..WritePayload::default()
        },
        ClipContent::Files { .. } if plain => WritePayload { text: content.plain_text(), ..WritePayload::default() },
        ClipContent::Files { paths } => WritePayload { files: paths.clone(), ..WritePayload::default() },
        ClipContent::Image { .. } => WritePayload { image, ..WritePayload::default() },
    }
}

/// Text cut to `DISPLAY_TEXT_LIMIT` bytes without formats; other content as is.
pub fn display_content(content: &ClipContent) -> ClipContent {
    match content {
        ClipContent::Text { text, .. } => ClipContent::Text {
            text: text[..text.floor_char_boundary(DISPLAY_TEXT_LIMIT)].to_string(),
            html: None,
            rtf: None,
        },
        other => other.clone(),
    }
}

pub fn text_kind(content: &ClipContent) -> TextKind {
    match content {
        ClipContent::Text { text, .. } => classify(text),
        _ => TextKind::Plain,
    }
}

/// What the panel's rows are made of, kept between refreshes so typing in the search never re-classifies or copies
/// whole texts.
#[derive(Default)]
pub struct RowCache {
    display: HashMap<ClipId, (u64, ClipContent, TextKind)>,
    thumbnails: HashMap<ClipId, Rc<Bitmap>>,
    /// None while the icon is being fetched or when the app has none.
    icons: HashMap<PathBuf, Option<Rc<Bitmap>>>,
}

impl RowCache {
    /// Rows for `ids` in that order, and the source executables whose icons were never asked for (now marked as
    /// pending).
    pub fn rows(&mut self, history: &History, ids: &[ClipId]) -> (Vec<ClipRow>, Vec<PathBuf>) {
        let by_id: HashMap<ClipId, &ClipItem> = history.items().iter().map(|item| (item.id, item)).collect();
        let mut missing_icons = Vec::new();
        let mut rows = Vec::with_capacity(ids.len());
        for item in ids.iter().filter_map(|id| by_id.get(id)) {
            let (content, kind) = self.display_of(item);
            let app_icon = item.source.as_ref().and_then(|source| match self.icons.get(&source.exe) {
                Some(icon) => icon.clone(),
                None => {
                    self.icons.insert(source.exe.clone(), None);
                    missing_icons.push(source.exe.clone());
                    None
                }
            });
            rows.push(ClipRow {
                item: ClipItem { content, source: item.source.clone(), ..clone_meta(item) },
                kind,
                thumbnail: self.thumbnails.get(&item.id).cloned(),
                app_icon,
                app_name: item.source.as_ref().map(|source| source.name.clone()),
            });
        }
        (rows, missing_icons)
    }

    fn display_of(&mut self, item: &ClipItem) -> (ClipContent, TextKind) {
        let cached = self.display.get(&item.id).filter(|(hash, _, _)| *hash == item.hash);
        if let Some((_, content, kind)) = cached {
            return (content.clone(), *kind);
        }
        let content = display_content(&item.content);
        let kind = text_kind(&content);
        self.display.insert(item.id, (item.hash, content.clone(), kind));
        (content, kind)
    }

    pub fn thumbnail(&self, id: ClipId) -> Option<Rc<Bitmap>> {
        self.thumbnails.get(&id).cloned()
    }

    pub fn set_thumbnail(&mut self, id: ClipId, bitmap: Rc<Bitmap>) {
        self.thumbnails.insert(id, bitmap);
    }

    pub fn set_icon(&mut self, exe: PathBuf, icon: Option<Rc<Bitmap>>) {
        self.icons.insert(exe, icon);
    }

    /// Drops what belonged to a removed item.
    pub fn forget(&mut self, id: ClipId) {
        self.display.remove(&id);
        self.thumbnails.remove(&id);
    }
}

/// Every field but content and source, which callers fill in.
fn clone_meta(item: &ClipItem) -> ClipItem {
    ClipItem {
        id: item.id,
        content: ClipContent::Files { paths: Vec::new() },
        hash: item.hash,
        created_ms: item.created_ms,
        used_ms: item.used_ms,
        pinned: item.pinned,
        source: None,
        bytes: item.bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuck_core::{SourceApp, content_bytes};

    fn captured_text(text: &str) -> Captured {
        Captured { text: Some(text.into()), html: Some("<b>x</b>".into()), ..Captured::default() }
    }

    #[test]
    fn copies_prefer_files_then_text_then_image() {
        let files = Captured {
            files: vec![PathBuf::from(r"C:\Users\you\a.txt")],
            text: Some("ignored".into()),
            ..Captured::default()
        };
        assert!(matches!(copied(files, "b".into()).unwrap().content, ClipContent::Files { .. }));
        let text = copied(captured_text("hello"), "b".into()).unwrap();
        assert_eq!(text.hash, text_hash("hello"));
        assert!(matches!(text.content, ClipContent::Text { html: Some(_), .. }));
        let image = Captured { image: Some(Image::new(3, 2)), ..Captured::default() };
        let image = copied(image, "0000000000000007".into()).unwrap();
        assert_eq!(image.content, ClipContent::Image { blob: "0000000000000007".into(), width: 3, height: 2 });
        assert!(image.image.is_some());
        assert!(copied(Captured::default(), "b".into()).is_none());
    }

    #[test]
    fn plain_payloads_drop_formats_and_turn_files_into_paths() {
        let text = ClipContent::Text { text: "a".into(), html: Some("h".into()), rtf: Some("r".into()) };
        let formatted = payload(&text, false, None);
        assert_eq!((formatted.html.as_deref(), formatted.rtf.as_deref()), (Some("h"), Some("r")));
        let plain = payload(&text, true, None);
        assert_eq!((plain.text.as_deref(), plain.html, plain.rtf), (Some("a"), None, None));
        let paths = vec![PathBuf::from(r"C:\Users\you\a.txt"), PathBuf::from(r"C:\Users\you\b.txt")];
        let files = ClipContent::Files { paths: paths.clone() };
        assert_eq!(payload(&files, false, None).files, paths);
        assert_eq!(payload(&files, true, None).text.as_deref(), Some("C:\\Users\\you\\a.txt\r\nC:\\Users\\you\\b.txt"));
        let image = ClipContent::Image { blob: "x".into(), width: 1, height: 1 };
        assert_eq!(payload(&image, true, Some(Image::new(1, 1))).image, Some(Image::new(1, 1)));
    }

    #[test]
    fn display_text_is_cut_on_a_char_boundary() {
        let long = "é".repeat(DISPLAY_TEXT_LIMIT);
        let ClipContent::Text { text, html, .. } =
            display_content(&ClipContent::Text { text: long, html: Some("h".into()), rtf: None })
        else {
            panic!("text stays text");
        };
        assert!(text.len() <= DISPLAY_TEXT_LIMIT && text.len() >= DISPLAY_TEXT_LIMIT - 1);
        assert!(html.is_none());
    }

    #[test]
    fn rows_follow_ids_and_ask_once_for_each_icon() {
        let mut history = History::new();
        let source = SourceApp { exe: PathBuf::from(r"C:\Program Files\App\app.exe"), name: "App".into() };
        for (n, text) in ["https://example.com", "#0A84FF", "plain"].iter().enumerate() {
            let content = ClipContent::Text { text: text.to_string(), html: None, rtf: None };
            let bytes = content_bytes(&content);
            history.add(content, text_hash(text), bytes, Some(source.clone()), 1_000 + n as i64);
        }
        let mut cache = RowCache::default();
        let (rows, missing) = cache.rows(&history, &[1, 3]);
        assert_eq!(rows.iter().map(|r| r.item.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(rows[0].kind, TextKind::Url);
        assert_eq!(rows[0].app_name.as_deref(), Some("App"));
        assert_eq!(missing, vec![source.exe.clone()]);
        let (rows, missing) = cache.rows(&history, &history.search(""));
        assert!(missing.is_empty());
        assert_eq!(rows.iter().map(|r| r.item.id).collect::<Vec<_>>(), [3, 2, 1]);
        assert!(matches!(rows[1].kind, TextKind::Color(_)));
    }
}
