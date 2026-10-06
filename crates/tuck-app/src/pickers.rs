//! The Emoji, Kaomoji and Symbols tabs' content from tuck-emoji: full sections (frequently used first, then one
//! section per group, emoji the font cannot draw hidden), search results, and the reverse lookup that records a pick
//! under its base entry.

use std::collections::HashMap;
use std::sync::OnceLock;

use tuck_emoji::{Coverage, Entry, Kind, Usage, catalog, search};
use tuck_panel::{PickerItem, PickerSection, Tab};
use tuck_ui::Icon;

pub const FREQUENT_TITLE: &str = "Frequently used";

pub fn kind_of(tab: Tab) -> Option<Kind> {
    match tab {
        Tab::Clipboard => None,
        Tab::Emoji => Some(Kind::Emoji),
        Tab::Kaomoji => Some(Kind::Kaomoji),
        Tab::Symbols => Some(Kind::Symbol),
    }
}

pub fn tab_of(kind: Kind) -> Tab {
    match kind {
        Kind::Emoji => Tab::Emoji,
        Kind::Kaomoji => Tab::Kaomoji,
        Kind::Symbol => Tab::Symbols,
    }
}

/// Frequently used items per tab: two rows of the emoji and symbol grids, three rows of kaomoji.
fn frequent_limit(kind: Kind) -> usize {
    match kind {
        Kind::Emoji | Kind::Symbol => 18,
        Kind::Kaomoji => 6,
    }
}

pub fn group_icon(kind: Kind, key: &str) -> Icon {
    match (kind, key) {
        (Kind::Emoji, "smileys-emotion") => Icon::Smile,
        (Kind::Emoji, "people-body") => Icon::Hand,
        (Kind::Emoji, "animals-nature") => Icon::PawPrint,
        (Kind::Emoji, "food-drink") => Icon::Apple,
        (Kind::Emoji, "travel-places") => Icon::Car,
        (Kind::Emoji, "activities") => Icon::Volleyball,
        (Kind::Emoji, "objects") => Icon::Lightbulb,
        (Kind::Emoji, "symbols") => Icon::Heart,
        (Kind::Emoji, "flags") => Icon::Flag,
        (Kind::Emoji, _) => Icon::Smile,
        (Kind::Symbol, "punctuation") => Icon::Quote,
        (Kind::Symbol, "currency") => Icon::Euro,
        (Kind::Symbol, "arrows") => Icon::ArrowLeftRight,
        (Kind::Symbol, "math") => Icon::Sigma,
        (Kind::Symbol, "geometric") => Icon::Shapes,
        (Kind::Symbol, "letterlike-technical") => Icon::Type,
        (Kind::Symbol, "latin-accented") => Icon::Languages,
        (Kind::Symbol, "greek") => Icon::Omega,
        (Kind::Symbol, "box-blocks") => Icon::Grid,
        (Kind::Symbol, _) => Icon::Omega,
        (Kind::Kaomoji, "love") => Icon::Heart,
        (Kind::Kaomoji, "greeting") => Icon::Hand,
        (Kind::Kaomoji, "cute") => Icon::Sparkles,
        (Kind::Kaomoji, "animals") => Icon::PawPrint,
        (Kind::Kaomoji, "sleepy") => Icon::Moon,
        (Kind::Kaomoji, _) => Icon::Parentheses,
    }
}

/// Entry index of every text a catalog inserts, skin-tone variants included.
fn index_of(kind: Kind, text: &str) -> Option<u32> {
    static MAPS: OnceLock<[HashMap<&'static str, u32>; 3]> = OnceLock::new();
    let maps = MAPS.get_or_init(|| Kind::ALL.map(text_index));
    let slot = Kind::ALL.iter().position(|k| *k == kind)?;
    maps[slot].get(text).copied()
}

fn text_index(kind: Kind) -> HashMap<&'static str, u32> {
    let mut map = HashMap::new();
    for (index, entry) in catalog(kind).entries.iter().enumerate() {
        for text in std::iter::once(&entry.text).chain(&entry.toned) {
            map.entry(text.as_str()).or_insert(index as u32);
        }
    }
    map
}

/// The catalog text a pick is recorded under: the entry itself when a skin-tone variant was picked.
pub fn base_text(kind: Kind, text: &str) -> Option<&'static str> {
    index_of(kind, text).map(|index| catalog(kind).entries[index as usize].text.as_str())
}

fn visible(kind: Kind, coverage: &Coverage, index: u32) -> bool {
    kind != Kind::Emoji || coverage.supports(index)
}

fn item(entry: &Entry) -> PickerItem {
    let item = PickerItem::new(&entry.text, &entry.name);
    if entry.toned.len() + 1 == tuck_core::SkinTone::ALL.len() {
        item.with_variants(std::iter::once(&entry.text).chain(&entry.toned).cloned().collect())
    } else {
        item
    }
}

