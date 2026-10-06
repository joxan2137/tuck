use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

use crate::image::Image;

pub type ClipId = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipKind {
    Text,
    Image,
    Files,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    /// `html` is the full CF_HTML payload ("Version:0.9 ... <html>..."), `rtf` the raw RTF text.
    Text { text: String, html: Option<String>, rtf: Option<String> },
    /// Pixels live in the store under `blob`.
    Image { blob: String, width: u32, height: u32 },
    Files { paths: Vec<PathBuf> },
}

impl ClipContent {
    pub fn kind(&self) -> ClipKind {
        match self {
            Self::Text { .. } => ClipKind::Text,
            Self::Image { .. } => ClipKind::Image,
            Self::Files { .. } => ClipKind::Files,
        }
    }

    /// What "paste as plain text" and search use: the text, or one path per line. Images have none.
    pub fn plain_text(&self) -> Option<String> {
        match self {
            Self::Text { text, .. } => Some(text.clone()),
            Self::Image { .. } => None,
            Self::Files { paths } => {
                Some(paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\r\n"))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceApp {
    pub exe: PathBuf,
    /// FileDescription from the version resource, else the file stem.
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipItem {
    pub id: ClipId,
    pub content: ClipContent,
    /// See `text_hash` / `image_hash` / `files_hash`; equal hashes fold into one item.
    pub hash: u64,
    /// Unix milliseconds.
    pub created_ms: i64,
    /// Last copy or paste, Unix milliseconds. History is ordered by this, newest first.
    pub used_ms: i64,
    pub pinned: bool,
    pub source: Option<SourceApp>,
    /// Text + html + rtf bytes, image width*height*4, or path bytes.
    pub bytes: u64,
}

pub fn text_hash(text: &str) -> u64 {
    let mut h = Xxh3::new();
    h.update(b"text\0");
    h.update(text.as_bytes());
    h.digest()
}

pub fn image_hash(image: &Image) -> u64 {
    let mut h = Xxh3::new();
    h.update(b"image\0");
    h.update(&image.width.to_le_bytes());
    h.update(&image.height.to_le_bytes());
    h.update(&image.data);
    h.digest()
}

pub fn files_hash(paths: &[PathBuf]) -> u64 {
    let mut h = Xxh3::new();
    h.update(b"files\0");
    for path in paths {
        h.update(path.to_string_lossy().to_lowercase().as_bytes());
        h.update(b"\0");
    }
    h.digest()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// How a text item is presented (link row, color swatch, monospaced code...). Paste is unaffected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    Plain,
    Url,
    Email,
    Color(Rgba),
    /// An absolute Windows path (`C:\...`, `\\server\share\...`).
    Path,
    Code,
}

const MAX_TOKEN_LEN: usize = 2048;

pub fn classify(text: &str) -> TextKind {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return TextKind::Plain;
    }
    if trimmed.len() <= 64
        && let Some(color) = parse_color(trimmed)
    {
        return TextKind::Color(color);
    }
    let single_token = trimmed.len() <= MAX_TOKEN_LEN && !trimmed.contains(char::is_whitespace);
    if single_token {
        if is_url(trimmed) {
            return TextKind::Url;
        }
        if is_email(trimmed) {
            return TextKind::Email;
        }
    }
    if !trimmed.contains('\n') && is_windows_path(trimmed) {
        return TextKind::Path;
    }
    if looks_like_code(trimmed) {
        return TextKind::Code;
    }
    TextKind::Plain
}

fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let after_scheme = ["https://", "http://", "ftp://", "file://"].iter().find_map(|scheme| lower.strip_prefix(scheme));
    match after_scheme {
        Some(rest) => !rest.is_empty(),
        None => lower.strip_prefix("www.").is_some_and(|rest| rest.contains('.') && !rest.starts_with('.')),
    }
}

fn is_email(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    s.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

fn is_windows_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    let unc = s.starts_with("\\\\") && s[2..].contains('\\');
    (drive || unc) && !s.contains(['<', '>', '"', '|', '?', '*'])
}

fn parse_color(s: &str) -> Option<Rgba> {
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    let lower = s.to_ascii_lowercase();
    let inner = lower.strip_prefix("rgba(").or_else(|| lower.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let channel = |p: &str| p.parse::<u8>().ok();
    let alpha = match parts.get(3) {
        Some(a) => (a.parse::<f32>().ok().filter(|a| (0.0..=1.0).contains(a))? * 255.0).round() as u8,
        None => 255,
    };
    Some(Rgba { r: channel(parts[0])?, g: channel(parts[1])?, b: channel(parts[2])?, a: alpha })
}

fn parse_hex_color(hex: &str) -> Option<Rgba> {
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let nibble = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|v| v * 17);
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => Some(Rgba { r: nibble(0)?, g: nibble(1)?, b: nibble(2)?, a: 255 }),
        4 => Some(Rgba { r: nibble(0)?, g: nibble(1)?, b: nibble(2)?, a: nibble(3)? }),
        6 => Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }),
        8 => Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
        _ => None,
    }
}

fn looks_like_code(s: &str) -> bool {
    const MARKERS: [&str; 14] = [
        ";", "{", "}", "=>", "->", "fn ", "def ", "function ", "</", "#include", "import ", "let ", "const ", "return ",
    ];
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    let code_lines = lines.iter().filter(|l| MARKERS.iter().any(|m| l.contains(m))).count();
    code_lines >= 2 && code_lines * 10 >= lines.len() * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_shapes() {
        assert_eq!(classify("https://example.com/a?b=1"), TextKind::Url);
        assert_eq!(classify("www.example.com"), TextKind::Url);
        assert_eq!(classify("someone@example.com"), TextKind::Email);
        assert_eq!(classify("#0A84FF"), TextKind::Color(Rgba { r: 10, g: 132, b: 255, a: 255 }));
        assert_eq!(classify("#fff"), TextKind::Color(Rgba { r: 255, g: 255, b: 255, a: 255 }));
        assert_eq!(classify("rgba(1, 2, 3, 0.5)"), TextKind::Color(Rgba { r: 1, g: 2, b: 3, a: 128 }));
        assert_eq!(classify(r"C:\Users\you\Documents\a file.txt"), TextKind::Path);
        assert_eq!(classify(r"\\server\share\x"), TextKind::Path);
        assert_eq!(classify("fn main() {\n    println!(\"hi\");\n}"), TextKind::Code);
        assert_eq!(classify("hello world"), TextKind::Plain);
        assert_eq!(classify("#hashtag"), TextKind::Plain);
        assert_eq!(classify("a@b"), TextKind::Plain);
    }

    #[test]
    fn hashes_are_stable_and_distinct() {
        assert_eq!(text_hash("abc"), text_hash("abc"));
        assert_ne!(text_hash("abc"), text_hash("abd"));
        let img = Image::new(2, 2);
        assert_eq!(image_hash(&img), image_hash(&img.clone()));
        assert_ne!(files_hash(&[PathBuf::from("a")]), files_hash(&[PathBuf::from("b")]));
        assert_eq!(files_hash(&[PathBuf::from("A")]), files_hash(&[PathBuf::from("a")]));
    }
}
