//! In-memory clipboard history. Owned by the UI thread; `tuck-store` persists every change as an `Op`.

use std::cmp::Reverse;
use std::collections::HashMap;

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::clip::{ClipContent, ClipId, ClipItem, SourceApp};

const FIRST_ID: ClipId = 1;

/// Only this much of a text is searchable, so indexing a huge copy never stalls the UI thread.
const SEARCH_TEXT_LIMIT: usize = 64 * 1024;

const WHOLE_WORD: u32 = 3;
const WORD_PREFIX: u32 = 2;
const SUBSTRING: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Newest `used_ms` first.
    items: Vec<ClipItem>,
    next_id: ClipId,
    /// Folded searchable strings per item id (see `searchable_strings`), kept in step with `items`.
    folded: HashMap<ClipId, Vec<String>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

/// A new copy either became a new item or refreshed an existing one with the same hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added(ClipId),
    Promoted(ClipId),
}

impl AddOutcome {
    pub fn id(self) -> ClipId {
        match self {
            Self::Added(id) | Self::Promoted(id) => id,
        }
    }
}

/// Everything a history change needs to reach disk, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Add(ClipItem),
    /// A repeated copy or a paste moved the item to the top; `content` replaces formats that changed (e.g. the
    /// same text copied again from a page now carries HTML).
    Touch { id: ClipId, used_ms: i64, content: Option<ClipContent>, source: Option<SourceApp> },
    Pin { id: ClipId, pinned: bool },
    Remove(ClipId),
}

/// `ClipItem::bytes` for `content`: text + html + rtf bytes, image width*height*4, or path bytes. Replaying a
/// `Touch` that carries new content recomputes `bytes` with this, so callers of `History::add` should use it too.
pub fn content_bytes(content: &ClipContent) -> u64 {
    match content {
        ClipContent::Text { text, html, rtf } => {
            [Some(text), html.as_ref(), rtf.as_ref()].into_iter().flatten().map(|part| part.len() as u64).sum()
        }
        ClipContent::Image { width, height, .. } => u64::from(*width) * u64::from(*height) * 4,
        ClipContent::Files { paths } => paths.iter().map(|path| path.to_string_lossy().len() as u64).sum(),
    }
}

