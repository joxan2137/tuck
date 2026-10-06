# tuck-clip API

The clipboard thread (DESIGN.md §5 "Watching"). Depends on tuck-core only. Nothing on the UI thread ever opens the
clipboard: every read and write happens on the service's own thread.

## Service

```rust
pub struct ClipboardService { .. }          // Send + Sync
impl ClipboardService {
    pub fn start(filter: CaptureFilter, sink: impl Fn(ClipEvent) + Send + 'static) -> Result<Self>;
    pub fn set_filter(&self, filter: CaptureFilter);
    pub fn write(&self, payload: WritePayload, token: u64, done: impl FnOnce(Result<()>) + Send + 'static);
}
impl Drop for ClipboardService { .. }       // removes the listener, stops the thread (waits ≤ 1 s, then detaches)
```

- `start` spawns thread `tuck-clipboard` with a message-only window (`Tuck.Clipboard`) registered through
  `AddClipboardFormatListener`, and returns once it is listening. The copy that is on the clipboard at start is
  treated as already seen (no event for it).
- **`sink` and `done` run on the clipboard thread.** Keep them short (post to your own thread); a slow sink delays
  the next capture and every queued write.
- `write` queues the write and returns at once. On the clipboard thread: encode everything, `OpenClipboard(own
  window)` with retries, `EmptyClipboard`, set the formats below, close, then `done(result)`. If the thread is gone,
  `done(Err)` runs on the caller's thread. An empty payload is an error.

### Capture pipeline
`WM_CLIPBOARDUPDATE` → 40 ms settle timer (restarted by each further update) → `GetClipboardSequenceNumber`
(already-seen sequences produce no event) → `OpenClipboard` (8 tries, waits 10, 20, 40, 80, 80, 80, 80 ms) → checks
in this order, first hit wins:

| Check | Event |
|---|---|
| clipboard stayed busy | `Skipped(Unreadable)` |
| `ExcludeClipboardContentFromMonitorProcessing` or `Clipboard Viewer Ignore` present, or `CanIncludeInClipboardHistory` present and not a non-zero DWORD | `Skipped(Excluded)` |
| `Tuck.Token` present (8-byte LE) | `OwnWrite { token }` |
| `filter.paused` | `Skipped(Paused)` |
| source exe file name in `filter.ignored_apps` | `Skipped(IgnoredApp)` |
| item (below) larger than `filter.max_bytes` | `Skipped(TooLarge)` |
| no item | `Skipped(Empty)`; `Skipped(Unreadable)` when a present format failed to decode |
| otherwise | `Captured(..)` |

Item choice (first match wins; a format that fails to decode falls through to the next):
1. `CF_HDROP` with at least one path → `files`.
2. `CF_UNICODETEXT` non-empty (cut at the first NUL; whitespace counts) → `text`, plus `html` ("HTML Format", UTF-8,
   the full CF_HTML payload) and `rtf` ("Rich Text Format", ANSI code page) when present. Text alone must fit
   `max_bytes`; HTML then RTF are dropped (not the copy) if adding them would exceed it.
3. `PNG` (alpha kept), else `CF_DIBV5`, else `CF_DIB` → `image`. Size is checked from the header before decoding.

Size = `Captured::byte_len()` = text + html + rtf UTF-8 bytes, image `w * h * 4`, or path bytes (`ClipItem::bytes`).

Source app: `GetClipboardOwner` → process image path. When the owner is null or a shell host (`explorer.exe`,
`sihost.exe`, `shellexperiencehost.exe`, `startmenuexperiencehost.exe`, `searchhost.exe`, `textinputhost.exe`,
`applicationframehost.exe`), the foreground window's process is used instead (falling back to the owner).

### Write formats
| Payload field | Clipboard formats |
|---|---|
| `text` | `CF_UNICODETEXT` |
| `html` | `HTML Format`, written as given (must already be CF_HTML; see `formats::cf_html_from_fragment`) |
| `rtf` | `Rich Text Format` (ANSI code page) |
| `image` | `PNG` (fast compression) + `CF_DIBV5` (32 bpp BI_BITFIELDS, straight alpha) |
| `files` | `CF_HDROP` (wide) + `Preferred DropEffect` = `DROPEFFECT_COPY` |
| always | `Tuck.Token` = `token.to_le_bytes()`; the resulting update arrives as `ClipEvent::OwnWrite { token }` |

## Types