fn frequent_items(kind: Kind, coverage: &Coverage, usage: &Usage) -> Vec<PickerItem> {
    let entries = &catalog(kind).entries;
    let limit = frequent_limit(kind);
    usage
        .frequent(kind, limit * 2)
        .iter()
        .filter_map(|text| index_of(kind, text))
        .filter(|index| visible(kind, coverage, *index))
        .take(limit)
        .map(|index| item(&entries[index as usize]))
        .collect()
}

/// Every visible entry by group, after "Frequently used" when anything was used.
pub fn sections(kind: Kind, coverage: &Coverage, usage: &Usage) -> Vec<PickerSection> {
    let catalog = catalog(kind);
    let mut groups: Vec<Vec<PickerItem>> = vec![Vec::new(); catalog.groups.len()];
    for (index, entry) in catalog.entries.iter().enumerate() {
        if visible(kind, coverage, index as u32)
            && let Some(group) = groups.get_mut(entry.group as usize)
        {
            group.push(item(entry));
        }
    }
    let frequent = frequent_items(kind, coverage, usage);
    let frequent = (!frequent.is_empty()).then(|| PickerSection {
        title: FREQUENT_TITLE.to_string(),
        icon: Icon::Clock,
        items: frequent,
    });
    let grouped = catalog
        .groups
        .iter()
        .zip(groups)
        .filter(|(_, items)| !items.is_empty())
        .map(|(group, items)| PickerSection { title: group.title.clone(), icon: group_icon(kind, &group.key), items });
    frequent.into_iter().chain(grouped).collect()
}

/// One untitled section of matches, best first.
pub fn search_sections(kind: Kind, query: &str, coverage: &Coverage, usage: &Usage) -> Vec<PickerSection> {
    let entries = &catalog(kind).entries;
    let items = search(kind, query, usage)
        .into_iter()
        .filter(|index| visible(kind, coverage, *index))
        .map(|index| item(&entries[index as usize]))
        .collect();
    vec![PickerSection { title: String::new(), icon: Icon::Search, items }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(sections: &[PickerSection]) -> Vec<&str> {
        sections.iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn full_sections_follow_the_catalog_groups() {
        let usage = Usage::default();
        let emoji = sections(Kind::Emoji, &Coverage::all_supported(), &usage);
        assert_eq!(emoji.len(), catalog(Kind::Emoji).groups.len());
        assert_eq!(emoji[0].title, "Smileys & Emotion");
        assert_eq!(emoji[0].icon, Icon::Smile);
        let count: usize = emoji.iter().map(|s| s.items.len()).sum();
        assert_eq!(count, catalog(Kind::Emoji).entries.len());
        let symbols = sections(Kind::Symbol, &Coverage::all_supported(), &usage);
        assert!(titles(&symbols).contains(&"Currency"));
        assert!(symbols.iter().all(|s| s.icon != Icon::Smile));
    }

    #[test]
    fn toned_entries_carry_six_variants() {
        let sections = sections(Kind::Emoji, &Coverage::all_supported(), &Usage::default());
        let wave = sections.iter().flat_map(|s| &s.items).find(|i| i.text == "👋").expect("waving hand");
        assert!(wave.has_tones());
        assert_eq!(wave.variants[0], "👋");
        assert_eq!(wave.variants[3], "👋🏽");
    }

    #[test]
    fn frequently_used_comes_first_and_records_base_entries() {
        let mut usage = Usage::default();
        assert_eq!(base_text(Kind::Emoji, "👋🏽"), Some("👋"));
        assert_eq!(base_text(Kind::Emoji, "not an emoji"), None);
        for (n, text) in ["👋", "🔥", "🔥"].iter().enumerate() {
            usage.record(Kind::Emoji, base_text(Kind::Emoji, text).unwrap(), 1_000 + n as i64);
        }
        let sections = sections(Kind::Emoji, &Coverage::all_supported(), &usage);
        assert_eq!(sections[0].title, FREQUENT_TITLE);
        assert_eq!(sections[0].icon, Icon::Clock);
        let texts: Vec<&str> = sections[0].items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["🔥", "👋"]);
    }

    #[test]
    fn search_returns_one_untitled_section() {
        let results = search_sections(Kind::Symbol, "euro", &Coverage::all_supported(), &Usage::default());
        assert_eq!(results.len(), 1);
        assert!(results[0].title.is_empty());
        assert_eq!(results[0].items.first().map(|i| i.text.as_str()), Some("€"));
        let none = search_sections(Kind::Kaomoji, "zzzqqq", &Coverage::all_supported(), &Usage::default());
        assert!(none[0].items.is_empty());
    }

    #[test]
    fn tabs_and_kinds_map_both_ways() {
        for kind in Kind::ALL {
            assert_eq!(kind_of(tab_of(kind)), Some(kind));
        }
        assert_eq!(kind_of(Tab::Clipboard), None);
    }
}