impl History {
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: FIRST_ID, folded: HashMap::new() }
    }

    /// Rebuilds from stored items in any order; sorts by `used_ms` and continues ids after the largest one.
    pub fn from_items(mut items: Vec<ClipItem>) -> Self {
        items.sort_by(|a, b| b.used_ms.cmp(&a.used_ms).then(b.id.cmp(&a.id)));
        let next_id = items.iter().map(|item| item.id).max().map_or(FIRST_ID, |max| (max + 1).max(FIRST_ID));
        let folded = items.iter().map(|item| (item.id, searchable_strings(item))).collect();
        Self { items, next_id, folded }
    }

    /// Newest first.
    pub fn items(&self) -> &[ClipItem] {
        &self.items
    }

    pub fn get(&self, id: ClipId) -> Option<&ClipItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Reserves the id the next `add` would use (the app names image blobs with it before adding).
    pub fn peek_next_id(&self) -> ClipId {
        self.next_id
    }

    /// Adds a copy. An existing item with the same `hash` moves to the top instead (its content and source are
    /// replaced, `created_ms` and `pinned` kept). Returns the outcome and the ops to persist.
    ///
    /// A `None` source keeps the existing one, because `Op::Touch` cannot express "clear". An image copied again
    /// keeps the blob it already has: equal hashes mean equal pixels, and the blob name `content` carries is the
    /// id of a future item.
    pub fn add(
        &mut self,
        content: ClipContent,
        hash: u64,
        bytes: u64,
        source: Option<SourceApp>,
        now_ms: i64,
    ) -> (AddOutcome, Vec<Op>) {
        if let Some(index) = self.items.iter().position(|item| item.hash == hash) {
            let mut item = self.items.remove(index);
            item.content = refreshed_content(&item.content, content);
            item.bytes = bytes;
            item.used_ms = now_ms;
            if source.is_some() {
                item.source = source.clone();
            }
            let op = Op::Touch { id: item.id, used_ms: now_ms, content: Some(item.content.clone()), source };
            let id = item.id;
            self.index(&item);
            self.insert_by_recency(item);
            return (AddOutcome::Promoted(id), vec![op]);
        }
        let item = ClipItem {
            id: self.next_id,
            content,
            hash,
            created_ms: now_ms,
            used_ms: now_ms,
            pinned: false,
            source,
            bytes,
        };
        self.next_id += 1;
        let (id, op) = (item.id, Op::Add(item.clone()));
        self.index(&item);
        self.insert_by_recency(item);
        (AddOutcome::Added(id), vec![op])
    }

    /// A paste from history: move to the top.
    pub fn touch(&mut self, id: ClipId, now_ms: i64) -> Option<Op> {
        let mut item = self.items.remove(self.position(id)?);
        item.used_ms = now_ms;
        self.insert_by_recency(item);
        Some(Op::Touch { id, used_ms: now_ms, content: None, source: None })
    }

    pub fn set_pinned(&mut self, id: ClipId, pinned: bool) -> Option<Op> {
        let index = self.position(id)?;
        self.items[index].pinned = pinned;
        Some(Op::Pin { id, pinned })
    }

    pub fn remove(&mut self, id: ClipId) -> Option<(ClipItem, Op)> {
        let item = self.items.remove(self.position(id)?);
        self.folded.remove(&id);
        Some((item, Op::Remove(id)))
    }

    /// Removes every unpinned item.
    pub fn clear_unpinned(&mut self) -> (Vec<ClipItem>, Vec<Op>) {
        self.remove_where(|item| !item.pinned)
    }

    /// Drops the oldest unpinned items beyond `max_unpinned`.
    pub fn trim(&mut self, max_unpinned: usize) -> (Vec<ClipItem>, Vec<Op>) {
        let mut unpinned_seen = 0;
        self.remove_where(|item| {
            if item.pinned {
                return false;
            }
            unpinned_seen += 1;
            unpinned_seen > max_unpinned
        })
    }

    /// Ids matching `query`, best first. Case- and accent-insensitive; every whitespace-separated term must match
    /// the item's plain text, a file name, or the source app name. Ranking: whole-word match beats prefix beats
    /// substring; ties keep history order. Empty query returns every id in history order.
    pub fn search(&self, query: &str) -> Vec<ClipId> {
        let folded_query = fold(query);
        let terms: Vec<&str> = folded_query.split_whitespace().collect();
        if terms.is_empty() {
            return self.items.iter().map(|item| item.id).collect();
        }
        let mut hits: Vec<(u32, ClipId)> = self
            .items
            .iter()
            .filter_map(|item| item_score(self.folded.get(&item.id)?, &terms).map(|score| (score, item.id)))
            .collect();
        hits.sort_by_key(|&(score, _)| Reverse(score));
        hits.into_iter().map(|(_, id)| id).collect()
    }

    fn position(&self, id: ClipId) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    fn index(&mut self, item: &ClipItem) {
        self.folded.insert(item.id, searchable_strings(item));
    }

    /// Puts `item` ahead of every item that is not newer than it, so the latest action wins ties.
    fn insert_by_recency(&mut self, item: ClipItem) {
        let index = self.items.partition_point(|other| other.used_ms > item.used_ms);
        self.items.insert(index, item);
    }

    /// Visits items newest first, removes the ones `should_remove` accepts and returns them with their ops.
    fn remove_where(&mut self, mut should_remove: impl FnMut(&ClipItem) -> bool) -> (Vec<ClipItem>, Vec<Op>) {
        let (removed, kept): (Vec<ClipItem>, Vec<ClipItem>) =
            std::mem::take(&mut self.items).into_iter().partition(|item| should_remove(item));
        self.items = kept;
        for item in &removed {
            self.folded.remove(&item.id);
        }
        let ops = removed.iter().map(|item| Op::Remove(item.id)).collect();
        (removed, ops)
    }
}

fn refreshed_content(existing: &ClipContent, copied: ClipContent) -> ClipContent {
    match (existing, copied) {
        (ClipContent::Image { blob, .. }, ClipContent::Image { width, height, .. }) => {
            ClipContent::Image { blob: blob.clone(), width, height }
        }
        (_, copied) => copied,
    }
}

fn fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    text.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// Sum of the best rank each term reaches in any of the item's searchable strings; `None` if a term matches none.
fn item_score(haystacks: &[String], terms: &[&str]) -> Option<u32> {
    terms
        .iter()
        .map(|term| haystacks.iter().map(|haystack| match_rank(haystack, term)).max().filter(|&rank| rank > 0))
        .sum()
}

