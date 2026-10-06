//! Embedded TSV data parsed into catalogs (one per `Kind`, parsed once).

use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use tuck_core::SkinTone;

use crate::popular;
use crate::search::SearchKeys;

const EMOJI_TSV: &str = include_str!("../data/emoji.tsv");
const SYMBOLS_TSV: &str = include_str!("../data/symbols.tsv");
const KAOMOJI_TSV: &str = include_str!("../data/kaomoji.tsv");

const FIRST_TONE_MODIFIER: u32 = 0x1F3FB;
const TONE_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Emoji,
    Kaomoji,
    Symbol,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Emoji, Kind::Kaomoji, Kind::Symbol];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Lowercase slug, e.g. `smileys-emotion`.
    pub key: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub text: String,
    /// CLDR name for emoji, lowercase Unicode name for symbols, the first keyword for kaomoji.
    pub name: String,
    pub keywords: Vec<String>,
    /// Index into `Catalog::groups`.
    pub group: u16,
    /// Empty or the five uniform skin-tone variants, light to dark.
    pub toned: Vec<String>,
    /// Emoji version that introduced the emoji; 0.0 for symbols and kaomoji.
    pub version: f32,
}

impl Entry {
    /// The variant for `tone`; the base text for `SkinTone::Default` and for entries without tones.
    pub fn with_tone(&self, tone: SkinTone) -> &str {
        let variant = tone
            .modifier()
            .and_then(|modifier| (modifier as u32).checked_sub(FIRST_TONE_MODIFIER))
            .and_then(|offset| self.toned.get(offset as usize));
        variant.map_or(self.text.as_str(), String::as_str)
    }
}

pub struct Catalog {
    pub kind: Kind,
    pub groups: Vec<Group>,
    pub entries: Vec<Entry>,
    pub(crate) search_keys: Vec<SearchKeys>,
    /// Position in the curated popularity list (lower is more popular); `u16::MAX` when absent.
    pub(crate) popularity: Vec<u16>,
}

/// The catalog for `kind`, parsed from the embedded data on first use.
pub fn catalog(kind: Kind) -> &'static Catalog {
    static EMOJI: OnceLock<Catalog> = OnceLock::new();
    static KAOMOJI: OnceLock<Catalog> = OnceLock::new();
    static SYMBOLS: OnceLock<Catalog> = OnceLock::new();
    let cell = match kind {
        Kind::Emoji => &EMOJI,
        Kind::Kaomoji => &KAOMOJI,
        Kind::Symbol => &SYMBOLS,
    };
    cell.get_or_init(|| Catalog::parse_embedded(kind).expect("embedded catalog data is valid"))
}

/// Fingerprint of the embedded emoji data, part of the coverage cache key.
pub(crate) fn emoji_data_version() -> u64 {
    EMOJI_TSV.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

impl Catalog {
    pub(crate) fn parse_embedded(kind: Kind) -> Result<Catalog> {
        let tsv = match kind {
            Kind::Emoji => EMOJI_TSV,
            Kind::Kaomoji => KAOMOJI_TSV,
            Kind::Symbol => SYMBOLS_TSV,
        };
        Catalog::parse(kind, tsv)
    }

    fn parse(kind: Kind, tsv: &str) -> Result<Catalog> {
        let mut groups: Vec<Group> = Vec::new();
        let mut entries = Vec::new();
        for (line_number, line) in tsv.lines().enumerate().filter(|(_, line)| !line.is_empty()) {
            let entry = parse_row(kind, line, |title| group_index(&mut groups, title))
                .with_context(|| format!("{kind:?} data line {}", line_number + 1))?;
            entries.push(entry);
        }
        let search_keys =
            entries.iter().map(|entry| SearchKeys::new(kind, entry, &groups[entry.group as usize])).collect();
        let popularity = match kind {
            Kind::Emoji => popular::ranks(&entries),
            Kind::Kaomoji | Kind::Symbol => vec![u16::MAX; entries.len()],
        };
        Ok(Catalog { kind, groups, entries, search_keys, popularity })
    }
}

fn parse_row(kind: Kind, line: &str, mut group_of: impl FnMut(&str) -> u16) -> Result<Entry> {
    let mut columns = line.split('\t');
    let mut next = |what: &str| columns.next().with_context(|| format!("missing {what} column"));
    let group = group_of(next("group")?);
    let text = next("text")?.to_string();
    match kind {
        Kind::Emoji => {
            let name = next("name")?.to_string();
            let keywords = split_list(next("keywords")?);
            let version = next("version")?.parse().context("version")?;
            let toned = split_list(next("toned")?);
            if !toned.is_empty() && toned.len() != TONE_COUNT {
                bail!("toned has {} variants, expected {TONE_COUNT}", toned.len());
            }
            Ok(Entry { text, name, keywords, group, toned, version })
        }
        Kind::Symbol => {
            let name = next("name")?.to_string();
            Ok(Entry { text, name, keywords: Vec::new(), group, toned: Vec::new(), version: 0.0 })
        }
        Kind::Kaomoji => {
            let keywords = split_list(next("keywords")?);
            let name = keywords.first().cloned().unwrap_or_default();
            Ok(Entry { text, name, keywords, group, toned: Vec::new(), version: 0.0 })
        }
    }
}

fn split_list(column: &str) -> Vec<String> {
    column.split('|').filter(|item| !item.is_empty()).map(str::to_string).collect()
}

/// Groups arrive in runs, so the last one is almost always the match.
fn group_index(groups: &mut Vec<Group>, title: &str) -> u16 {
    if let Some(index) = groups.iter().rposition(|group| group.title == title) {
        return index as u16;
    }
    groups.push(Group { key: slug(title), title: title.to_string() });
    (groups.len() - 1) as u16
}

fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}
