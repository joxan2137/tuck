# tuck-emoji API

Emoji, kaomoji and symbol catalogs, search, usage ranking and font coverage (DESIGN §6). Everything is synchronous and
`Send + Sync` except `Coverage::compute`, which creates its own DirectWrite factory (call it off the UI thread).

```rust
use tuck_emoji::{catalog, search, Catalog, Coverage, Entry, Group, Kind, Usage};
```

## Catalogs

```rust
pub enum Kind { Emoji, Kaomoji, Symbol }          // Copy, Eq, Hash; Kind::ALL = [Emoji, Kaomoji, Symbol]
pub struct Group { pub key: String, pub title: String }   // key = slug of the title, e.g. "smileys-emotion"
pub struct Entry {
    pub text: String,          // what gets inserted (emoji fully-qualified, with U+FE0F where required)
    pub name: String,          // emoji: CLDR name; symbol: lowercase Unicode name; kaomoji: its first keyword
    pub keywords: Vec<String>, // emoji: CLDR keywords; kaomoji: curated words; symbols: empty
    pub group: u16,            // index into Catalog::groups
    pub toned: Vec<String>,    // empty, or the 5 uniform skin-tone variants light -> dark
    pub version: f32,          // emoji version (0.6 .. 18.0); 0.0 for symbols and kaomoji
}
pub struct Catalog { pub kind: Kind, pub groups: Vec<Group>, pub entries: Vec<Entry> /* + private search index */ }
pub fn catalog(kind: Kind) -> &'static Catalog;   // parsed once on first use (OnceLock)
impl Entry { pub fn with_tone(&self, tone: tuck_core::SkinTone) -> &str; }
```

- Entry order = Unicode order for emoji (groups in emoji-test.txt order, "Component" dropped, skin-tone variants are
  never rows of their own), code point order per group for symbols, file order for kaomoji.
- Entry indices (`u32`) are stable for a build and are what `search` returns and `Coverage::supports` takes.
- `with_tone`: `Default`, or an entry without `toned`, returns `text`; otherwise the matching variant. Multi-person
  sequences (handshake, couples) use the same tone for every person.
- Sizes today: 1923 emoji in 9 groups, 1481 symbols in 9 groups, 299 kaomoji in 15 groups. Parsing all three takes
  3-6 ms in release (asserted < 15 ms).

## Search

```rust
pub fn search(kind: Kind, query: &str, usage: &Usage) -> Vec<u32>;   // entry indices, best first
```

- Empty or blank query: every index in catalog order. No match: empty vec.
- The query is NFD-folded (accents dropped, lowercase) and split into terms on anything that is not a letter or digit
  (`heart-eyes` = `heart eyes`). Every term must match; names and keywords are tokenised the same way.
- Per term: name word exact 85, name word prefix 70, keyword exact 65, keyword prefix 55, name substring 30. An entry
  scores the average of its terms; when the terms spell the whole name it scores 100.
- Kaomoji have no names: `name` is the first keyword, and the words of the group title count as keywords, so
  "sleepy" lists the Sleepy group and "shrug" puts the shrug first.
- A query that is the entry itself (ignoring U+FE0F) or one of its `toned` variants scores 1000. For `Kind::Symbol`,
  `U+20AC` / `u+20ac` (2-6 hex digits) and bare `20ac` (4-6 hex digits) also score 1000 for that code point.
- Usage boost 0..=20 is added to the score (`8 * ln(1 + frecency)`, decayed to now), so a used item can pass
  better-scoring ones.
- Order among equal scores: fewer name words, then position in a curated popularity list (emoji only, so "heart"
  finds the red heart before the 40 other hearts), then catalog order.
- Typical cost: about 0.4 ms per query over all emoji in release (asserted < 2 ms); the index is built at parse time.

## Usage (frecency)

```rust
#[derive(Serialize, Deserialize, Default)] pub struct Usage { .. }
impl Usage {
    pub fn record(&mut self, kind: Kind, text: &str, now_ms: i64);        // one pick; unix ms
    pub fn frequent(&self, kind: Kind, limit: usize) -> Vec<String>;      // texts, most frecent first
    pub fn clear(&mut self, kind: Kind);
}
```

- Score: each pick adds 1; the total halves every 14 days. `frequent` ranks by that decayed score, newest first on
  ties. Items that decayed below 0.02 are forgotten on the next `record`.
- Serde JSON (`{"emoji":{"<text>":{"score":..,"last_ms":..}},"kaomoji":{..},"symbols":{..}}`); missing fields default,
  so an older or empty `{}` file loads. The app owns the file (`%LOCALAPPDATA%\Tuck\usage.json`) and debounces saves.
- `frequent(Kind::Emoji, n)` returns base texts; hide unsupported ones with `Coverage` after looking the text up.

## Coverage

```rust
pub struct Coverage { .. }                 // Clone, Eq; one flag per emoji entry
impl Coverage {
    pub fn compute() -> anyhow::Result<Coverage>;                  // ~10 ms, shapes every emoji
    pub fn load_or_compute(cache: &Path) -> anyhow::Result<Coverage>;
    pub fn supports(&self, index: u32) -> bool;                    // emoji entry index; out of range = false
    pub fn supported_count(&self) -> usize;
    pub fn unsupported_indices(&self) -> impl Iterator<Item = u32> + '_;
    pub fn all_supported() -> Coverage;                            // fallback when DirectWrite fails: hides nothing
}
```

- Supported = `IDWriteTextAnalyzer::GetGlyphs` with the system "Segoe UI Emoji" face yields exactly one non-zero glyph
  for the entry's whole text (script runs from `AnalyzeScript`).
- `load_or_compute(cache)`: `cache` is the JSON file path (e.g. `%LOCALAPPDATA%\Tuck\coverage.json`); parent
  directories are created. The key is a hash of the embedded emoji data + the font file's size + last-write time, so a
  Windows font update or new data recomputes. A cache write failure is logged, not returned. It still creates the
  DirectWrite factory and font face to read the key (a few ms).
- On a stock Windows 11 with Segoe UI Emoji 1.70: 1616 of 1923 supported. Hidden: all country flags (Windows draws
  regional indicators as letters), ZWJ families, couples and kisses without a font ligature, "people holding hands",
  and the nine Emoji 18.0 additions.
- Coverage only covers emoji. Every symbol and kaomoji in the data was checked against DirectWrite's system font
  fallback from "Segoe UI" (no missing glyph); the unit tests repeat that check.

## Data

`data/{emoji,symbols,kaomoji}.tsv` + `LICENSE-UNICODE.txt` are embedded with `include_str!`.
`cargo run -p gen-data [-- --fetch]` regenerates emoji.tsv and symbols.tsv from `tools/gen-data/cache`;
`kaomoji.tsv` (`group<TAB>kaomoji<TAB>keywords|...`) is hand-edited: unique kaomoji, plain English keywords, the first
keyword is the display name.

## Not in DESIGN §6 (additions)

`Kind::ALL`, `Group.key`, `Coverage::{all_supported, supported_count, unsupported_indices}`, the popularity tie-break,
and the `U+`/hex lookup rules above. `cargo run -p tuck-emoji --example coverage` prints supported/unsupported counts,
the time taken and the newest unsupported emoji (no window).
