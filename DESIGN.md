# Tuck — design contract

Tuck is a resident Windows 11 app written in Rust that replaces the Win+V clipboard history popup and the Win+. emoji
panel with one fast, reliable popup: the **panel**. It has four tabs: Clipboard, Emoji, Kaomoji, Symbols. The UI must
feel Apple-made: calm, precise, instant, animated with springs, crisp at every DPI. It is a sibling of Glint
(`../glint`) and reuses Glint's render kit and keyboard-hook design.

This file is the contract. Workers implement against it; disputes are settled here. Only the main thread edits this
file, the workspace `Cargo.toml`, and `crates/tuck-core/src/{geom,image,settings,clip,lib}.rs`. If you need a change
there (or a new dependency), say so in your report instead of editing.

## 1. Workspace

```
tuck/
  Cargo.toml            workspace; all shared deps pinned in [workspace.dependencies]
  DESIGN.md
  crates/
    tuck-core/          pure Rust: geometry, Image, Settings, ClipItem + classify + hashes, History, thumbnail
    tuck-ui/            Glint's Direct2D/DirectWrite/DirectComposition render kit (renamed), plus the widgets §7 needs
    tuck-sys/          input hook + key routing, key→text translation, text/paste injection, target window + caret
                        lookup, app names/icons, foreground watch; adapted from Glint: tray, single instance + IPC,
                        install, logging, paths, settings file, shell helpers, OCR
    tuck-clip/          the clipboard thread: watch copies, read formats, write items back, import Windows history
    tuck-store/         persistent history: DPAPI-protected append-only log + image blobs
    tuck-emoji/         generated emoji/symbol data + curated kaomoji, catalog, search, usage ranking, font coverage
    tuck-panel/         the panel view (all four tabs) and its data types; no Windows calls beyond tuck-ui
    tuck-app/           bin `tuck`: wiring, settings window, tray, toast, install, CLI, selftest, previews, icon art
  tools/
    gen-data/           offline generator for crates/tuck-emoji/data (§6)
```

Dependency direction: core ← ui, sys, clip, store, emoji ← panel (core + ui only) ← app. No cycles. `tuck-ui` and
`tuck-panel` never depend on sys/clip/store/emoji; the app converts between them.

Toolchain: stable MSVC (`x86_64-pc-windows-msvc`), edition 2024, `windows` 0.62 pinned once in the workspace with the
union of features. No C/C++ build steps.

Process: Per-Monitor-V2 DPI aware (manifest embedded by tuck-app's build.rs; examples/tests call
`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` first). `#![windows_subsystem =
"windows"]`. Log to `%LOCALAPPDATA%\Tuck\tuck.log` (keep last 1 MB). **Never log clipboard contents, typed search text
or inserted text** — only kinds, sizes, timings and app names.

Data locations: settings `%APPDATA%\Tuck\settings.json`; history, blobs, usage, coverage cache and log under
`%LOCALAPPDATA%\Tuck\`. Install dir `%LOCALAPPDATA%\Programs\Tuck\tuck.exe`.

## 2. Rules for workers

- Never launch `tuck.exe` (or any example) in a way that shows a window, installs a hook, injects input, touches the
  clipboard, writes the registry, or installs anything. Every crate has a headless verification path (§11); use it.
- Never use computer-use tools. Visual checks go through offscreen PNG renders.
- Never kill processes by name. Stop only PIDs you started.
- Build with your own target dir: `$env:CARGO_TARGET_DIR="target/<crate-name>"`. Quote the repo path in shell commands.
- `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo test -p <crate>` must pass for your crate.
- Code style: self-documenting names, few comments, no commented-out code, `anyhow::Result` at boundaries,
  `thiserror` only where callers match on errors. Unsafe Win32 calls wrapped in small safe functions. Match the
  style of the code copied from Glint.
- Keep the public API in this file's signatures (additions are fine; say so). Write `crates/<crate>/API.md`: the
  public API in ≤150 lines, for the workers who build on it.
- Stay inside your crate(s). Your report lists every file you changed and anything you need from the main thread.

## 3. Shortcuts and input routing (tuck-sys::hook)

`RegisterHotKey` cannot take Win+V or Win+. (the shell owns them). A dedicated hook thread (priority TIME_CRITICAL,
own message loop) runs `WH_KEYBOARD_LL`, and `WH_MOUSE_LL` only while the panel captures input. The hook procs only
classify, swallow (return 1) and send to a channel; a dispatcher thread (priority HIGHEST) calls the sink. They never
block (Windows silently drops hooks that exceed `LowLevelHooksTimeout`). Track modifier state from the hook's own
events; clear stale modifiers with `GetAsyncKeyState` exactly as Glint's `release_stale_modifiers` does. Always
`CallNextHookEx` for events we pass. Re-install both hooks every 10 minutes and on session unlock. Ignore events whose
`dwExtraInfo` is our marker `0x5455434B` ("TUCK").

Hotkeys (always active, when enabled in settings):
- Win+V → `Hotkey::Clipboard`. Win+. (VK_OEM_PERIOD) and Win+; (VK_OEM_1) → `Hotkey::Emoji`. Only with Win alone
  (Shift/Ctrl/Alt up). Swallow key-down, auto-repeat and key-up of the trigger key.
- When a swallowed combo leaves Win logically down, inject the mask key (VK 0xE8 down/up, tagged with the marker) so
  the Start menu does not open on Win release (Glint's mechanism).

Capture mode (between `begin_capture` and `end_capture`; the panel never takes focus, so keys must be routed):
- Modifier keys (L/R Shift, Ctrl, Alt, Win), Caps/Num/Scroll Lock: pass through, track state.
- Media, volume and browser keys: pass through, no dismiss.
- `opening_win`: if Win was down when capture began, keys pressed while that same Win stays down are treated as if
  Win were up. Cleared on Win release.
- AltGr (RAlt, or LCtrl+RAlt as layouts with AltGr report it) counts as text input, not as Ctrl/Alt.
- Key-down with Ctrl, Alt or Win effectively held: if the combo is one the panel handles (Ctrl+Backspace, Ctrl+A,
  Ctrl+1…Ctrl+9, Ctrl+P, Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+Enter) swallow and route; otherwise pass it through and send
  `Dismiss(Shortcut)` (Alt+Tab, Win+D, Ctrl+C etc. keep working and close the panel).
- Any other key-down (plain, Shift, AltGr, F-keys excluded: F-keys pass + dismiss): swallow and route as
  `HookEvent::Key`. Key-ups follow their key-down (swallowed down → swallowed up; passed down → passed up; a key held
  before capture began passes its up).
- Mouse: a button-down (left/right/middle/X) outside the panel rect passes through and sends `Dismiss(ClickOutside)`.
- Failsafe: the UI thread calls `heartbeat()` at least every 250 ms while capturing. If the last heartbeat is older
  than 1000 ms when a key arrives, the hook ends capture, passes the key and sends `Dismiss(Stalled)`. A hung panel
  must never eat the keyboard.

```rust
pub const MARKER: usize = 0x5455_434B;
pub enum Hotkey { Clipboard, Emoji }
pub struct HookConfig { pub win_v: bool, pub win_period: bool }          // From<&ShortcutSettings>
pub struct Mods { pub shift: bool, pub ctrl: bool, pub alt: bool, pub win: bool, pub altgr: bool }
pub struct RoutedKey { pub vk: u16, pub scan: u16, pub extended: bool, pub down: bool, pub repeat: bool,
                       pub mods: Mods, pub caps_lock: bool }
