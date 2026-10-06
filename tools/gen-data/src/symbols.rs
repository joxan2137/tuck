//! symbols.tsv: fixed Unicode ranges with names from UnicodeData.txt.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use anyhow::{Context, Result};

pub struct Row {
    pub group: &'static str,
    pub text: char,
    pub name: String,
}

enum Source {
    Range(u32, u32),
    Chars(&'static str),
}

struct SymbolGroup {
    title: &'static str,
    sources: &'static [Source],
    skip_emoji_presentation: bool,
}

use Source::{Chars, Range};

const GROUPS: &[SymbolGroup] = &[
    SymbolGroup {
        title: "Punctuation",
        sources: &[Range(0x2010, 0x2027), Range(0x2030, 0x205E), Chars("¡¿«»§¶")],
        skip_emoji_presentation: false,
    },
    SymbolGroup {
        title: "Currency",
        sources: &[Chars("$¢£¤¥"), Range(0x20A0, 0x20C1)],
        skip_emoji_presentation: false,
    },
    SymbolGroup {
        title: "Arrows",
        sources: &[Range(0x2190, 0x21FF), Range(0x27F0, 0x27FF), Range(0x2900, 0x297F)],
        skip_emoji_presentation: false,
    },
    SymbolGroup {
        title: "Math",
        sources: &[Chars("±×÷¬°µ¹²³¼½¾"), Range(0x2150, 0x215F), Range(0x2200, 0x22FF)],
        skip_emoji_presentation: false,
    },
    SymbolGroup { title: "Geometric", sources: &[Range(0x25A0, 0x25FF)], skip_emoji_presentation: false },
    SymbolGroup {
        title: "Letterlike & technical",
        sources: &[Chars("©®"), Range(0x2100, 0x214F), Range(0x2300, 0x23FF)],
        skip_emoji_presentation: true,
    },
    SymbolGroup {
        title: "Latin accented",
        sources: &[Range(0x00C0, 0x00D6), Range(0x00D8, 0x00F6), Range(0x00F8, 0x017F)],
        skip_emoji_presentation: false,
    },
    SymbolGroup {
        title: "Greek",
        sources: &[Range(0x0391, 0x03A9), Range(0x03B1, 0x03C9)],
        skip_emoji_presentation: false,
    },
    SymbolGroup { title: "Box & blocks", sources: &[Range(0x2500, 0x259F)], skip_emoji_presentation: false },
];

struct CodePoint {
    name: String,
    category: String,
}

impl CodePoint {
    /// Controls, format characters, separators and combining marks cannot stand alone in a picker cell.
    fn is_pickable(&self) -> bool {
        !self.category.starts_with(['C', 'Z', 'M'])
    }
}

pub fn build_rows(unicode_data: &str, emoji_test: &str) -> Result<Vec<Row>> {
    let code_points = parse_unicode_data(unicode_data)?;
    let emoji_presentation = emoji_presentation_code_points(emoji_test);
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for group in GROUPS {
        for source in group.sources {
            let candidates: Vec<u32> = match source {
                Range(first, last) => (*first..=*last).collect(),
                Chars(chars) => chars.chars().map(u32::from).collect(),
            };
            for code_point in candidates {
                let Some(info) = code_points.get(&code_point).filter(|info| info.is_pickable()) else { continue };
                if group.skip_emoji_presentation && emoji_presentation.contains(&code_point) {
                    continue;
                }
                let Some(text) = char::from_u32(code_point) else { continue };
                if seen.insert(text) {
                    rows.push(Row { group: group.title, text, name: info.name.to_lowercase() });
                }
            }
        }
    }
    Ok(rows)
}

pub fn to_tsv(rows: &[Row]) -> String {
    let mut tsv = String::new();
    for row in rows {
        let _ = writeln!(tsv, "{}\t{}\t{}", row.group, row.text, row.name);
    }
    tsv
}

fn parse_unicode_data(unicode_data: &str) -> Result<HashMap<u32, CodePoint>> {
    let mut code_points = HashMap::new();
    for line in unicode_data.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split(';');
        let (Some(hex), Some(name), Some(category)) = (fields.next(), fields.next(), fields.next()) else {
            anyhow::bail!("malformed UnicodeData line {line:?}");
        };
        let code_point = u32::from_str_radix(hex, 16).with_context(|| format!("code point in {line:?}"))?;
        code_points.insert(code_point, CodePoint { name: name.to_string(), category: category.to_string() });
    }
    Ok(code_points)
}

/// Single code points that are fully-qualified without U+FE0F render as color emoji by default.
fn emoji_presentation_code_points(emoji_test: &str) -> HashSet<u32> {
    emoji_test
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(';'))
        .filter(|(_, rest)| rest.trim_start().starts_with("fully-qualified"))
        .filter_map(|(code_points, _)| {
            let mut hex = code_points.split_whitespace();
            let (first, rest) = (hex.next()?, hex.next());
            rest.is_none().then(|| u32::from_str_radix(first, 16).ok()).flatten()
        })
        .collect()
}
