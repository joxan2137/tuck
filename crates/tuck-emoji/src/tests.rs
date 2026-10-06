use std::collections::HashSet;
use std::time::Instant;

use tuck_core::SkinTone;

use crate::catalog::Catalog;
use crate::render_check::FallbackChecker;
use crate::search::search_at;
use crate::{Coverage, Kind, Usage, catalog, popular, search};

const RED_HEART: &str = "\u{2764}\u{FE0F}";
const THUMBS_UP: &str = "\u{1F44D}";
const THUMBS_DOWN: &str = "\u{1F44E}";
const EURO: &str = "\u{20AC}";
const SHRUG: &str = r"¯\_(ツ)_/¯";
const NOW_MS: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn top_text(kind: Kind, query: &str) -> String {
    let hits = search(kind, query, &Usage::default());
    let first = *hits.first().unwrap_or_else(|| panic!("no result for {query:?}"));
    catalog(kind).entries[first as usize].text.clone()
}

fn index_of(kind: Kind, text: &str) -> u32 {
    catalog(kind).entries.iter().position(|entry| entry.text == text).unwrap_or_else(|| panic!("{text} missing")) as u32
}

#[test]
fn catalog_sizes() {
    assert!(catalog(Kind::Emoji).entries.len() >= 1800);
    assert!(catalog(Kind::Symbol).entries.len() >= 900);
    assert!(catalog(Kind::Kaomoji).entries.len() >= 220);
    assert!(catalog(Kind::Kaomoji).groups.len() >= 10);
}

#[test]
fn catalog_invariants() {
    for kind in Kind::ALL {
        let catalog = catalog(kind);
        let mut seen = HashSet::new();
        for entry in &catalog.entries {
            assert!(!entry.text.is_empty());
            assert!((entry.group as usize) < catalog.groups.len());
            assert!(seen.insert(entry.text.as_str()), "{kind:?}: duplicate {:?}", entry.text);
            assert!(entry.toned.is_empty() || entry.toned.len() == 5);
        }
        let unused: Vec<&str> = (0..catalog.groups.len())
            .filter(|&group| catalog.entries.iter().all(|entry| entry.group as usize != group))
            .map(|group| catalog.groups[group].title.as_str())
            .collect();
        assert!(unused.is_empty(), "{kind:?}: empty groups {unused:?}");
    }
}

#[test]
fn emoji_groups_follow_the_unicode_order_without_component() {
    let titles: Vec<&str> = catalog(Kind::Emoji).groups.iter().map(|group| group.title.as_str()).collect();
    assert_eq!(titles.first(), Some(&"Smileys & Emotion"));
    assert_eq!(titles.last(), Some(&"Flags"));
    assert!(!titles.contains(&"Component"));
    assert_eq!(catalog(Kind::Emoji).groups[0].key, "smileys-emotion");
}

#[test]
fn with_tone_returns_the_matching_variant() {
    let thumbs_up = &catalog(Kind::Emoji).entries[index_of(Kind::Emoji, THUMBS_UP) as usize];
    assert_eq!(thumbs_up.toned.len(), 5);
    assert_eq!(thumbs_up.with_tone(SkinTone::Default), THUMBS_UP);
    assert_eq!(thumbs_up.with_tone(SkinTone::Light), "\u{1F44D}\u{1F3FB}");
    assert_eq!(thumbs_up.with_tone(SkinTone::Medium), "\u{1F44D}\u{1F3FD}");
    assert_eq!(thumbs_up.with_tone(SkinTone::Dark), "\u{1F44D}\u{1F3FF}");
}

#[test]
fn multi_person_tones_are_uniform() {
    let handshake_people = "\u{1F9D1}\u{200D}\u{1F91D}\u{200D}\u{1F9D1}";
    let entry = &catalog(Kind::Emoji).entries[index_of(Kind::Emoji, handshake_people) as usize];
    assert_eq!(entry.with_tone(SkinTone::MediumDark), "\u{1F9D1}\u{1F3FE}\u{200D}\u{1F91D}\u{200D}\u{1F9D1}\u{1F3FE}");
}