pub enum DismissReason { ClickOutside, Shortcut, Stalled }
pub enum HookEvent { Hotkey(Hotkey), Key(RoutedKey), Dismiss(DismissReason) }
pub struct InputHook { .. }
impl InputHook {
    pub fn start(config: HookConfig, sink: impl Fn(HookEvent) + Send + Sync + 'static) -> Result<Self>;
    pub fn set_config(&self, config: HookConfig);
    pub fn begin_capture(&self, panel_rect_px: RectI);
    pub fn set_panel_rect(&self, panel_rect_px: RectI);
    pub fn end_capture(&self);
    pub fn is_capturing(&self) -> bool;
    pub fn heartbeat(&self);
    pub fn on_session_unlock(&self);
}
```

The routing decision is a pure state machine (`HookState::on_key(..) -> Decision`, like Glint's) with unit tests for
every rule above, including AltGr, `opening_win`, key-up pairing and the stall failsafe.

## 4. Text, targets and system pieces (tuck-sys)

**keys** — routed keys become text on the UI thread with `ToUnicodeEx(vk, scan, state, buf, 0x4, hkl)` (flag 0x4:
do not change keyboard state), using the target thread's layout (`GetKeyboardLayout(target_thread)`) and a state
array synthesized from `RoutedKey.mods` + `caps_lock` (AltGr = Ctrl+Alt+RMenu). Dead keys: remember the dead char;
the next key's char is combined with `NormalizeString(NormalizationC, base + combining mark)`; dead + space or an
uncombinable key yields the dead char (then the key's char). Control characters are not text.

```rust
pub enum KeyText { None, Text(String), Dead }
pub struct TextTranslator { .. }   // holds the pending dead key
impl TextTranslator { pub fn new() -> Self; pub fn translate(&mut self, key: &RoutedKey, hkl: isize) -> KeyText;
                      pub fn reset(&mut self); }
```

**inject** — everything tagged with `MARKER`.
- `type_text(text)`: wait up to 300 ms for Shift/Ctrl/Alt/Win to be physically released (`GetAsyncKeyState`, 5 ms
  polls). Modifiers still down after that get injected key-ups, only for the ones actually down; before releasing
  Win or Alt inject the mask key (VK 0xE8 down/up) so neither the Start menu nor a menu bar (Alt) activates. Never
  re-press them. Then one `SendInput` batch of `KEYEVENTF_UNICODE` down/up pairs per UTF-16 unit (surrogate pairs as
  two units, `wVk` 0); `\n` becomes VK_RETURN. `SendInput` cannot reach a higher-integrity window and reports
  nothing (UIPI); `Target` records whether the target is elevated so the app can say so instead of failing silently.
- `paste(key: PasteKey)`: same modifier handling, then Ctrl+V / Shift+Insert / Ctrl+Shift+V as one batch.

**target** — `capture_target() -> Option<Target>` in < 1 ms: foreground hwnd, `GetGUIThreadInfo` (focus hwnd; caret
rect mapped from `hwndCaret` to screen with `MapWindowPoints` when `rcCaret` is non-empty), thread id, pid, exe path
(`QueryFullProcessImageNameW`), keyboard layout of that thread, work area of the monitor holding the caret (else the
window). `Target::is_foreground()`. `ForegroundWatch::start(sink: impl Fn(isize) + 'static)` wraps
`SetWinEventHook(EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT)` on the calling (UI) thread.

```rust
pub struct Target { pub hwnd: isize, pub focus: isize, pub thread_id: u32, pub pid: u32,
                    pub exe: Option<PathBuf>, pub layout: isize, pub caret: Option<RectI>, pub work_area: RectI,
                    pub elevated: bool }
```

**caret** — `CaretLocator::start()` creates an MTA worker thread that owns a warm `IUIAutomation`
(`CUIAutomation8`). `locate(&self, target, budget) -> Option<Placement>`:
1. `target.caret` (GetGUIThreadInfo) → `Placement::Caret`.
2. Worker: MSAA `AccessibleObjectFromWindow(focus, OBJID_CARET)` → `accLocation` (works for Chromium/Electron,
   Word, Visual Studio). Chromium windows (class `Chrome_WidgetWin_*` / `Chrome_RenderWidgetHostHWND`) stop here:
   never ask them for UIA or `OBJID_CLIENT` (that switches Chromium's full accessibility on and slows the browser).
   Others: UIA focused element → `TextPattern2::GetCaretRange` → `GetBoundingRectangles` → `Placement::Caret`; else
   the focused element's `CurrentBoundingRectangle` (if smaller than 80 % of the work area) → `Placement::Element`.
3. Answers later than `budget` are dropped; a process that missed the budget is remembered and the worker is skipped
   for it for 60 s (no repeated waits).
`pub enum Placement { Caret(RectI), Element(RectI) }`
`pub fn panel_origin(placement: Option<Placement>, cursor: PointI, panel_px: (i32, i32), work_area: RectI,
scale: f32) -> PointI` implements §8's placement rules (pure, unit tested).

**apps** — `display_name(exe) -> String` (FileDescription from the version resource, else file stem; cached),
`icon(exe, size_px) -> Option<Image>` (shell icon as straight-alpha BGRA; cached per exe+size).

**Adapted from Glint** (copied into `src/glint/` for reference; move each into `src/` renamed for Tuck, delete the
reference copy): `tray`, `instance` (mutex `Local\Tuck.SingleInstance`, IPC window `Tuck.Ipc`, WM_COPYDATA),
`install` (§8; drop Print Screen and ms-screenclip parts), `logging`, `paths` (`local_data_dir()` =
`%LOCALAPPDATA%\Tuck`, `settings_dir()` = `%APPDATA%\Tuck`), `settings_store` (tuck_core::Settings),
`shell` (open_uri, open_path, reveal_in_explorer), `ocr` (`recognize(&Image) -> Result<String>`), `util`.
`hotkey.rs` is the model for §3.

## 5. Clipboard history (tuck-clip, tuck-store, tuck-core::history)

### Watching (tuck-clip)
A dedicated clipboard thread owns a message-only window with `AddClipboardFormatListener`. On `WM_CLIPBOARDUPDATE`
it waits 40 ms (apps often set the clipboard several times), reads `GetClipboardSequenceNumber` and skips sequences
already seen, then `OpenClipboard` with retries (8 tries, 10→80 ms backoff). Delayed rendering can block on a hung
owner: that is why this is its own thread; nothing on the UI thread ever opens the clipboard.

Skip (no item) when any of these hold, checked before reading data:
- format `ExcludeClipboardContentFromMonitorProcessing` present; `Clipboard Viewer Ignore` present;
  `CanIncludeInClipboardHistory` present with DWORD 0 (password managers set these);
- our own write (format `Tuck.Token` present → `ClipEvent::OwnWrite { token }`);
- paused; source exe in `ignored_apps`; nothing readable; larger than `max_bytes`.

Source app: `GetClipboardOwner` → process exe; if the owner is null or belongs to a shell host, use the foreground
window's process.

What becomes the item (first match wins):
1. `CF_HDROP` → files.
2. `CF_UNICODETEXT` non-empty (after trimming only `\0`) → text, plus `HTML Format` and `Rich Text Format` if present.
3. Image: registered `PNG` (keeps alpha) else `CF_DIBV5` else `CF_DIB` → `Image` (BGRA straight alpha; DIBs with an
   all-zero alpha channel are opaque; handle BI_BITFIELDS and bottom-up rows).

Writing (`write(payload, token, done)`): on the clipboard thread, `OpenClipboard(own window)`, `EmptyClipboard`, set
every present format (text → CF_UNICODETEXT; html → `HTML Format` as given; rtf → `Rich Text Format`; image →
`PNG` + `CF_DIBV5`; files → `CF_HDROP` + `Preferred DropEffect` = copy) plus `Tuck.Token` (8-byte LE token), close,
then call `done`.

```rust
pub struct Captured { pub sequence: u32, pub text: Option<String>, pub html: Option<String>,
                      pub rtf: Option<String>, pub image: Option<Image>, pub files: Vec<PathBuf>,
                      pub source_exe: Option<PathBuf> }
pub enum SkipReason { Excluded, IgnoredApp, Paused, TooLarge, Empty, Unreadable }
pub enum ClipEvent { Captured(Captured), OwnWrite { token: u64 }, Skipped(SkipReason) }
pub struct CaptureFilter { pub paused: bool, pub ignored_apps: Vec<String>, pub max_bytes: u64 }
pub struct WritePayload { pub text: Option<String>, pub html: Option<String>, pub rtf: Option<String>,
                          pub image: Option<Image>, pub files: Vec<PathBuf> }
pub struct ClipboardService { .. }
impl ClipboardService {
    pub fn start(filter: CaptureFilter, sink: impl Fn(ClipEvent) + Send + 'static) -> Result<Self>;
    pub fn set_filter(&self, filter: CaptureFilter);
    pub fn write(&self, payload: WritePayload, token: u64, done: impl FnOnce(Result<()>) + Send + 'static);
}
impl Drop for ClipboardService { /* RemoveClipboardFormatListener, stop thread */ }
pub fn import_windows_history() -> Result<Vec<Captured>>;   // WinRT Clipboard::GetHistoryItemsAsync, oldest first
```
Pure helpers live in `formats.rs` with unit tests on synthetic bytes: DIB/DIBV5 ⇄ Image, HDROP parse/build, CF_HTML
build (from a fragment) and parse (fragment extraction for previews).

### History semantics (tuck-core::history, see the signatures in `history.rs`)
- Ordered by `used_ms`, newest first. Pinned items are shown in their own section but keep their order.
- `add`: an item with the same hash moves to the top (content/source replaced, `created_ms` and `pinned` kept);
  otherwise a new item with the next id. Then the app calls `trim(max_items)`.
- `search`: case- and accent-insensitive (NFD, strip combining marks, lowercase); all terms must match; whole word >
  prefix > substring; ties keep history order.
- `thumbnail`: area-averaged downscale, alpha-weighted.

### Persistence (tuck-store)
`%LOCALAPPDATA%\Tuck\history\history.tlog` + `blobs\<id:016x>.bin`. Log: 8-byte magic `TUCKLOG1`, then records
`[u32 LE length][payload]`, payload = DPAPI `CryptProtectData` (user scope, no UI) of the JSON of one record (a serde
mirror of `tuck_core::Op`). Blobs: DPAPI-protected PNG. A torn last record is ignored and truncated on open. Compact
(rewrite live items to a temp file, atomic rename, delete unreferenced blobs) on open when the log is over 2× the
live size or 8 MB, and on `compact()`. `keep_unpinned = false` drops unpinned items at open (Windows' behavior).
The store is single-threaded; the app owns it on a store thread.

```rust
pub enum Protection { Dpapi, Plain }   // Plain only for tests
pub struct Store { .. }
impl Store {
    pub fn open(dir: &Path, protection: Protection, keep_unpinned: bool) -> Result<(Store, History)>;
    pub fn apply(&mut self, ops: &[Op]) -> Result<()>;
    pub fn put_image(&mut self, blob: &str, image: &Image) -> Result<()>;
    pub fn get_image(&self, blob: &str) -> Result<Image>;
    pub fn compact(&mut self, history: &History) -> Result<()>;
    pub fn wipe(&mut self) -> Result<()>;
}
pub fn blob_name(id: ClipId) -> String;   // format!("{id:016x}")
```

## 6. Emoji, kaomoji, symbols (tuck-emoji, tools/gen-data)

Data is generated offline and committed: `crates/tuck-emoji/data/{emoji.tsv, symbols.tsv, kaomoji.tsv,
LICENSE-UNICODE.txt}`, embedded with `include_str!`. `cargo run -p gen-data -- --fetch` downloads (with `curl.exe`)
into `tools/gen-data/cache/` (gitignored) the latest `emoji-test.txt`, CLDR English `annotations.json` +
`annotationsDerived.json` (cldr-json, `cldr-annotations-full` / `cldr-annotations-derived-full`, locale `en`) and
`UnicodeData.txt`, then writes the TSVs. `kaomoji.tsv` is hand-curated in the repo (not generated). Pin the sources: `https://www.unicode.org/Public/18.0.0/
emoji/emoji-test.txt`, `https://www.unicode.org/Public/18.0.0/ucd/UnicodeData.txt`, cldr-json tag `48.2.3`
(`https://raw.githubusercontent.com/unicode-org/cldr-json/48.2.3/cldr-json/cldr-annotations-full/annotations/en/
annotations.json` and `.../cldr-annotations-derived-full/annotationsDerived/en/annotations.json`). Unicode data is
under the Unicode License v3; ship `LICENSE-UNICODE.txt` (https://www.unicode.org/license.txt) beside the data.
Stock Windows 11 24H2/25H2 renders Emoji 16.0; newer entries stay in the data and are hidden by coverage.

- `emoji.tsv`: `group \t emoji \t name \t keywords(|) \t version \t toned(|)` — fully-qualified sequences only, group
  "Component" dropped, `toned` = the five uniform-tone variants (light → dark) from emoji-test.txt when they exist
  (for multi-person sequences only the ones where every person has the same tone).
- `symbols.tsv`: `group \t char \t name` for fixed code point sets: Punctuation, Currency, Arrows, Math, Geometric,
  Letterlike & technical (©®™℃№…), Latin accented, Greek, Box & blocks. Names from UnicodeData, lowercase.
- `kaomoji.tsv`: `group \t kaomoji \t keywords(|)`, ≥ 220 entries in ≥ 10 groups (Happy, Love, Greeting, Cute,
  Laughing, Sad, Angry, Surprised, Embarrassed, Confused, Shrug & Apathetic, Sleepy, Animals, Classic ASCII,
  Actions/Table flip). Each one checked to render in Segoe UI + fallbacks without tofu.

```rust
pub enum Kind { Emoji, Kaomoji, Symbol }
pub struct Group { pub key: String, pub title: String }
pub struct Entry { pub text: String, pub name: String, pub keywords: Vec<String>, pub group: u16,
                   pub toned: Vec<String> /* empty or 5 */, pub version: f32 }
pub struct Catalog { pub kind: Kind, pub groups: Vec<Group>, pub entries: Vec<Entry> }
pub fn catalog(kind: Kind) -> &'static Catalog;                  // parsed once (OnceLock), < 15 ms total
impl Entry { pub fn with_tone(&self, tone: SkinTone) -> &str; }
pub fn search(kind: Kind, query: &str, usage: &Usage) -> Vec<u32>;  // entry indices, best first
#[derive(Serialize, Deserialize, Default)] pub struct Usage { .. }
impl Usage { pub fn record(&mut self, kind: Kind, text: &str, now_ms: i64);
             pub fn frequent(&self, kind: Kind, limit: usize) -> Vec<String>;   // frecency, newest wins ties
             pub fn clear(&mut self, kind: Kind); }
pub struct Coverage { .. }  // which emoji entries render as one glyph in Segoe UI Emoji
impl Coverage { pub fn compute() -> Result<Coverage>; pub fn load_or_compute(cache: &Path) -> Result<Coverage>;
                pub fn supports(&self, index: u32) -> bool; }
```
Search: query split on whitespace, NFD-folded lowercase; every term must match a name word or keyword. Score: exact
name 100, name word exact 85, name word prefix 70, keyword exact 65, keyword prefix 55, name substring 30; plus usage
boost (≤ 20) then catalog order. A query that is itself an emoji/symbol finds it; `U+20AC` / `20ac` finds the code
point (symbols). Kaomoji match group title and keywords. Search over all emoji < 2 ms in release.

Coverage: shape each emoji with the `Segoe UI Emoji` font face (DirectWrite text analyzer or a counting text
renderer); supported = exactly one non-zero glyph. Cache keyed by the font file's size + write time + data version.
Unsupported emoji are hidden everywhere. Frequency (`frecency`): count decays with a 14-day half-life.

## 7. The panel (tuck-panel)

One pre-created, hidden popup window (`WindowSpec::popup`, topmost, non-activating, never takes focus), 380×460
DIP. Built only from tuck-ui; data comes in through setters, results go out as `PanelAction` via `cx.post`.

### Layout (DIP, 12 outer padding)
- Search row (top, 34 tall): search field (magnifier, text, placeholder "Search clipboard" / "Search emoji" /
  "Search kaomoji" / "Search symbols", clear ✕ when non-empty, blinking caret while the panel is open) + 32×32 gear
  icon button (→ `OpenSettings`).
- Tab bar (32 tall, 8 below): segmented control, four segments with icon + label: Clipboard, Emoji, Kaomoji,
  Symbols. Tooltips name the shortcut (Win+V, Win+.).
- Content (fills the rest): see tabs. Scrolls with spring-smoothed wheel/touchpad, overlay scrollbar that fades in
  while scrolling.
- Bottom bar (40 tall, separated by a hairline): Emoji: category buttons (Frequently used + each group) as 28×28
  icons with an accent pill on the current one; clicking springs the scroll to that section, scrolling updates it.
  Skin-tone button (current tone swatch) at the right opens a 6-swatch popover. Kaomoji/Symbols: horizontally
  scrolling group chips. Clipboard: "N items" (or "Paused" with Resume) left, "Clear" (destructive text button,
  asks inline "Clear unpinned? Clear / Cancel") right.

### Clipboard tab
- Sections "Pinned" and "Recent" (caption 11 semibold secondary, sticky). Each item is a card (radius 10, fill
  hover-level tint, 8 gap): text up to 4 lines (Code kind: monospace `Cascadia Mono` 12, Url: link icon + host in
  semibold + full URL secondary single line, Color: 20 DIP swatch + the value, Email: mail icon, Path: folder
  icon + middle-ellipsized path), image (thumbnail fit in width × 110 DIP, radius 6, checkerboard under alpha, size
  "1920 × 1080" caption), files (file icon, first names, "+N more").
- Meta row under content: app icon 14 + app name · relative time ("now", "2 min", "Yesterday", "3 Oct"), caption
  tertiary. Pinned items show a pin glyph. While Ctrl is held the first nine cards show ⌃1…⌃9 badges.
- Hover shows pin and delete icon buttons at the card's top-right; right-click opens a menu: Paste, Paste as plain
  text, Copy, Pin/Unpin, Copy text from image (images), Edit in Glint (images, only when Glint is installed),
  Show in Explorer (files), Delete.
- Selection: keyboard-selected card has an accent ring 2 DIP; the ring moves with a spring between cards.
- Empty state (icon + "Nothing copied yet" + "Copy something and it will show up here."), no-results state ("No
  results for “query”"), paused banner at the top ("History is paused" + Resume).
- Deleting collapses the card (height spring) and the rest move up; pinning moves the card into Pinned with the
  same layout springs.

### Emoji, Kaomoji, Symbols tabs
- Emoji: grid of 40×40 cells (9 columns), emoji drawn at 28 DIP from `Segoe UI Emoji`'s COLRv1 (Fluent) glyphs.
  Plain `DrawText` with `ENABLE_COLOR_FONT` may only reach the flat COLRv0 layers, so tuck-ui gets an emoji drawing
  path: `IDWriteTextLayout::Draw` with a custom `IDWriteTextRenderer` (`#[windows::core::implement]`) that calls
  `TranslateColorGlyphRun` with `DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE` and draws paint runs with
  `ID2D1DeviceContext7::DrawPaintGlyphRun` (or `DrawGlyphRunWithColorSupport`), falling back to COLRv0 layers and
  then plain glyphs. Rasterized emoji are cached per (text, size px) in a glyph atlas so scrolling redraws bitmaps
  only. Sticky section headers. "Frequently used" section first (2 rows max). Search results replace the sections
  with one flat grid.
- Kaomoji: cells 2 per row (or 3 when short), 36 tall, body text. Symbols: grid of 36×36 cells, `Segoe UI Symbol`
  fallback chain.
- Hover / keyboard selection shows the item's name in a tooltip-style label above the cell after 350 ms (immediately
  for keyboard moves). Right-click (or long press) on an emoji with tones opens a popover with the 6 tone variants.

### Keyboard (routed by the app from the hook; the view handles `Event::KeyDown` / `Event::Text`)
Typing edits the search (Backspace, Ctrl+Backspace word, Ctrl+A selects all of the query; Left/Right move the caret
only when the query is non-empty and Shift is held, otherwise arrows move the selection). Arrows move the selection
(grid: 2-D; list: up/down), PageUp/PageDown, Home/End. Enter: insert/paste the selection (`close: true`). Shift+Enter
in Clipboard: the other paste mode (plain vs. formatted). Ctrl+Enter: copy only. Delete (Clipboard, when the query is
empty or the caret is at its end): delete the selected item. Ctrl+P: pin/unpin. Ctrl+1…9: paste that card. Tab /
Shift+Tab and Ctrl+Tab / Ctrl+Shift+Tab: next/previous tab. Esc: clear the query, else close.

### Mouse
Click an item: Clipboard → paste; Emoji/Kaomoji/Symbols → insert (`close` per `close_after_click`). Drag on the
search-row background or the tab-bar gaps moves the window (`DragTo`).

### Motion
Appear: opacity 0→1, scale 0.96→1, y −6→0 with the default spring; disappear: 120 ms fade + scale 0.98. Tab switch:
content cross-fade + 12 DIP slide in the direction of travel. Hover highlights snappy. Selection ring and section
pill move with springs. Nothing over 350 ms; honour reduced motion.

### API
```rust
pub enum Tab { Clipboard, Emoji, Kaomoji, Symbols }
pub struct PickerItem { pub text: String, pub name: String, pub variants: Vec<String> /* 6 incl. default, or empty */ }
pub struct PickerSection { pub title: String, pub icon: Icon, pub items: Vec<PickerItem> }
pub struct ClipRow { pub item: ClipItem, pub kind: TextKind, pub thumbnail: Option<Rc<Bitmap>>,
                     pub app_icon: Option<Rc<Bitmap>>, pub app_name: Option<String> }
pub enum PanelAction {
    Insert { tab: Tab, text: String, close: bool },
    Paste { id: ClipId, plain: bool },
    Copy { id: ClipId },
    SetPinned { id: ClipId, pinned: bool },
    Delete { id: ClipId },
    ClearUnpinned,
    CopyImageText { id: ClipId },
    EditInGlint { id: ClipId },
    ShowInExplorer { id: ClipId },
    QueryChanged { tab: Tab, query: String },
    TabChanged(Tab),
    NeedThumbnails(Vec<ClipId>),
    SkinTone(SkinTone),
    SetPaused(bool),
    OpenSettings,
    DragTo(PointI),           // new window origin, physical px
    Close,
}
pub struct PanelOptions { pub skin_tone: SkinTone, pub close_after_click: bool, pub plain_by_default: bool,
                          pub glint_available: bool, pub paused: bool }
pub struct Panel { .. }   // impl tuck_ui::View
impl Panel {
    pub fn new(options: PanelOptions) -> Self;
    pub fn set_options(&mut self, cx: &mut Ctx, options: PanelOptions);
    pub fn open(&mut self, cx: &mut Ctx, tab: Tab);          // clears query + selection, plays appear
    pub fn close(&mut self, cx: &mut Ctx);                   // plays disappear, then posts nothing (app hides)
    pub fn tab(&self) -> Tab;
    pub fn query(&self) -> &str;
    pub fn set_clips(&mut self, cx: &mut Ctx, rows: Vec<ClipRow>, total: usize);  // already filtered by query
    pub fn set_thumbnail(&mut self, cx: &mut Ctx, id: ClipId, bitmap: Rc<Bitmap>);
    pub fn set_sections(&mut self, cx: &mut Ctx, tab: Tab, sections: Vec<PickerSection>); // search → 1 section
    pub fn panel_rect(&self) -> RectF;                        // the visible card inside the window, DIP
}
```
`close()` returns after starting the animation; `Panel::is_closing_done()` tells the app when to hide the window.

## 8. The app (tuck-app)

Startup: logging → single instance (second instance forwards its args and exits) → settings → tuck-ui `run`:
create the panel window hidden and render its first frame (`render_hidden`) → tray → `InputHook` → store thread opens
the history → `ClipboardService` → `CaretLocator` → catalogs parsed on a worker, coverage computed in the background
(sections rebuilt when it lands) → toast "Tuck is running" unless `--background`.

Open (`Hotkey`): `capture_target()`; placement = target caret, else `CaretLocator::locate(budget 20 ms)`, else the
mouse cursor. Panel below the caret (6 DIP gap), above it when there is no room, left edge at caret.x − 24 DIP,
clamped to the work area with 8 DIP margin; `Element` placement anchors to the element's bottom-left; cursor
placement puts the panel 12 DIP below-right of the cursor. Set tab, `open()`, show without activation,
`begin_capture(rect)`, heartbeat timer (200 ms), foreground watch. Log "panel on screen N ms after the key".
Same hotkey while open on the same tab → close; other hotkey → switch tab.

Keys: `HookEvent::Key` → `TextTranslator` (target layout) → `Event::KeyDown` (+ `Event::Text`) into the panel via
`App::with_view`. Ctrl held state is forwarded for the ⌃ badges. `Dismiss`, foreground change away from the target,
or the target window closing → close.

Actions:
- `Insert`: close → `type_text` → `Usage::record` (saved debounced to `%LOCALAPPDATA%\Tuck\usage.json`). With
  `close: false` the panel stays and the text is typed into the still-focused target.
- `Paste {id, plain}`: close → build `WritePayload` (plain → text only; image loaded from the store) → `write` →
  on done `paste(settings.paste.key_for(target exe))` → `history.touch` → store op. No target (opened from the
  tray) → copy only + a small "Copied" toast.
- `Copy`, `SetPinned`, `Delete`, `ClearUnpinned` → history + store; panel rows refreshed.
- `CopyImageText`: OCR off the UI thread → write text → toast "Text copied".
- `EditInGlint`: write the image to `%TEMP%\Tuck\<id>.png`, run `glint --edit <path>` from the Glint install path.
- `QueryChanged` → `History::search` / `tuck_emoji::search` → setters. `NeedThumbnails` → store thread loads blobs →
  `thumbnail(…, 2× card size)` → `set_thumbnail`.
- `DragTo` → move the window, update the hook's panel rect.

Clipboard events: `Captured` → hash (`text_hash`/`image_hash`/`files_hash`) → `History::add` → image saved with
`put_image(blob_name(id))` on the store thread → `trim` → ops to the store → panel rows (kept current while hidden so
opening never waits). `OwnWrite` → nothing (the paste path already touched the item).

Settings window (`WindowSpec::normal`, Mica, unified title bar, 640×560, sidebar-less grouped sections like Glint's):
General (Appearance System/Light/Dark, Launch at login, Win+V and Win+. toggles), Clipboard (Record history, Keep N
items menu, Keep after restart, Paste plain text by default, Ignored apps list with remove + "Add app…" menu of
running apps with windows, Import Windows clipboard history, Clear history… with confirm), Emoji (skin tone swatches,
Close after clicking, Clear frequently used), About (version, GitHub link, Uninstall…). Changes apply live and save.

Tray (left-click opens the panel on Clipboard at the tray corner; right-click menu): Clipboard history, Emoji,
Pause history (checked), Settings…, Quit Tuck.

Install / CLI (Glint's pattern): `tuck` (start resident + toast), `tuck --background` (Run key),
`tuck --clipboard`, `tuck --emoji`, `tuck --settings`, `tuck --install` (copy to `%LOCALAPPDATA%\Programs\Tuck`,
HKCU Run `Tuck` = `"…\tuck.exe" --background`, Start-menu shortcut, HKCU Uninstall entry, start the installed
copy), `tuck --uninstall` (reverse; data kept unless `--purge`), `tuck --quit`, `tuck --selftest [--json]`,
`tuck --preview <view> --out <png> [--theme dark|light] [--scale 1|1.5|2]`. Windows' own clipboard history and
emoji panel settings are never changed; when Tuck is not running the keys go back to Windows.

Icon art: drawn with tuck-ui like Glint's `art.rs` (rounded tile with a warm gradient and a white glyph that reads as
"clipboard + sparkle"), producing `res/tuck.ico` (16–256) via an example, the tray glyph (monochrome, follows the
taskbar theme) and the settings/toast artwork.

Known limits (README): elevated windows get neither the hook nor injected input (UIPI), so Windows' own popups answer
there; IME composition is not available in the panel's search field; games in exclusive fullscreen hide the panel;
no GIF tab (Tenor's API shut down 2026-06-30, GIPHY needs a per-app key). Windows Terminal exposes no caret, so the
panel opens at the focused element or the cursor there. `import_windows_history` runs only from the settings window
(WinRT may deny clipboard-history access to a background app).

## 9. Visual language

Glint's §5 tokens apply unchanged (type scale, grid, palettes, shadows, motion; see `tuck-ui/src/theme.rs`). Panel
specifics:
- Material: DWM acrylic behind the panel, with the theme's glass fill on top at reduced alpha (dark
  `rgba(30,30,32,0.55)`, light `rgba(246,246,248,0.62)`) and a hairline inner border. The panel window is a new
  tuck-ui window kind (`WindowSpec::panel`): `WS_POPUP`, `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
  WS_EX_NOREDIRECTIONBITMAP`, **not** `WS_EX_LAYERED` (layered windows can never get DWM rounding), DComp content,
  window rect = panel rect (no shadow margins). Before the first show: `DWMWA_WINDOW_CORNER_PREFERENCE` = ROUND,
  `DWMWA_BORDER_COLOR` = `DWMWA_COLOR_NONE`, `DWMWA_SYSTEMBACKDROP_TYPE` = `DWMSBT_TRANSIENTWINDOW` with
  `DwmExtendFrameIntoClientArea(-1)`; the backdrop is set once and never switched (switching after creation fails,
  and accent policy and DWM backdrops do not mix). Whether DWM keeps acrylic on a never-activated window is
  unconfirmed: the kit supports `PanelBackdrop::{SystemAcrylic, AccentAcrylic, Solid}` (accent =
  `SetWindowCompositionAttribute(WCA_ACCENT_POLICY = 19, ACCENT_ENABLE_ACRYLICBLURBEHIND = 4)`), sends itself
  `WM_NCACTIVATE(TRUE)` through `DefWindowProc` after showing, and `cargo run -p tuck-ui --example backdrop_probe`
  (main thread only) decided the mode: on 2026-10-06 system acrylic rendered solid on the never-activated panel and the accent acrylic blurred, so AccentAcrylic ships. Solid = the opaque glass fill. Offscreen previews draw a synthetic
  blurred wallpaper behind the panel. Radius: DWM round (8) for the window; inner content radii below.
- Search field: radius 8, fill `rgba(255,255,255,0.07)` dark / `rgba(0,0,0,0.05)` light, focus ring none (the panel
  is always "focused"), caret accent color 1.5 DIP, blinking 530 ms on / 530 ms off (solid while typing).
- Cards: radius 10; selected ring 2 DIP accent; hover tint = theme hover.
- Emoji cells: hover = theme hover, rounded 8; selected = theme selected + 1.5 DIP accent ring.

No first-frame flash: the window is shown only after its first frame is committed. Idle cost 0 % (render only while
something animates or changes; caret blink is the only idle animation and only while the panel is open).

## 10. Performance budgets (release build)

- Hotkey key-down → panel first frame on screen: ≤ 16 ms p95 with the caret known synchronously, ≤ 40 ms when the
  UIA path runs. Logged every time.
- Keystroke → updated results painted: ≤ 8 ms. `tuck_emoji::search` ≤ 2 ms, `History::search` (1000 items) ≤ 3 ms.
- Copy → item in history (text): ≤ 60 ms after `WM_CLIPBOARDUPDATE` (40 ms of that is the settle delay).
- Paste: panel closed and Ctrl+V injected ≤ 30 ms after Enter for text items.
- Startup to hooks live: ≤ 300 ms. Resident memory ≤ 80 MB with 200 items. Idle CPU 0 %.

## 11. Verification

- tuck-core: `cargo test -p tuck-core` (classify, hashes, history ops, search ranking, thumbnail).
- tuck-sys: unit tests for the routing state machine, the dead-key combining logic (a pure function fed with fixed
  `ToUnicodeEx` outputs; never call `LoadKeyboardLayoutW`/`ActivateKeyboardLayout`, they change user state),
  modifier-release planning, placement math.
  `cargo run -p tuck-sys --example probe` prints the current foreground target, caret placement (both paths, with
  timings) and app name — read-only, no hook, no window.
- tuck-clip: unit tests for `formats.rs`. No test touches the real clipboard.
- tuck-store: round-trip tests in a temp dir with `Protection::Dpapi` and `Plain`, torn-tail recovery, compaction,
  `keep_unpinned = false`.
- tuck-emoji: `cargo test -p tuck-emoji` (catalog counts, tones, search ranking cases: "heart" → ❤️ first,
  "thumbs" → 👍, "euro" → €, "shrug" → ¯\_(ツ)_/¯, timing); `cargo run -p tuck-emoji --example coverage` prints
  supported/unsupported counts and the newest unsupported emoji.
- tuck-ui: `cargo run -p tuck-ui --example gallery -- --out <dir>`.
- tuck-panel: `cargo run -p tuck-panel --example gallery -- --out <dir>` renders every tab/state (synthetic data,
  dark + light, scale 1 and 2) to PNGs.
- tuck-app: `tuck --preview panel-clipboard|panel-clipboard-empty|panel-clipboard-search|panel-emoji|
  panel-emoji-search|panel-emoji-tones|panel-kaomoji|panel-symbols|panel-menu|settings|settings-clipboard|toast
  --out x.png`; `tuck --selftest --json` (catalogs, search cases, coverage, store round trip in a temp dir, formats,
  previews render, timing budgets that can be measured headless).

## 12. Shared dependencies

Pinned in the workspace root; crates use `{ workspace = true }`. Add a dependency only through the main thread.
