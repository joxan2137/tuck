# tuck-store API

Persistent clipboard history (DESIGN.md §5). Depends on `tuck-core` only. The store is single-threaded: the app
owns it on the store thread and sends it ops; `History` itself lives on the UI thread (tuck-core).

```rust
pub enum Protection { Dpapi, Plain }   // Plain only for tests
pub struct Store { .. }                // Send, not Sync
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

## On disk (under `dir`, normally `%LOCALAPPDATA%\Tuck`)

```
history\history.tlog          8-byte magic "TUCKLOG1", then records [u32 LE length][payload]
history\blobs\<name>.bin      protected PNG
history\history.tlog.tmp      compaction scratch file (renamed over the log)
history\history.tlog.corrupt  an unreadable log moved aside (see open)
```

Payload = `CryptProtectData` (user scope, `CRYPTPROTECT_UI_FORBIDDEN`, entropy `b"Tuck history v1"`) of the JSON of
one `Record`, a serde mirror of `tuck_core::Op` (`Add`, `Touch`, `Pin`, `Remove`). `Protection::Plain` stores the JSON
and the PNG as is. Blob names are `[A-Za-z0-9_-]{1,64}`; anything else is an error (no path traversal).

## Behaviour

- `open` creates `dir\history\blobs` and the log when missing, replays the log and returns the `History`
  (`History::from_items`). Replay: `Add` inserts; `Touch` sets `used_ms`, replaces `content` (recomputing `bytes`
  with `tuck_core::history::content_bytes`) and `source` when `Some`; `Pin`; `Remove`; ops for unknown ids are ignored.
- An incomplete last record, or a run of undecodable records at the end (a zero-filled tail after a crash), is
  dropped and the file truncated. Undecodable records between good ones are skipped and the log is compacted.
  A file that is not a log, or whose records are all undecodable (e.g. written under another Windows user), is moved
  to `history.tlog.corrupt` and an empty log starts; it is never silently destroyed.
- `keep_unpinned = false` drops unpinned items at open, permanently (the log is compacted, their blobs deleted).
- `open` compacts when the log is over 2x the live size, or over 8 MB with at least a quarter of it dead.
- `apply` appends all ops in one write and flushes. A failed write is rolled back by truncating to the old length.
  Applying `Op::Remove(id)` also deletes the blob named `blob_name(id)` (image items keep that name), so deleting or
  clearing history removes pixels at once. The log is recreated if it was deleted (e.g. after `wipe`).
- `put_image` writes `<name>.bin.tmp` then renames, replacing any blob of that name. Opaque images are stored as RGB
  PNG, others as RGBA (alpha and the colour under alpha 0 survive). `get_image` reverses it; BGRA straight alpha.
- `compact(&history)` rewrites the log with one `Add` per live item (oldest first) to a temp file, renames it over the
  log and deletes every file in `blobs` that no live image item references (strays, `.tmp` leftovers).
- `wipe` deletes the log, its scratch/corrupt files and all blobs (missing files are fine; it keeps trying after a
  failure and reports the first error). The store stays usable.
- Nothing is logged except counts, sizes and error kinds.

## Using it from the app

```rust
let (mut store, mut history) = Store::open(&local_data_dir, Protection::Dpapi, settings.history.keep_after_restart)?;

// image copy: name the blob with the id the next add will use, write it, then add
let blob = blob_name(history.peek_next_id());
store.put_image(&blob, &image)?;
let content = ClipContent::Image { blob, width, height };
let bytes = content_bytes(&content);
let (outcome, ops) = history.add(content, image_hash(&image), bytes, source, now_ms());
let (_removed, trim_ops) = history.trim(settings.history.max_items);
store.apply(&[ops, trim_ops].concat())?;
```

- History runs on the UI thread, the store on its own thread: send the ops (`Vec<Op>`) over a channel, keep the order.
- Use `tuck_core::history::content_bytes(&content)` for `add`'s `bytes`. After a restart `bytes` of a promoted item is
  recomputed with it, so a different formula would make `bytes` change across restarts.
- A copy that folds into an existing image item (`AddOutcome::Promoted`) writes a blob nobody references (the item
  keeps its old blob). Skip `put_image` when `Promoted`, or let the next compaction delete it.
- Call `compact(&history)` only with a history that already contains every item whose blob was put; otherwise a
  just-written blob looks like an orphan. A natural place: after `clear_unpinned`, and on a low-priority timer.
- Thumbnails: `get_image` then `tuck_core::thumbnail`. DPAPI costs about 0.15 ms per record to open and 0.5 ms per
  `apply` call (release, 1000 items: open 170 ms, compact 260 ms, one `apply` of a text item 0.6 ms), so open and
  compact on the store thread, never the UI thread.

## tuck-core additions made for the store

- `tuck_core::history::content_bytes(&ClipContent) -> u64`: text + html + rtf bytes, `width*height*4`, or the UTF-8
  length of the paths.
- `History::add` on a hash match keeps the existing image blob name and keeps the existing source when the new one is
  `None` (`Op::Touch` cannot express "clear the source"). `History::default()` equals `History::new()` (ids start at
  1). `History::search` indexes a folded copy of each item at add time (first 64 KiB of a text), so a search over 1000
  items takes about 0.3 ms.