#[test]
fn entries_without_tones_return_the_base() {
    let heart = &catalog(Kind::Emoji).entries[index_of(Kind::Emoji, RED_HEART) as usize];
    assert!(heart.toned.is_empty());
    assert_eq!(heart.with_tone(SkinTone::Dark), RED_HEART);
    let euro = &catalog(Kind::Symbol).entries[index_of(Kind::Symbol, EURO) as usize];
    assert_eq!(euro.with_tone(SkinTone::Light), EURO);
}

#[test]
fn heart_finds_the_red_heart_first() {
    assert_eq!(top_text(Kind::Emoji, "heart"), RED_HEART);
}

#[test]
fn thumbs_finds_thumbs_up_first() {
    assert_eq!(top_text(Kind::Emoji, "thumbs"), THUMBS_UP);
}

#[test]
fn euro_finds_the_euro_sign_first() {
    assert_eq!(top_text(Kind::Symbol, "euro"), EURO);
}

#[test]
fn shrug_finds_the_shrug_kaomoji_first() {
    assert_eq!(top_text(Kind::Kaomoji, "shrug"), SHRUG);
}

#[test]
fn kaomoji_match_group_titles() {
    let hits = search(Kind::Kaomoji, "sleepy", &Usage::default());
    let sleepy_group = catalog(Kind::Kaomoji).groups.iter().position(|group| group.title == "Sleepy").unwrap();
    assert!(hits.len() >= 10);
    assert!(
        hits.iter().take(10).all(|&hit| catalog(Kind::Kaomoji).entries[hit as usize].group as usize == sleepy_group)
    );
}

#[test]
fn code_points_find_symbols() {
    for query in ["U+20AC", "u+20ac", "20ac", "20AC"] {
        assert_eq!(top_text(Kind::Symbol, query), EURO, "{query}");
    }
}

#[test]
fn a_query_that_is_the_item_finds_it() {
    assert_eq!(top_text(Kind::Symbol, EURO), EURO);
    assert_eq!(top_text(Kind::Emoji, "\u{1F525}"), "\u{1F525}");
    assert_eq!(top_text(Kind::Emoji, "\u{2764}"), RED_HEART);
    assert_eq!(top_text(Kind::Emoji, "\u{1F44D}\u{1F3FD}"), THUMBS_UP);
    assert_eq!(top_text(Kind::Kaomoji, SHRUG), SHRUG);
}

#[test]
fn search_ignores_case_and_accents() {
    let pinata = index_of(Kind::Emoji, "\u{1FA85}");
    for query in ["pinata", "PIÑATA", "piñata"] {
        assert!(search(Kind::Emoji, query, &Usage::default()).contains(&pinata), "{query}");
    }
}

#[test]
fn every_term_must_match() {
    assert_eq!(top_text(Kind::Emoji, "red heart"), RED_HEART);
    assert_eq!(top_text(Kind::Emoji, "heart red"), RED_HEART);
    assert!(search(Kind::Emoji, "heart zzzzqq", &Usage::default()).is_empty());
    assert!(search(Kind::Emoji, "zzzzqq", &Usage::default()).is_empty());
}

#[test]
fn names_beat_keywords_and_prefixes_beat_substrings() {
    let usage = Usage::default();
    let hits = search(Kind::Emoji, "rocket", &usage);
    let entries = &catalog(Kind::Emoji).entries;
    assert_eq!(entries[hits[0] as usize].text, "\u{1F680}");
    let mid_word = search(Kind::Emoji, "ocket", &usage);
    assert!(mid_word.contains(&hits[0]));
}

