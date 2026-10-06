//! Query folding, scoring (DESIGN §6) and ranking.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::catalog::{Entry, Group, Kind, catalog};
use crate::usage::Usage;

const EXACT_NAME: u32 = 100;
const NAME_WORD: u32 = 85;
const NAME_WORD_PREFIX: u32 = 70;
const KEYWORD: u32 = 65;
const KEYWORD_PREFIX: u32 = 55;
const NAME_SUBSTRING: u32 = 30;
const LITERAL_MATCH: u32 = 1000;

const VARIATION_SELECTOR: char = '\u{FE0F}';
const MIN_PREFIXED_HEX_DIGITS: usize = 2;
const MIN_BARE_HEX_DIGITS: usize = 4;
const MAX_HEX_DIGITS: usize = 6;

/// Folded name and keywords of one entry, prepared at parse time so a keystroke only compares strings.
pub(crate) struct SearchKeys {
    name: String,
    /// Folded words, each followed by a space: splitting on ' ' recovers them without allocating.
    name_words: String,
    name_word_count: usize,
    keyword_words: String,
}

impl SearchKeys {
    /// Kaomoji also match the words of their group title.
    pub(crate) fn new(kind: Kind, entry: &Entry, group: &Group) -> Self {
        let name = fold(&entry.name);
        let mut name_words = String::with_capacity(name.len() + 1);
        push_words(&mut name_words, &name);
        let name_word_count = name_words.split_terminator(' ').count();

        let keyword_bytes: usize = entry.keywords.iter().map(|keyword| keyword.len() + 1).sum();
        let mut keyword_words = String::with_capacity(keyword_bytes);
        for keyword in &entry.keywords {
            push_folded_words(&mut keyword_words, keyword);
        }
        if kind == Kind::Kaomoji {
            push_folded_words(&mut keyword_words, &group.title);
        }
        Self { name, name_words, name_word_count, keyword_words }
    }

    fn name_words(&self) -> impl Iterator<Item = &str> {
        self.name_words.split_terminator(' ')
    }

    fn keyword_words(&self) -> impl Iterator<Item = &str> {
        self.keyword_words.split_terminator(' ')
    }
}

/// Entry indices matching `query`, best first. An empty query returns every entry in catalog order.
pub fn search(kind: Kind, query: &str, usage: &Usage) -> Vec<u32> {
    search_at(kind, query, usage, tuck_core::now_ms())
}

pub(crate) fn search_at(kind: Kind, query: &str, usage: &Usage, now_ms: i64) -> Vec<u32> {
    let catalog = catalog(kind);
    let query = query.trim();
    if query.is_empty() {
        return (0..catalog.entries.len() as u32).collect();
    }
    let folded_query = fold(query);
    let terms: Vec<&str> = words(&folded_query).collect();
    let code_point = (kind == Kind::Symbol).then(|| parse_code_point(query)).flatten();

    let mut hits = Vec::new();
    for (index, (entry, keys)) in catalog.entries.iter().zip(&catalog.search_keys).enumerate() {
        let is_literal =
            is_literal_match(entry, query) || code_point.is_some_and(|code| is_single_char(&entry.text, code));
        let Some(base_score) = is_literal.then_some(LITERAL_MATCH).or_else(|| score_terms(&terms, keys)) else {
            continue;
        };
        hits.push(Hit {
            score: base_score + usage.boost(kind, &entry.text, now_ms),
            name_words: keys.name_word_count,
            popularity: catalog.popularity[index],
            index: index as u32,
        });
    }
    hits.sort_unstable_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.name_words.cmp(&b.name_words))
            .then(a.popularity.cmp(&b.popularity))
            .then(a.index.cmp(&b.index))
    });
    hits.into_iter().map(|hit| hit.index).collect()
}

struct Hit {
    score: u32,
    name_words: usize,
    popularity: u16,
    index: u32,
}

/// Lowercase with accents removed (NFD, combining marks dropped).
pub(crate) fn fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    text.nfd().filter(|&c| !is_combining_mark(c)).flat_map(char::to_lowercase).collect()
}

fn words(folded: &str) -> impl Iterator<Item = &str> {
    folded.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty())
}

/// Appends the folded words of `text`, each followed by a space.
fn push_folded_words(list: &mut String, text: &str) {
    if text.is_ascii() {
        let start = list.len();
        push_words(list, text);
        list[start..].make_ascii_lowercase();
    } else {
        push_words(list, &fold(text));
    }
}

fn push_words(list: &mut String, text: &str) {
    for word in words(text) {
        list.push_str(word);
        list.push(' ');
    }
}

/// Every term must match; the entry scores the average of its terms, or 100 when the terms spell the whole name.
fn score_terms(terms: &[&str], keys: &SearchKeys) -> Option<u32> {
    if terms.is_empty() {
        return None;
    }
    if terms.iter().copied().eq(keys.name_words()) {
        return Some(EXACT_NAME);
    }
    let mut total = 0;
    for term in terms {
        total += term_score(term, keys)?;
    }
    Some(total / terms.len() as u32)
}

fn term_score(term: &str, keys: &SearchKeys) -> Option<u32> {
    let name_word = keys.name_words().filter_map(|word| word_score(term, word, NAME_WORD, NAME_WORD_PREFIX)).max();
    let keyword = keys.keyword_words().filter_map(|word| word_score(term, word, KEYWORD, KEYWORD_PREFIX)).max();
    let substring = keys.name.contains(term).then_some(NAME_SUBSTRING);
    [name_word, keyword, substring].into_iter().flatten().max()
}

fn word_score(term: &str, word: &str, exact: u32, prefix: u32) -> Option<u32> {
    if word == term {
        Some(exact)
    } else if word.starts_with(term) {
        Some(prefix)
    } else {
        None
    }
}

/// The query is the entry itself (ignoring variation selectors), or one of its skin-tone variants.
fn is_literal_match(entry: &Entry, query: &str) -> bool {
    let same = |candidate: &str| {
        candidate.chars().filter(|&c| c != VARIATION_SELECTOR).eq(query.chars().filter(|&c| c != VARIATION_SELECTOR))
    };
    same(&entry.text) || entry.toned.iter().any(|variant| same(variant))
}

fn is_single_char(text: &str, code_point: u32) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c as u32 == code_point) && chars.next().is_none()
}

/// `U+20AC` / `u+20ac` (2 to 6 hex digits), or bare `20ac` (4 to 6 hex digits).
fn parse_code_point(query: &str) -> Option<u32> {
    let lowered = query.to_ascii_lowercase();
    let (digits, min_digits) = match lowered.strip_prefix("u+") {
        Some(digits) => (digits, MIN_PREFIXED_HEX_DIGITS),
        None => (lowered.as_str(), MIN_BARE_HEX_DIGITS),
    };
    if !(min_digits..=MAX_HEX_DIGITS).contains(&digits.len()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}