fn searchable_strings(item: &ClipItem) -> Vec<String> {
    let mut strings = Vec::new();
    match &item.content {
        ClipContent::Text { text, .. } => strings.push(fold(&text[..text.floor_char_boundary(SEARCH_TEXT_LIMIT)])),
        ClipContent::Files { paths } => strings.extend(paths.iter().map(|path| fold(&path.to_string_lossy()))),
        ClipContent::Image { .. } => {}
    }
    if let Some(source) = &item.source {
        strings.push(fold(&source.name));
    }
    strings
}

fn match_rank(haystack: &str, term: &str) -> u32 {
    let mut best = 0;
    for (start, found) in haystack.match_indices(term) {
        let starts_word = haystack[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let ends_word = haystack[start + found.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        let rank = match (starts_word, ends_word) {
            (true, true) => return WHOLE_WORD,
            (true, false) => WORD_PREFIX,
            _ => SUBSTRING,
        };
        best = best.max(rank);
    }
    best
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;
    use crate::clip::text_hash;

    fn text(s: &str) -> ClipContent {
        ClipContent::Text { text: s.to_owned(), html: None, rtf: None }
    }

    fn app(name: &str) -> SourceApp {
        SourceApp { exe: PathBuf::from(format!(r"C:\apps\{name}.exe")), name: name.to_owned() }
    }

    fn add_text(history: &mut History, s: &str, now_ms: i64) -> (AddOutcome, Vec<Op>) {
        history.add(text(s), text_hash(s), s.len() as u64, None, now_ms)
    }

    fn add_text_from(history: &mut History, s: &str, source: &str, now_ms: i64) -> ClipId {
        history.add(text(s), text_hash(s), s.len() as u64, Some(app(source)), now_ms).0.id()
    }

    fn add_files(history: &mut History, paths: &[&str], now_ms: i64) -> ClipId {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let hash = crate::clip::files_hash(&paths);
        history.add(ClipContent::Files { paths }, hash, 0, None, now_ms).0.id()
    }

    fn add_image(history: &mut History, blob: &str, hash: u64, now_ms: i64) -> (AddOutcome, Vec<Op>) {
        history.add(ClipContent::Image { blob: blob.to_owned(), width: 4, height: 3 }, hash, 48, None, now_ms)
    }

    fn ids(history: &History) -> Vec<ClipId> {
        history.items().iter().map(|item| item.id).collect()
    }

    #[test]
    fn new_history_is_empty_and_starts_at_id_one() {
        let history = History::new();
        assert!(history.is_empty());
        assert_eq!(history.len(), 0);
        assert_eq!(history.peek_next_id(), 1);
        assert_eq!(History::default(), history);
    }

    #[test]
    fn add_creates_newest_first_items_with_sequential_ids() {
        let mut history = History::new();
        assert_eq!(history.peek_next_id(), 1);
        let (first, first_ops) = add_text(&mut history, "one", 100);
        let (second, second_ops) = add_text(&mut history, "two", 200);
        assert_eq!((first, second), (AddOutcome::Added(1), AddOutcome::Added(2)));
        assert_eq!(ids(&history), [2, 1]);
        assert_eq!(history.peek_next_id(), 3);
        assert_eq!(first_ops, vec![Op::Add(history.get(1).unwrap().clone())]);
        assert_eq!(second_ops, vec![Op::Add(history.get(2).unwrap().clone())]);
        let item = history.get(1).unwrap();
        assert_eq!((item.created_ms, item.used_ms, item.pinned, item.bytes), (100, 100, false, 3));
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn add_with_equal_hash_promotes_and_keeps_created_and_pinned() {
        let mut history = History::new();
        add_text(&mut history, "keep", 100);
        add_text(&mut history, "other", 200);
        history.set_pinned(1, true).unwrap();
        let html = ClipContent::Text { text: "keep".into(), html: Some("<b>keep</b>".into()), rtf: None };
        let (outcome, ops) = history.add(html.clone(), text_hash("keep"), 15, Some(app("Edge")), 300);
        assert_eq!(outcome, AddOutcome::Promoted(1));
        assert_eq!(ids(&history), [1, 2]);
        let item = history.get(1).unwrap();
        assert_eq!((item.created_ms, item.used_ms, item.pinned, item.bytes), (100, 300, true, 15));
        assert_eq!(item.content, html);
        assert_eq!(item.source, Some(app("Edge")));
        assert_eq!(history.len(), 2);
        assert_eq!(history.peek_next_id(), 3);
        assert_eq!(ops, vec![Op::Touch { id: 1, used_ms: 300, content: Some(html), source: Some(app("Edge")) }]);
    }

    #[test]
    fn add_with_equal_hash_and_no_source_keeps_the_known_source() {
        let mut history = History::new();
        add_text_from(&mut history, "same", "Code", 100);
        let (_, ops) = add_text(&mut history, "same", 200);
        assert_eq!(history.get(1).unwrap().source, Some(app("Code")));
        assert_eq!(ops, vec![Op::Touch { id: 1, used_ms: 200, content: Some(text("same")), source: None }]);
    }

    #[test]
    fn add_with_equal_image_hash_keeps_the_existing_blob() {
        let mut history = History::new();
        add_image(&mut history, "0000000000000001", 77, 100);
        let (outcome, ops) = add_image(&mut history, "0000000000000002", 77, 200);
        assert_eq!(outcome, AddOutcome::Promoted(1));
        let expected = ClipContent::Image { blob: "0000000000000001".into(), width: 4, height: 3 };
        assert_eq!(history.get(1).unwrap().content, expected);
        assert_eq!(ops, vec![Op::Touch { id: 1, used_ms: 200, content: Some(expected), source: None }]);
    }

    #[test]
    fn touch_moves_to_the_top_and_reports_an_op_without_content() {
        let mut history = History::new();
        for (s, t) in [("a", 10), ("b", 20), ("c", 30)] {
            add_text(&mut history, s, t);
        }
        assert_eq!(history.touch(1, 40), Some(Op::Touch { id: 1, used_ms: 40, content: None, source: None }));
        assert_eq!(ids(&history), [1, 3, 2]);
        assert_eq!(history.get(1).unwrap().used_ms, 40);
        assert_eq!(history.get(1).unwrap().created_ms, 10);
        assert_eq!(history.touch(99, 50), None);
        assert_eq!(ids(&history), [1, 3, 2]);
    }

    #[test]
    fn touch_in_the_same_millisecond_still_goes_first() {
        let mut history = History::new();
        add_text(&mut history, "a", 10);
        add_text(&mut history, "b", 10);
        assert_eq!(ids(&history), [2, 1]);
        history.touch(1, 10).unwrap();
        assert_eq!(ids(&history), [1, 2]);
    }

    #[test]
    fn touch_with_an_older_clock_sorts_by_time() {
        let mut history = History::new();
        for (s, t) in [("a", 10), ("b", 20), ("c", 30)] {
            add_text(&mut history, s, t);
        }
        history.touch(3, 15).unwrap();
        assert_eq!(ids(&history), [2, 3, 1]);
    }

    #[test]
    fn set_pinned_reports_an_op_and_keeps_order() {
        let mut history = History::new();
        add_text(&mut history, "a", 10);
        add_text(&mut history, "b", 20);
        assert_eq!(history.set_pinned(1, true), Some(Op::Pin { id: 1, pinned: true }));
        assert!(history.get(1).unwrap().pinned);
        assert_eq!(ids(&history), [2, 1]);
        assert_eq!(history.set_pinned(1, false), Some(Op::Pin { id: 1, pinned: false }));
        assert!(!history.get(1).unwrap().pinned);
        assert_eq!(history.set_pinned(99, true), None);
    }

    #[test]
    fn remove_returns_the_item_and_its_op() {
        let mut history = History::new();
        add_text(&mut history, "a", 10);
        add_text(&mut history, "b", 20);
        let (item, op) = history.remove(1).unwrap();
        assert_eq!(item.id, 1);
        assert_eq!(op, Op::Remove(1));
        assert_eq!(ids(&history), [2]);
        assert!(history.get(1).is_none());
        assert!(history.remove(1).is_none());
        assert_eq!(history.peek_next_id(), 3);
    }

    #[test]
    fn clear_unpinned_keeps_pinned_items() {
        let mut history = History::new();
        for (s, t) in [("a", 10), ("b", 20), ("c", 30), ("d", 40)] {
            add_text(&mut history, s, t);
        }
        history.set_pinned(2, true).unwrap();
        history.set_pinned(4, true).unwrap();
        let (removed, ops) = history.clear_unpinned();
        assert_eq!(removed.iter().map(|item| item.id).collect::<Vec<_>>(), [3, 1]);
        assert_eq!(ops, vec![Op::Remove(3), Op::Remove(1)]);
        assert_eq!(ids(&history), [4, 2]);
        let (removed, ops) = history.clear_unpinned();
        assert!(removed.is_empty() && ops.is_empty());
    }

    #[test]
    fn trim_drops_the_oldest_unpinned_beyond_the_limit() {
        let mut history = History::new();
        for (s, t) in [("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)] {
            add_text(&mut history, s, t);
        }
        history.set_pinned(1, true).unwrap();
        let (removed, ops) = history.trim(2);
        assert_eq!(removed.iter().map(|item| item.id).collect::<Vec<_>>(), [3, 2]);
        assert_eq!(ops, vec![Op::Remove(3), Op::Remove(2)]);
        assert_eq!(ids(&history), [5, 4, 1]);
        let (removed, ops) = history.trim(2);
        assert!(removed.is_empty() && ops.is_empty());
        let (removed, _) = history.trim(0);
        assert_eq!(removed.len(), 2);
        assert_eq!(ids(&history), [1]);
    }

    #[test]
    fn from_items_sorts_and_continues_ids() {
        let source = {
            let mut history = History::new();
            for (s, t) in [("a", 10), ("b", 30), ("c", 20)] {
                add_text(&mut history, s, t);
            }
            history
        };
        let mut shuffled: Vec<ClipItem> = source.items().to_vec();
        shuffled.reverse();
        shuffled.swap(0, 1);
        let rebuilt = History::from_items(shuffled);
        assert_eq!(rebuilt, source);
        assert_eq!(rebuilt.peek_next_id(), 4);
        assert_eq!(History::from_items(Vec::new()), History::new());
    }

    #[test]
    fn from_items_breaks_ties_by_newest_id() {
        let mut history = History::new();
        add_text(&mut history, "a", 10);
        add_text(&mut history, "b", 10);
        let rebuilt = History::from_items(history.items().iter().rev().cloned().collect());
        assert_eq!(ids(&rebuilt), [2, 1]);
    }

    #[test]
    fn content_bytes_counts_every_format() {
        let rich = ClipContent::Text { text: "ab".into(), html: Some("<b>".into()), rtf: Some("{r}".into()) };
        assert_eq!(content_bytes(&rich), 8);
        assert_eq!(content_bytes(&text("ab")), 2);
        assert_eq!(content_bytes(&ClipContent::Image { blob: "x".into(), width: 5, height: 2 }), 40);
        let files = ClipContent::Files { paths: vec![PathBuf::from("a.txt"), PathBuf::from("bc")] };
        assert_eq!(content_bytes(&files), 7);
    }

    #[test]
    fn empty_query_returns_every_id_in_history_order() {
        let mut history = History::new();
        for (s, t) in [("a", 10), ("b", 30), ("c", 20)] {
            add_text(&mut history, s, t);
        }
        assert_eq!(history.search(""), [2, 3, 1]);
        assert_eq!(history.search("   \t "), [2, 3, 1]);
        assert!(History::new().search("x").is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let mut history = History::new();
        add_text(&mut history, "Hello World", 10);
        add_text(&mut history, "goodbye", 20);
        assert_eq!(history.search("HELLO"), [1]);
        assert_eq!(history.search("world"), [1]);
        assert!(history.search("mars").is_empty());
    }

    #[test]
    fn search_folds_accents_both_ways() {
        let mut history = History::new();
        add_text(&mut history, "Crème brûlée at the café", 10);
        add_text(&mut history, "plain cafe", 20);
        add_text(&mut history, "Ångström ñandú", 30);
        assert_eq!(history.search("creme brulee"), [1]);
        assert_eq!(history.search("CAFÉ"), [2, 1]);
        assert_eq!(history.search("cafe"), [2, 1]);
        assert_eq!(history.search("cafe\u{301}"), [2, 1]);
        assert_eq!(history.search("angstrom nandu"), [3]);
        assert_eq!(history.search("brûlée"), [1]);
    }

    #[test]
    fn search_requires_every_term() {
        let mut history = History::new();
        add_text(&mut history, "red apple pie", 10);
        add_text(&mut history, "green apple", 20);
        assert_eq!(history.search("apple red"), [1]);
        assert_eq!(history.search("apple"), [2, 1]);
        assert!(history.search("apple blue").is_empty());
    }

    #[test]
    fn search_ranks_whole_word_over_prefix_over_substring() {
        let mut history = History::new();
        add_text(&mut history, "scatter plot", 10);
        add_text(&mut history, "catalog", 20);
        add_text(&mut history, "the cat sat", 30);
        add_text(&mut history, "bobcat", 40);
        assert_eq!(history.search("cat"), [3, 2, 4, 1]);
    }

    #[test]
    fn search_treats_punctuation_as_word_boundaries() {
        let mut history = History::new();
        add_text(&mut history, "xcat", 10);
        add_text(&mut history, "pet:cat.", 20);
        assert_eq!(history.search("cat"), [2, 1]);
    }

    #[test]
    fn search_ties_keep_history_order() {
        let mut history = History::new();
        for (s, t) in [("cat one", 10), ("cat two", 30), ("cat three", 20)] {
            add_text(&mut history, s, t);
        }
        assert_eq!(history.search("cat"), [2, 3, 1]);
    }

    #[test]
    fn search_sums_term_ranks_across_the_item() {
        let mut history = History::new();
        add_text(&mut history, "catalog dogma", 10);
        add_text(&mut history, "cat dog", 20);
        add_text(&mut history, "cat dogma", 30);
        assert_eq!(history.search("cat dog"), [2, 3, 1]);
    }

    #[test]
    fn search_matches_the_source_app_name() {
        let mut history = History::new();
        add_text_from(&mut history, "nothing relevant", "Visual Studio Code", 10);
        add_text_from(&mut history, "code review", "Slack", 20);
        add_text(&mut history, "unrelated", 30);
        assert_eq!(history.search("code"), [2, 1]);
        assert_eq!(history.search("studio relevant"), [1]);
        assert_eq!(history.search("SLACK review"), [2]);
    }

    #[test]
    fn search_matches_file_names_and_paths() {
        let mut history = History::new();
        add_files(&mut history, &[r"C:\Users\me\Reports\Budget Überblick.xlsx", r"C:\Users\me\notes.txt"], 10);
        add_files(&mut history, &[r"D:\photos\holiday.png"], 20);
        assert_eq!(history.search("budget"), [1]);
        assert_eq!(history.search("uberblick xlsx"), [1]);
        assert_eq!(history.search("holiday"), [2]);
        assert_eq!(history.search("notes"), [1]);
        assert!(history.search("missing").is_empty());
    }

    #[test]
    fn search_finds_images_only_through_their_source() {
        let mut history = History::new();
        add_image(&mut history, "0000000000000001", 1, 10);
        history.add(ClipContent::Image { blob: "b".into(), width: 1, height: 1 }, 2, 4, Some(app("Snip")), 20);
        assert_eq!(history.search("snip"), [2]);
        assert!(history.search("image").is_empty());
    }

    #[test]
    fn search_is_fast_over_a_thousand_items() {
        const VOCABULARY: &str =
            "lorem ipsum dolor sit amet elit café résumé naïve über Zażółć clipboard history emoji";
        let words: Vec<&str> = VOCABULARY.split(' ').collect();
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut history = History::new();
        for n in 0..1000 {
            let mut body = String::new();
            while body.len() < 200 {
                body.push_str(words[(next() % words.len() as u64) as usize]);
                body.push(' ');
            }
            body.push_str(&format!("item{n}"));
            add_text_from(&mut history, &body, if n % 2 == 0 { "Notepad" } else { "Chrome" }, n);
        }
        let queries = ["item999", "lorem ipsum", "cafe resume", "zzzz", "dolor sit amet"];
        let mut best = std::time::Duration::MAX;
        for _ in 0..10 {
            let started = Instant::now();
            for query in queries {
                std::hint::black_box(history.search(query));
            }
            best = best.min(started.elapsed() / queries.len() as u32);
        }
        assert_eq!(history.search("item999"), [1000]);
        assert!(cfg!(debug_assertions) || best < std::time::Duration::from_millis(3), "search took {best:?}");
    }
}