#[test]
fn empty_query_lists_everything_in_catalog_order() {
    let hits = search(Kind::Kaomoji, "  ", &Usage::default());
    assert_eq!(hits.len(), catalog(Kind::Kaomoji).entries.len());
    assert!(hits.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn usage_boost_reorders_ties() {
    let thumbs_up = index_of(Kind::Emoji, THUMBS_UP);
    let thumbs_down = index_of(Kind::Emoji, THUMBS_DOWN);
    let mut usage = Usage::default();
    let before = search_at(Kind::Emoji, "thumbs", &usage, NOW_MS);
    assert_eq!(before[..2], [thumbs_up, thumbs_down]);

    usage.record(Kind::Emoji, THUMBS_DOWN, NOW_MS);
    let after = search_at(Kind::Emoji, "thumbs", &usage, NOW_MS);
    assert_eq!(after[..2], [thumbs_down, thumbs_up]);
}

#[test]
fn usage_boost_is_capped_and_decays() {
    let mut usage = Usage::default();
    for _ in 0..200 {
        usage.record(Kind::Emoji, THUMBS_UP, NOW_MS);
    }
    assert_eq!(usage.boost(Kind::Emoji, THUMBS_UP, NOW_MS), 20);
    assert_eq!(usage.boost(Kind::Emoji, THUMBS_DOWN, NOW_MS), 0);
    assert!(usage.boost(Kind::Emoji, THUMBS_UP, NOW_MS + 140 * DAY_MS) < 20);
    assert_eq!(usage.boost(Kind::Symbol, THUMBS_UP, NOW_MS), 0);
}

#[test]
fn frecency_halves_every_fourteen_days() {
    let mut usage = Usage::default();
    for _ in 0..4 {
        usage.record(Kind::Emoji, "a", NOW_MS);
    }
    usage.record(Kind::Emoji, "b", NOW_MS + 14 * DAY_MS);
    usage.record(Kind::Emoji, "b", NOW_MS + 14 * DAY_MS);
    usage.record(Kind::Emoji, "b", NOW_MS + 14 * DAY_MS);
    assert_eq!(usage.frequent(Kind::Emoji, 10), ["b", "a"].map(String::from));

    usage.record(Kind::Emoji, "a", NOW_MS + 14 * DAY_MS);
    usage.record(Kind::Emoji, "a", NOW_MS + 14 * DAY_MS);
    assert_eq!(usage.frequent(Kind::Emoji, 10), ["a", "b"].map(String::from));
}

#[test]
fn frequent_breaks_ties_with_the_newest_and_respects_limit_and_kind() {
    let mut usage = Usage::default();
    usage.record(Kind::Emoji, "old", NOW_MS);
    usage.record(Kind::Emoji, "new", NOW_MS + 1000);
    usage.record(Kind::Symbol, "sym", NOW_MS);
    assert_eq!(usage.frequent(Kind::Emoji, 10), ["new", "old"].map(String::from));
    assert_eq!(usage.frequent(Kind::Emoji, 1), ["new".to_string()]);
    assert_eq!(usage.frequent(Kind::Symbol, 10), ["sym".to_string()]);
    assert!(usage.frequent(Kind::Kaomoji, 10).is_empty());

    usage.clear(Kind::Emoji);
    assert!(usage.frequent(Kind::Emoji, 10).is_empty());
    assert_eq!(usage.frequent(Kind::Symbol, 10).len(), 1);
}

#[test]
fn usage_survives_json() {
    let mut usage = Usage::default();
    usage.record(Kind::Emoji, THUMBS_UP, NOW_MS);
    usage.record(Kind::Kaomoji, SHRUG, NOW_MS);
    let restored: Usage = serde_json::from_str(&serde_json::to_string(&usage).unwrap()).unwrap();
    assert_eq!(restored, usage);
    let from_empty: Usage = serde_json::from_str("{}").unwrap();
    assert_eq!(from_empty, Usage::default());
}

#[test]
fn every_popular_emoji_exists() {
    let texts: HashSet<&str> = catalog(Kind::Emoji).entries.iter().map(|entry| entry.text.as_str()).collect();
    let missing: Vec<&str> = popular::listed().filter(|emoji| !texts.contains(emoji)).collect();
    assert!(missing.is_empty(), "not fully-qualified emoji: {missing:?}");
}

#[test]
fn parsing_all_catalogs_is_fast() {
    let started = Instant::now();
    for kind in Kind::ALL {
        Catalog::parse_embedded(kind).unwrap();
    }
    let elapsed = started.elapsed();
    println!("parsed all catalogs in {elapsed:?}");
    if cfg!(not(debug_assertions)) {
        assert!(elapsed.as_millis() < 15, "{elapsed:?}");
    }
}

#[test]
fn searching_every_emoji_is_fast() {
    let usage = Usage::default();
    let queries = ["heart", "face", "red heart", "flag", "a", "smiling face with", "thumbs"];
    for query in queries {
        search(Kind::Emoji, query, &usage);
    }
    let started = Instant::now();
    let rounds = 20;
    for _ in 0..rounds {
        for query in queries {
            search(Kind::Emoji, query, &usage);
        }
    }
    let per_search = started.elapsed() / (rounds * queries.len() as u32);
    println!("emoji search: {per_search:?} per query");
    if cfg!(not(debug_assertions)) {
        assert!(per_search.as_micros() < 2000, "{per_search:?}");
    }
}

#[test]
fn the_fallback_checker_reports_characters_no_font_has() {
    let checker = FallbackChecker::new().unwrap();
    assert!(checker.undrawable_chars("abc \u{30C4}\u{25D5}").unwrap().is_empty());
    assert_eq!(checker.undrawable_chars("a\u{10FFFD}b").unwrap(), ['\u{10FFFD}']);
}

#[test]
fn kaomoji_draw_without_missing_glyphs() {
    let checker = FallbackChecker::new().unwrap();
    let offenders: Vec<String> = catalog(Kind::Kaomoji)
        .entries
        .iter()
        .filter_map(|entry| {
            let missing = checker.undrawable_chars(&entry.text).unwrap();
            (!missing.is_empty()).then(|| format!("{} missing {missing:?}", entry.text))
        })
        .collect();
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn symbols_draw_without_missing_glyphs() {
    let checker = FallbackChecker::new().unwrap();
    let offenders: Vec<String> = catalog(Kind::Symbol)
        .entries
        .iter()
        .filter(|entry| !checker.undrawable_chars(&entry.text).unwrap().is_empty())
        .map(|entry| format!("U+{:04X} {}", entry.text.chars().next().unwrap() as u32, entry.name))
        .collect();
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn coverage_hides_only_what_segoe_ui_emoji_cannot_draw() {
    let coverage = Coverage::compute().unwrap();
    let entries = &catalog(Kind::Emoji).entries;
    assert!(coverage.supported_count() > entries.len() * 3 / 4);
    let woman_technologist = "\u{1F469}\u{200D}\u{1F4BB}";
    let rainbow_flag = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
    let keycap_hash = "#\u{FE0F}\u{20E3}";
    for text in [THUMBS_UP, RED_HEART, "\u{1F600}", woman_technologist, rainbow_flag, keycap_hash] {
        assert!(coverage.supports(index_of(Kind::Emoji, text)), "{text} should be drawable");
    }
    assert!(!coverage.supports(entries.len() as u32));
}

#[test]
fn coverage_cache_round_trips_and_notices_stale_keys() {
    let dir = std::env::temp_dir().join(format!("tuck-emoji-test-{}", std::process::id()));
    let cache = dir.join("nested").join("coverage.json");
    let computed = Coverage::load_or_compute(&cache).unwrap();
    assert!(cache.exists());
    assert_eq!(Coverage::load_or_compute(&cache).unwrap(), computed);

    std::fs::write(&cache, r#"{"key":"stale","supported":[]}"#).unwrap();
    assert_eq!(Coverage::load_or_compute(&cache).unwrap(), computed);
    assert!(std::fs::read_to_string(&cache).unwrap().len() > 100);
    std::fs::remove_dir_all(&dir).unwrap();
}