```rust
pub struct Captured { pub sequence: u32, pub text: Option<String>, pub html: Option<String>,
                      pub rtf: Option<String>, pub image: Option<Image>, pub files: Vec<PathBuf>,
                      pub source_exe: Option<PathBuf> }                  // Clone, Debug, Default, PartialEq
impl Captured {
    pub fn kind(&self) -> Option<ClipKind>;   // Files > Text > Image, None when empty
    pub fn byte_len(&self) -> u64;            // what ClipItem::bytes stores
}
pub enum SkipReason { Excluded, IgnoredApp, Paused, TooLarge, Empty, Unreadable }   // Copy, Eq
pub enum ClipEvent { Captured(Captured), OwnWrite { token: u64 }, Skipped(SkipReason) }
pub struct CaptureFilter { pub paused: bool, pub ignored_apps: Vec<String>, pub max_bytes: u64 }
impl CaptureFilter { pub fn ignores(&self, exe: &Path) -> bool; }   // exe file name, case-insensitive
impl From<&HistorySettings> for CaptureFilter;   // paused = !enabled, max_bytes = max_item_mb MiB
impl Default for CaptureFilter;                  // from HistorySettings::default() (32 MiB)
pub struct WritePayload { pub text: Option<String>, pub html: Option<String>, pub rtf: Option<String>,
                          pub image: Option<Image>, pub files: Vec<PathBuf> }   // Clone, Debug, Default, PartialEq
```

Exactly one of `files`, `text` (+ `html`/`rtf`) or `image` is set in a `Captured`. Imported items have
`sequence = 0` and `source_exe = None`.

## Windows clipboard history

```rust
pub fn import_windows_history() -> Result<Vec<Captured>>;
```
Blocking. Runs `Windows.ApplicationModel.DataTransfer.Clipboard::GetHistoryItemsAsync` on a private MTA thread, so
any thread (including an STA UI thread) may call it. A non-`Success` status is an error naming it
(`AccessDenied`, `ClipboardHistoryDisabled`). Items are returned oldest first (sorted by timestamp) and read with the
same priority as live copies: StorageItems paths, else Text (+ Html as CF_HTML, + Rtf), else Bitmap (BitmapDecoder →
SoftwareBitmap BGRA8 straight alpha). Items that fail to read are skipped with a warning. Call it only while Tuck is
in the foreground (the settings window); WinRT may deny background callers.

## `formats` (pure helpers, `pub mod formats`)

```rust
pub fn dib_to_image(bytes: &[u8]) -> Result<Image>;          // 24/32 bpp, BI_RGB/BI_BITFIELDS/BI_ALPHABITFIELDS,
                                                             // either row order; all-zero alpha → opaque
pub fn dib_dimensions(bytes: &[u8]) -> Option<(u32, u32)>;
pub fn image_to_dibv5(image: &Image) -> Vec<u8>;
pub fn decode_png(bytes: &[u8]) -> Result<Image>;
pub fn encode_png(image: &Image) -> Result<Vec<u8>>;
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)>;
pub fn parse_hdrop(bytes: &[u8]) -> Result<Vec<PathBuf>>;     // wide or ANSI DROPFILES
pub fn build_hdrop(paths: &[PathBuf]) -> Vec<u8>;            // wide; pass absolute paths
pub fn cf_html_from_fragment(fragment: &str) -> String;      // full CF_HTML, UTF-8 byte offsets
pub fn cf_html_fragment(cf_html: &str) -> Option<&str>;      // header offsets, else the fragment comments
pub fn utf16_until_nul(bytes: &[u8]) -> &[u8];
pub fn bytes_until_nul(bytes: &[u8]) -> &[u8];
pub fn decode_utf16_text(bytes: &[u8]) -> String;  pub fn encode_utf16_text(text: &str) -> Vec<u8>;
pub fn decode_utf8_text(bytes: &[u8]) -> String;   pub fn encode_utf8_text(text: &str) -> Vec<u8>;
pub fn decode_ansi_text(bytes: &[u8]) -> String;   pub fn encode_ansi_text(text: &str) -> Vec<u8>;
pub fn image_bytes(width: u32, height: u32) -> u64;          // w * h * 4, saturating
pub fn files_bytes(paths: &[PathBuf]) -> u64;
```
Images are tuck-core `Image`s: BGRA, straight alpha, top-down. Encoders append the NUL terminator the clipboard
expects; decoders stop at the first NUL.

## Logging and tests
Logs only kinds, byte counts, timings and exe names (`log` crate, `debug` for each event, `warn` for failures);
never clipboard contents. `cargo test -p tuck-clip` covers `formats`, write-entry assembly, retry timing, filters and
sizes with synthetic data; no test opens the real clipboard or starts the service.
