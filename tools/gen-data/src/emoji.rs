//! emoji.tsv: fully-qualified base emoji from emoji-test.txt with CLDR names, keywords and uniform skin-tone variants.

use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use serde_json::Value;

const VARIATION_SELECTOR: char = '\u{FE0F}';
const FIRST_TONE_MODIFIER: u32 = 0x1F3FB;
const TONE_COUNT: usize = 5;
const COMPONENT_GROUP: &str = "Component";

pub struct Row {
    pub group: String,
    pub text: String,
    pub name: String,
    pub keywords: Vec<String>,
    pub version: String,
    pub toned: Vec<String>,
}

struct Annotation {
    name: Option<String>,
    keywords: Vec<String>,
}

/// CLDR English annotations keyed by the emoji without variation selectors (CLDR omits U+FE0F).
pub struct Annotations(HashMap<String, Annotation>);

impl Annotations {
    /// `annotations.json` entries win over `annotationsDerived.json` ones.
    pub fn parse(annotations_json: &str, derived_json: &str) -> Result<Self> {
        let mut by_emoji = HashMap::new();
        for (json, root) in [(derived_json, "annotationsDerived"), (annotations_json, "annotations")] {
            let document: Value = serde_json::from_str(json).with_context(|| format!("parsing {root} JSON"))?;
            let entries = document[root]["annotations"].as_object().with_context(|| format!("{root}.annotations"))?;
            for (emoji, entry) in entries {
                let name = entry["tts"][0].as_str().map(str::to_string);
                let keywords = entry["default"]
                    .as_array()
                    .map(|words| words.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                by_emoji.insert(strip_variation_selectors(emoji), Annotation { name, keywords });
            }
        }
        Ok(Self(by_emoji))
    }

    fn get(&self, emoji: &str) -> Option<&Annotation> {
        self.0.get(&strip_variation_selectors(emoji))
    }
}

struct Sequence {
    group: String,
    text: String,
    name: String,
    version: String,
}

pub fn build_rows(emoji_test: &str, annotations: &Annotations) -> Result<Vec<Row>> {
    let sequences = parse_emoji_test(emoji_test)?;
    let mut toned_variants: HashMap<String, [Option<String>; TONE_COUNT]> = HashMap::new();
    for sequence in sequences.iter().filter(|sequence| has_tone_modifier(&sequence.text)) {
        if let Some(tone) = uniform_tone(&sequence.text) {
            let slot = &mut toned_variants.entry(tone_free_key(&sequence.text)).or_default()[tone];
            slot.get_or_insert_with(|| sequence.text.clone());
        }
    }

    let mut rows = Vec::new();
    for sequence in sequences.into_iter().filter(|sequence| !has_tone_modifier(&sequence.text)) {
        let annotation = annotations.get(&sequence.text);
        let name = annotation.and_then(|a| a.name.clone()).unwrap_or(sequence.name);
        let keywords = annotation.map(|a| a.keywords.clone()).unwrap_or_default();
        let toned = toned_variants
            .get(&tone_free_key(&sequence.text))
            .filter(|variants| variants.iter().all(Option::is_some))
            .map(|variants| variants.iter().flatten().cloned().collect())
            .unwrap_or_default();
        rows.push(Row { group: sequence.group, text: sequence.text, name, keywords, version: sequence.version, toned });
    }
    if rows.is_empty() {
        bail!("emoji-test.txt produced no rows");
    }
    Ok(rows)
}

pub fn to_tsv(rows: &[Row]) -> String {
    let mut tsv = String::new();
    for row in rows {
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{}",
            row.group,
            row.text,
            row.name,
            row.keywords.join("|"),
            row.version,
            row.toned.join("|")
        );
    }
    tsv
}

fn parse_emoji_test(emoji_test: &str) -> Result<Vec<Sequence>> {
    let mut group = String::new();
    let mut sequences = Vec::new();
    for line in emoji_test.lines() {
        if let Some(title) = line.strip_prefix("# group: ") {
            group = title.trim().to_string();
            continue;
        }
        if line.starts_with('#') || group == COMPONENT_GROUP {
            continue;
        }
        let Some((code_points, rest)) = line.split_once(';') else { continue };
        let Some((status, comment)) = rest.split_once('#') else { continue };
        if status.trim() != "fully-qualified" {
            continue;
        }
        let text = code_points
            .split_whitespace()
            .map(|hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32))
            .collect::<Option<String>>()
            .with_context(|| format!("bad code points in {line:?}"))?;
        let mut comment_fields = comment.trim().splitn(3, ' ');
        let (_emoji, version, name) = (comment_fields.next(), comment_fields.next(), comment_fields.next());
        let (Some(version), Some(name)) = (version.and_then(|v| v.strip_prefix('E')), name) else {
            bail!("unexpected comment in {line:?}");
        };
        sequences.push(Sequence { group: group.clone(), text, name: name.to_string(), version: version.to_string() });
    }
    Ok(sequences)
}

fn tone_index(c: char) -> Option<usize> {
    let offset = (c as u32).checked_sub(FIRST_TONE_MODIFIER)?;
    (offset < TONE_COUNT as u32).then_some(offset as usize)
}

fn has_tone_modifier(text: &str) -> bool {
    text.chars().any(|c| tone_index(c).is_some())
}

/// The tone shared by every modifier in the sequence, `None` when people differ.
fn uniform_tone(text: &str) -> Option<usize> {
    let mut tones = text.chars().filter_map(tone_index);
    let first = tones.next()?;
    tones.all(|tone| tone == first).then_some(first)
}

/// Base sequence identity: tone modifiers and variation selectors removed.
fn tone_free_key(text: &str) -> String {
    text.chars().filter(|&c| tone_index(c).is_none() && c != VARIATION_SELECTOR).collect()
}

fn strip_variation_selectors(text: &str) -> String {
    text.chars().filter(|&c| c != VARIATION_SELECTOR).collect()
}
