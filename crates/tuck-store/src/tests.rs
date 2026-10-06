use std::sync::atomic::{AtomicUsize, Ordering};

use tuck_core::SourceApp;
use tuck_core::clip::{files_hash, image_hash, text_hash};
use tuck_core::history::content_bytes;

use super::*;

const PROTECTIONS: [Protection; 2] = [Protection::Dpapi, Protection::Plain];

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let name = format!("tuck-store-{label}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst));
        let path = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn log_path(&self) -> PathBuf {
        self.0.join(HISTORY_DIR).join(LOG_FILE)
    }

    fn blob_path(&self, blob: &str) -> PathBuf {
        self.0.join(HISTORY_DIR).join(BLOBS_DIR).join(format!("{blob}.bin"))
    }

    fn log_len(&self) -> u64 {
        fs::metadata(self.log_path()).unwrap().len()
    }

    fn blob_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.0.join(HISTORY_DIR).join(BLOBS_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn app(name: &str) -> SourceApp {
    SourceApp { exe: PathBuf::from(format!(r"C:\Program Files\{name}\{name}.exe")), name: name.to_owned() }
}

fn rich_text(text: &str, html: Option<&str>, rtf: Option<&str>) -> ClipContent {
    ClipContent::Text { text: text.to_owned(), html: html.map(str::to_owned), rtf: rtf.map(str::to_owned) }
}

fn add(history: &mut History, content: ClipContent, hash: u64, source: Option<SourceApp>, now_ms: i64) -> Vec<Op> {
    let bytes = content_bytes(&content);
    history.add(content, hash, bytes, source, now_ms).1
}

fn add_text(history: &mut History, text: &str, now_ms: i64) -> Vec<Op> {
    add(history, rich_text(text, None, None), text_hash(text), Some(app("Notepad")), now_ms)
}

fn add_image(store: &mut Store, history: &mut History, pixel: u8, now_ms: i64) -> String {
    let blob = blob_name(history.peek_next_id());
    let image = solid_image(4, 3, [pixel, pixel, pixel, 255]);
    store.put_image(&blob, &image).unwrap();
    let content = ClipContent::Image { blob: blob.clone(), width: image.width, height: image.height };
    let ops = add(history, content, image_hash(&image), None, now_ms);
    store.apply(&ops).unwrap();
    blob
}

fn solid_image(width: u32, height: u32, bgra: [u8; 4]) -> Image {
    Image::from_bgra(width, height, bgra.repeat(width as usize * height as usize))
}

fn open(dir: &TempDir, protection: Protection) -> (Store, History) {
    Store::open(dir.path(), protection, true).unwrap()
}

fn append_raw(path: &Path, bytes: &[u8]) {
    OpenOptions::new().append(true).open(path).unwrap().write_all(bytes).unwrap();
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Adds, folds, pins, touches and removes through `history`, persisting every op as it happens.
fn scripted_history(store: &mut Store) -> History {
    let mut history = History::new();
    let ops = add_text(&mut history, "alpha needle-9f3a", 100);
    store.apply(&ops).unwrap();
    let rich = rich_text("beta ünïcode ✓", Some("<b>beta</b>"), Some(r"{\rtf1 beta}"));
    let ops = add(&mut history, rich, text_hash("beta"), Some(app("Edge")), 200);
    store.apply(&ops).unwrap();
    let paths = vec![PathBuf::from(r"C:\docs\a.txt"), PathBuf::from(r"D:\pics\b.png")];
    let ops = add(&mut history, ClipContent::Files { paths: paths.clone() }, files_hash(&paths), None, 300);
    store.apply(&ops).unwrap();
    add_image(store, &mut history, 90, 400);
    let pin = history.set_pinned(1, true).unwrap();
    let touch = history.touch(2, 500).unwrap();
    store.apply(&[pin, touch]).unwrap();
    let refolded = rich_text("alpha needle-9f3a", Some("<i>alpha</i>"), None);
    let ops = add(&mut history, refolded, text_hash("alpha needle-9f3a"), Some(app("Code")), 600);
    store.apply(&ops).unwrap();
    let ops = add_text(&mut history, "gamma", 700);
    store.apply(&ops).unwrap();
    let (_, remove) = history.remove(5).unwrap();
    store.apply(&[remove]).unwrap();
    history
}

fn round_trip(protection: Protection) {
    let dir = TempDir::new("roundtrip");
    let (mut store, empty) = open(&dir, protection);
    assert!(empty.is_empty());
    let history = scripted_history(&mut store);
    assert_eq!(history.len(), 4);

    let (_store, reopened) = open(&dir, protection);
    assert_eq!(reopened.items(), history.items());
    assert_eq!(reopened.peek_next_id(), 5);
    let alpha = reopened.get(1).unwrap();
    assert!(alpha.pinned);
    assert_eq!(alpha.created_ms, 100);
    assert_eq!(alpha.used_ms, 600);
    assert_eq!(alpha.source, Some(app("Code")));
    assert_eq!(alpha.bytes, content_bytes(&alpha.content));
    assert_eq!(reopened.items().iter().map(|item| item.id).collect::<Vec<_>>(), [1, 2, 4, 3]);
    assert_eq!(reopened.search("needle"), [1]);

    let log = fs::read(dir.log_path()).unwrap();
    assert!(log.starts_with(b"TUCKLOG1"));
    assert_eq!(contains(&log, b"needle-9f3a"), protection == Protection::Plain);
}

#[test]
fn round_trip_with_dpapi() {
    round_trip(Protection::Dpapi);
}

#[test]
fn round_trip_with_plain() {
    round_trip(Protection::Plain);
}

#[test]
fn empty_store_opens_with_an_empty_log() {
    for protection in PROTECTIONS {
        let dir = TempDir::new("empty");
        let (_store, history) = open(&dir, protection);
        assert!(history.is_empty());
        assert_eq!(fs::read(dir.log_path()).unwrap(), b"TUCKLOG1");
    }
}

#[test]
fn replay_follows_op_semantics() {
    let dir = TempDir::new("semantics");
    let (mut store, _) = open(&dir, Protection::Plain);
    let item = |id, hash, used_ms| ClipItem {
        id,
        content: rich_text("text", None, None),
        hash,
        created_ms: 10,
        used_ms,
        pinned: false,
        source: Some(app("Notepad")),
        bytes: 4,
    };
    let richer = rich_text("text", Some("<p>text</p>"), None);
    store
        .apply(&[
            Op::Add(item(1, 11, 10)),
            Op::Add(item(2, 22, 20)),
            Op::Add(item(3, 33, 30)),
            Op::Touch { id: 1, used_ms: 40, content: None, source: None },
            Op::Touch { id: 2, used_ms: 50, content: Some(richer.clone()), source: Some(app("Edge")) },
            Op::Touch { id: 99, used_ms: 60, content: None, source: None },
            Op::Pin { id: 3, pinned: true },
            Op::Pin { id: 99, pinned: true },
            Op::Remove(1),
            Op::Remove(99),
        ])
        .unwrap();

    let (_store, history) = open(&dir, Protection::Plain);
    assert_eq!(history.items().iter().map(|item| item.id).collect::<Vec<_>>(), [2, 3]);
    let touched = history.get(2).unwrap();
    assert_eq!((touched.used_ms, touched.created_ms, touched.pinned), (50, 10, false));
    assert_eq!(touched.content, richer);
    assert_eq!(touched.bytes, 15);
    assert_eq!(touched.source, Some(app("Edge")));
    let pinned = history.get(3).unwrap();
    assert!(pinned.pinned);
    assert_eq!(pinned.source, Some(app("Notepad")));
    assert_eq!(history.peek_next_id(), 4);
}

fn survives_torn_tail(protection: Protection, tail: &[u8]) {
    let dir = TempDir::new("torn");
    let (mut store, mut history) = open(&dir, protection);
    let ops = add_text(&mut history, "first", 100);
    store.apply(&ops).unwrap();
    let ops = add_text(&mut history, "second", 200);
    store.apply(&ops).unwrap();
    let intact_len = dir.log_len();
    let intact_items = history.items().to_vec();
    append_raw(&dir.log_path(), tail);
    assert!(dir.log_len() > intact_len);

    let (mut store, recovered) = open(&dir, protection);
    assert_eq!(recovered.items(), intact_items);
    assert_eq!(dir.log_len(), intact_len);

    let mut recovered = recovered;
    let ops = add_text(&mut recovered, "third", 300);
    store.apply(&ops).unwrap();
    let (_store, reopened) = open(&dir, protection);
    assert_eq!(reopened.items(), recovered.items());
    assert_eq!(reopened.len(), 3);
}

#[test]
fn torn_tail_is_ignored_and_truncated() {
    let tails: [&[u8]; 4] = [&[1, 2], &[100, 0, 0, 0, 1, 2, 3], &[3, 0, 0, 0, 9, 9, 9], &[0; 40]];
    for protection in PROTECTIONS {
        for tail in tails {
            survives_torn_tail(protection, tail);
        }
    }
}

#[test]
fn half_written_last_record_is_dropped() {
    for protection in PROTECTIONS {
        let dir = TempDir::new("half");
        let (mut store, mut history) = open(&dir, protection);
        let ops = add_text(&mut history, "kept", 100);
        store.apply(&ops).unwrap();
        let kept_len = dir.log_len();
        let kept_items = history.items().to_vec();
        let ops = add_text(&mut history, "lost", 200);
        store.apply(&ops).unwrap();
        let full_len = dir.log_len();
        OpenOptions::new().write(true).open(dir.log_path()).unwrap().set_len(full_len - 3).unwrap();

        let (_store, recovered) = open(&dir, protection);
        assert_eq!(recovered.items(), kept_items);
        assert_eq!(dir.log_len(), kept_len);
    }
}

#[test]
fn a_damaged_record_in_the_middle_is_skipped_and_compacted_away() {
    let dir = TempDir::new("middle");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    let ops = add_text(&mut history, "one", 100);
    store.apply(&ops).unwrap();
    let first_end = dir.log_len() as usize;
    let ops = add_text(&mut history, "two", 200);
    store.apply(&ops).unwrap();
    let ops = add_text(&mut history, "three", 300);
    store.apply(&ops).unwrap();
    let mut bytes = fs::read(dir.log_path()).unwrap();
    bytes[first_end + 4] = b'#';
    fs::write(dir.log_path(), &bytes).unwrap();

    let (_store, recovered) = open(&dir, Protection::Plain);
    assert_eq!(recovered.items().iter().map(|item| item.id).collect::<Vec<_>>(), [3, 1]);
    let (_store, again) = open(&dir, Protection::Plain);
    assert_eq!(again.items(), recovered.items());
}

#[test]
fn log_written_with_another_protection_is_moved_aside() {
    let dir = TempDir::new("foreign");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    for (text, now) in [("one", 100), ("two", 200), ("three", 300)] {
        let ops = add_text(&mut history, text, now);
        store.apply(&ops).unwrap();
    }
    let original = fs::read(dir.log_path()).unwrap();

    let (mut store, fresh) = open(&dir, Protection::Dpapi);
    assert!(fresh.is_empty());
    assert_eq!(fs::read(dir.0.join(HISTORY_DIR).join(CORRUPT_LOG_FILE)).unwrap(), original);
    let mut fresh = fresh;
    let ops = add_text(&mut fresh, "new", 400);
    store.apply(&ops).unwrap();
    let (_store, reopened) = open(&dir, Protection::Dpapi);
    assert_eq!(reopened.len(), 1);
}

#[test]
fn a_file_that_is_not_a_log_is_moved_aside() {
    let dir = TempDir::new("notalog");
    fs::create_dir_all(dir.path().join(HISTORY_DIR)).unwrap();
    fs::write(dir.log_path(), b"this is definitely not a tuck log").unwrap();
    let (_store, history) = open(&dir, Protection::Plain);
    assert!(history.is_empty());
    assert!(dir.0.join(HISTORY_DIR).join(CORRUPT_LOG_FILE).exists());
    assert_eq!(fs::read(dir.log_path()).unwrap(), b"TUCKLOG1");
}

#[test]
fn torn_header_starts_a_fresh_log() {
    let dir = TempDir::new("header");
    fs::create_dir_all(dir.path().join(HISTORY_DIR)).unwrap();
    fs::write(dir.log_path(), b"TUCK").unwrap();
    let (_store, history) = open(&dir, Protection::Plain);
    assert!(history.is_empty());
    assert_eq!(fs::read(dir.log_path()).unwrap(), b"TUCKLOG1");
    assert!(!dir.0.join(HISTORY_DIR).join(CORRUPT_LOG_FILE).exists());
}

#[test]
fn apply_after_the_log_was_deleted_recreates_it() {
    let dir = TempDir::new("recreate");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    fs::remove_file(dir.log_path()).unwrap();
    let ops = add_text(&mut history, "again", 100);
    store.apply(&ops).unwrap();
    let (_store, reopened) = open(&dir, Protection::Plain);
    assert_eq!(reopened.items(), history.items());
}

#[test]
fn compact_drops_dead_records_and_orphan_blobs() {
    for protection in PROTECTIONS {
        let dir = TempDir::new("compact");
        let (mut store, mut history) = open(&dir, protection);
        let keep_a = add_image(&mut store, &mut history, 10, 100);
        let dropped = add_image(&mut store, &mut history, 20, 200);
        let keep_b = add_image(&mut store, &mut history, 30, 300);
        let ops = add_text(&mut history, "bulky", 400);
        store.apply(&ops).unwrap();
        for step in 0..50 {
            let op = history.touch(4, 500 + step).unwrap();
            store.apply(&[op]).unwrap();
        }
        assert_eq!(dropped, blob_name(2));
        let (_, remove) = history.remove(2).unwrap();
        store.apply(&[remove]).unwrap();
        fs::write(dir.blob_path(&dropped), b"resurrected").unwrap();
        store.put_image("stray", &solid_image(2, 2, [1, 2, 3, 255])).unwrap();
        fs::write(dir.0.join(HISTORY_DIR).join(BLOBS_DIR).join("half-written.tmp"), b"x").unwrap();
        let before = dir.log_len();

        store.compact(&history).unwrap();

        assert!(dir.log_len() < before / 2, "{} vs {before}", dir.log_len());
        assert_eq!(dir.blob_names(), [format!("{keep_a}.bin"), format!("{keep_b}.bin")]);
        assert_eq!(store.get_image(&keep_a).unwrap(), solid_image(4, 3, [10, 10, 10, 255]));
        assert_eq!(store.get_image(&keep_b).unwrap(), solid_image(4, 3, [30, 30, 30, 255]));
        assert!(!dir.0.join(HISTORY_DIR).join(TMP_LOG_FILE).exists());
        let (_store, reopened) = open(&dir, protection);
        assert_eq!(reopened.items(), history.items());
    }
}

#[test]
fn open_compacts_a_bloated_log_and_leaves_a_tight_one_alone() {
    let dir = TempDir::new("autocompact");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    let ops = add_text(&mut history, "only item", 100);
    store.apply(&ops).unwrap();
    for step in 0..200 {
        let op = history.touch(1, 200 + step).unwrap();
        store.apply(&[op]).unwrap();
    }
    let bloated = dir.log_len();

    let (_store, reopened) = open(&dir, Protection::Plain);
    let compacted = dir.log_len();
    assert!(compacted < bloated / 10, "{compacted} vs {bloated}");
    assert_eq!(reopened.items(), history.items());

    let (_store, again) = open(&dir, Protection::Plain);
    assert_eq!(dir.log_len(), compacted);
    assert_eq!(again.items(), history.items());
}

#[test]
fn compaction_thresholds() {
    assert!(!needs_compaction(8, 8));
    assert!(!needs_compaction(1000, 600));
    assert!(needs_compaction(1000, 499));
    assert!(!needs_compaction(COMPACT_LOG_BYTES, COMPACT_LOG_BYTES - 1));
    assert!(!needs_compaction(COMPACT_LOG_BYTES + 100, COMPACT_LOG_BYTES));
    assert!(needs_compaction(COMPACT_LOG_BYTES + 1, COMPACT_LOG_BYTES * 3 / 4));
}

#[test]
fn keep_unpinned_false_drops_unpinned_items_and_their_blobs() {
    for protection in PROTECTIONS {
        let dir = TempDir::new("keep");
        let (mut store, mut history) = open(&dir, protection);
        let ops = add_text(&mut history, "pinned text", 100);
        store.apply(&ops).unwrap();
        let pinned_blob = add_image(&mut store, &mut history, 50, 200);
        add_image(&mut store, &mut history, 60, 300);
        let ops = add_text(&mut history, "loose text", 400);
        store.apply(&ops).unwrap();
        let pin_text = history.set_pinned(1, true).unwrap();
        let pin_image = history.set_pinned(2, true).unwrap();
        store.apply(&[pin_text, pin_image]).unwrap();

        let (_store, kept) = Store::open(dir.path(), protection, false).unwrap();
        assert_eq!(kept.items().iter().map(|item| item.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(dir.blob_names(), [format!("{pinned_blob}.bin")]);

        let (_store, later) = open(&dir, protection);
        assert_eq!(later.items(), kept.items());
        assert_eq!(later.peek_next_id(), 3);
    }
}

#[test]
fn keep_unpinned_true_keeps_everything() {
    let dir = TempDir::new("keepall");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    let ops = add_text(&mut history, "loose", 100);
    store.apply(&ops).unwrap();
    let (_store, reopened) = Store::open(dir.path(), Protection::Plain, true).unwrap();
    assert_eq!(reopened.items(), history.items());
}

#[test]
fn image_round_trip() {
    let mut mixed = Image::new(3, 2);
    mixed.data.copy_from_slice(&[
        10, 20, 30, 255, 40, 50, 60, 128, 70, 80, 90, 0, //
        1, 2, 3, 255, 200, 100, 50, 7, 255, 255, 255, 255,
    ]);
    let opaque = Image::from_bgra(2, 2, vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 250, 251, 252, 255]);
    let single = solid_image(1, 1, [9, 8, 7, 255]);
    for protection in PROTECTIONS {
        let dir = TempDir::new("image");
        let (mut store, _) = open(&dir, protection);
        for (blob, image) in [("mixed", &mixed), ("opaque", &opaque), ("single", &single)] {
            store.put_image(blob, image).unwrap();
            assert_eq!(&store.get_image(blob).unwrap(), image, "{blob} with {protection:?}");
        }
        let raw = fs::read(dir.blob_path("mixed")).unwrap();
        assert_eq!(raw.starts_with(b"\x89PNG"), protection == Protection::Plain);
        assert!(!dir.0.join(HISTORY_DIR).join(BLOBS_DIR).join("mixed.tmp").exists());
    }
}

#[test]
fn put_image_replaces_an_existing_blob() {
    let dir = TempDir::new("replace");
    let (mut store, _) = open(&dir, Protection::Dpapi);
    store.put_image("same", &solid_image(2, 2, [1, 1, 1, 255])).unwrap();
    store.put_image("same", &solid_image(3, 3, [2, 2, 2, 255])).unwrap();
    assert_eq!(store.get_image("same").unwrap(), solid_image(3, 3, [2, 2, 2, 255]));
    assert_eq!(dir.blob_names(), ["same.bin"]);
}

#[test]
fn blob_names_are_validated_and_missing_blobs_fail() {
    let dir = TempDir::new("names");
    let (mut store, _) = open(&dir, Protection::Plain);
    let image = solid_image(1, 1, [0, 0, 0, 255]);
    let too_long = "x".repeat(65);
    for bad in ["", "..", "../escape", "a/b", r"a\b", "dot.ted", "sp ace", too_long.as_str()] {
        assert!(store.put_image(bad, &image).is_err(), "{bad:?}");
        assert!(store.get_image(bad).is_err(), "{bad:?}");
    }
    assert!(store.get_image("missing").is_err());
    assert!(store.put_image("empty", &Image::new(0, 0)).is_err());
    store.put_image(&blob_name(0xabc), &image).unwrap();
    store.put_image("under_score-123", &image).unwrap();
}

#[test]
fn corrupt_blob_is_an_error() {
    let dir = TempDir::new("badblob");
    let (mut store, _) = open(&dir, Protection::Dpapi);
    store.put_image("x", &solid_image(2, 2, [5, 5, 5, 255])).unwrap();
    let mut bytes = fs::read(dir.blob_path("x")).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    fs::write(dir.blob_path("x"), bytes).unwrap();
    assert!(store.get_image("x").is_err());
}

#[test]
fn removing_an_item_deletes_its_blob() {
    let dir = TempDir::new("remove");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    let first = add_image(&mut store, &mut history, 1, 100);
    let second = add_image(&mut store, &mut history, 2, 200);
    assert_eq!(dir.blob_names().len(), 2);
    let (_, remove) = history.remove(1).unwrap();
    store.apply(&[remove]).unwrap();
    assert_eq!(dir.blob_names(), [format!("{second}.bin")]);
    assert!(store.get_image(&first).is_err());
    let (removed, ops) = history.clear_unpinned();
    assert_eq!(removed.len(), 1);
    store.apply(&ops).unwrap();
    assert!(dir.blob_names().is_empty());
}

#[test]
fn wipe_deletes_the_log_and_every_blob_and_the_store_keeps_working() {
    for protection in PROTECTIONS {
        let dir = TempDir::new("wipe");
        let (mut store, mut history) = open(&dir, protection);
        add_image(&mut store, &mut history, 1, 100);
        let ops = add_text(&mut history, "secret", 200);
        store.apply(&ops).unwrap();
        fs::write(dir.0.join(HISTORY_DIR).join(CORRUPT_LOG_FILE), b"old").unwrap();

        store.wipe().unwrap();
        assert!(!dir.log_path().exists());
        assert!(dir.blob_names().is_empty());
        assert!(!dir.0.join(HISTORY_DIR).join(CORRUPT_LOG_FILE).exists());
        store.wipe().unwrap();

        let (mut store, mut fresh) = open(&dir, protection);
        assert!(fresh.is_empty());
        let ops = add_text(&mut fresh, "after", 300);
        store.apply(&ops).unwrap();
        let (_store, reopened) = open(&dir, protection);
        assert_eq!(reopened.len(), 1);
    }
}

#[test]
fn wipe_then_apply_without_reopening_recreates_the_log() {
    let dir = TempDir::new("wipeapply");
    let (mut store, mut history) = open(&dir, Protection::Plain);
    let ops = add_text(&mut history, "before", 100);
    store.apply(&ops).unwrap();
    store.wipe().unwrap();
    let mut history = History::new();
    let ops = add_text(&mut history, "after", 200);
    store.apply(&ops).unwrap();
    let (_store, reopened) = open(&dir, Protection::Plain);
    assert_eq!(reopened.items(), history.items());
}

#[test]
fn blob_name_is_sixteen_lowercase_hex_digits() {
    assert_eq!(blob_name(1), "0000000000000001");
    assert_eq!(blob_name(255), "00000000000000ff");
    assert_eq!(blob_name(0x1234_5678_9abc_def0), "123456789abcdef0");
}

#[test]
fn dpapi_round_trips_and_detects_tampering() {
    for len in [1usize, 15, 1000, 1 << 20] {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let sealed = dpapi::protect(&data).unwrap();
        assert_ne!(sealed, data);
        assert!(sealed.len() > len);
        assert_eq!(dpapi::unprotect(&sealed).unwrap(), data);
    }
    let mut sealed = dpapi::protect(b"hello history").unwrap();
    let middle = sealed.len() / 2;
    sealed[middle] ^= 0x55;
    assert!(dpapi::unprotect(&sealed).is_err());
    assert!(dpapi::unprotect(b"not a dpapi blob").is_err());
}
