//! Persistent clipboard history: encrypted append-only log plus image blobs. See DESIGN.md §5.

mod blob;
mod dpapi;
mod records;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tuck_core::{ClipContent, ClipId, ClipItem, History, Image, Op};

use records::{MAGIC, Record};

const HISTORY_DIR: &str = "history";
const BLOBS_DIR: &str = "blobs";
const LOG_FILE: &str = "history.tlog";
const TMP_LOG_FILE: &str = "history.tlog.tmp";
const CORRUPT_LOG_FILE: &str = "history.tlog.corrupt";
const BLOB_EXTENSION: &str = "bin";
const TMP_BLOB_EXTENSION: &str = "tmp";
const MAX_BLOB_NAME_LEN: usize = 64;
const COMPACT_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    Dpapi,
    /// Unencrypted; only for tests.
    Plain,
}

impl Protection {
    fn protect(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Dpapi => dpapi::protect(data),
            Self::Plain => Ok(data.to_vec()),
        }
    }

    fn unprotect(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Dpapi => dpapi::unprotect(data),
            Self::Plain => Ok(data.to_vec()),
        }
    }
}

/// The store is single-threaded: the app owns it on a store thread.
pub struct Store {
    root: PathBuf,
    protection: Protection,
}

/// Blob name the app gives an image item's pixels.
pub fn blob_name(id: ClipId) -> String {
    format!("{id:016x}")
}

impl Store {
    /// Opens (creating if needed) the store under `dir` and replays the log into a `History`. A torn last record
    /// is dropped, `keep_unpinned = false` discards unpinned items, and a log that is mostly dead weight is
    /// compacted.
    pub fn open(dir: &Path, protection: Protection, keep_unpinned: bool) -> Result<(Store, History)> {
        let root = dir.join(HISTORY_DIR);
        fs::create_dir_all(root.join(BLOBS_DIR)).with_context(|| format!("creating {}", root.display()))?;
        let mut store = Store { root, protection };
        let loaded = store.read_log()?;
        let log_len = loaded.valid_len;
        let mut items = loaded.items;
        let dropped_unpinned = !keep_unpinned && items.iter().any(|item| !item.pinned);
        if !keep_unpinned {
            items.retain(|item| item.pinned);
        }
        let history = History::from_items(items);
        if dropped_unpinned || loaded.skipped > 0 || needs_compaction(log_len, loaded.live_len) {
            store.compact(&history)?;
        }
        log::info!("history opened: {} items, log was {log_len} bytes", history.len());
        Ok((store, history))
    }

    /// Appends the ops to the log and flushes. Removing an item also deletes its image blob.
    pub fn apply(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut batch = Vec::new();
        for op in ops {
            batch.extend(records::frame(self.protection, &Record::from(op))?);
        }
        self.append_to_log(&batch)?;
        for op in ops {
            if let Op::Remove(id) = op {
                self.remove_blob_quietly(&blob_name(*id));
            }
        }
        Ok(())
    }

    /// Stores `image` as a protected PNG under `blob`, replacing any earlier blob of that name.
    pub fn put_image(&mut self, blob: &str, image: &Image) -> Result<()> {
        let path = self.blob_path(blob)?;
        let sealed = self.protection.protect(&blob::encode_png(image)?)?;
        fs::create_dir_all(self.blobs_dir())?;
        let temp = path.with_extension(TMP_BLOB_EXTENSION);
        fs::write(&temp, sealed).with_context(|| format!("writing blob {blob}"))?;
        fs::rename(&temp, &path).with_context(|| format!("publishing blob {blob}"))?;
        Ok(())
    }

    pub fn get_image(&self, blob: &str) -> Result<Image> {
        let sealed = fs::read(self.blob_path(blob)?).with_context(|| format!("reading blob {blob}"))?;
        blob::decode_png(&self.protection.unprotect(&sealed)?)
    }

    /// Rewrites the log with only `history`'s items (atomic rename) and deletes blobs no item references. Pass a
    /// history that already contains every item whose blob has been put.
    pub fn compact(&mut self, history: &History) -> Result<()> {
        let before = fs::metadata(self.log_path()).map_or(0, |meta| meta.len());
        let after = self.write_snapshot(history.items())?;
        self.delete_orphan_blobs(history.items())?;
        log::info!("history log compacted: {before} -> {after} bytes");
        Ok(())
    }

    /// Deletes the log and every blob.
    pub fn wipe(&mut self) -> Result<()> {
        let mut paths = vec![self.log_path(), self.root.join(TMP_LOG_FILE), self.root.join(CORRUPT_LOG_FILE)];
        paths.extend(self.blob_files()?);
        remove_all(&paths)
    }

    fn log_path(&self) -> PathBuf {
        self.root.join(LOG_FILE)
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root.join(BLOBS_DIR)
    }

    fn blob_path(&self, blob: &str) -> Result<PathBuf> {
        let valid = !blob.is_empty()
            && blob.len() <= MAX_BLOB_NAME_LEN
            && blob.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            bail!("invalid blob name {blob:?}");
        }
        Ok(self.blobs_dir().join(format!("{blob}.{BLOB_EXTENSION}")))
    }

    fn blob_files(&self) -> Result<Vec<PathBuf>> {
        let entries = match fs::read_dir(self.blobs_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("listing blobs"),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.context("listing blobs")?.path();
            if path.is_file() {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn remove_blob_quietly(&self, blob: &str) {
        let Ok(path) = self.blob_path(blob) else { return };
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("could not delete blob {blob}: {error}"),
        }
    }

    /// Replays the log, creating it when absent. A log that is not ours, or whose records are all undecodable
    /// (e.g. another user's), is moved aside and replaced by an empty one instead of being destroyed.
    fn read_log(&self) -> Result<records::Loaded> {
        let bytes = match fs::read(self.log_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error).context("reading the history log"),
        };
        if !bytes.starts_with(MAGIC) {
            let torn_header = MAGIC.starts_with(&bytes);
            if !torn_header {
                self.move_log_aside()?;
            }
            self.reset_log()?;
            return Ok(records::Loaded::default());
        }
        let loaded = records::load(&bytes, self.protection);
        if loaded.readable == 0 && loaded.undecodable > 0 {
            self.move_log_aside()?;
            self.reset_log()?;
            return Ok(records::Loaded::default());
        }
        if (loaded.valid_len as usize) < bytes.len() {
            log::warn!("dropping {} torn bytes from the history log", bytes.len() - loaded.valid_len as usize);
            self.truncate_log(loaded.valid_len)?;
        }
        Ok(loaded)
    }

    fn move_log_aside(&self) -> Result<()> {
        log::warn!("history log is unreadable; moving it to {CORRUPT_LOG_FILE}");
        fs::rename(self.log_path(), self.root.join(CORRUPT_LOG_FILE)).context("moving the unreadable log aside")
    }

    fn reset_log(&self) -> Result<()> {
        fs::write(self.log_path(), MAGIC).context("creating the history log")
    }

    fn truncate_log(&self, len: u64) -> Result<()> {
        let file = OpenOptions::new().write(true).open(self.log_path()).context("opening the history log")?;
        file.set_len(len).context("truncating the history log")
    }

    fn append_to_log(&self, records: &[u8]) -> Result<()> {
        let mut file =
            OpenOptions::new().create(true).append(true).open(self.log_path()).context("opening the history log")?;
        let start = file.metadata().context("reading the history log size")?.len();
        let mut batch = Vec::with_capacity(MAGIC.len() + records.len());
        if start == 0 {
            batch.extend_from_slice(MAGIC);
        }
        batch.extend_from_slice(records);
        let written = file.write_all(&batch).and_then(|()| file.flush());
        if let Err(error) = written {
            drop(file);
            if let Err(truncate_error) = self.truncate_log(start) {
                log::warn!("could not roll back a failed log append: {truncate_error:#}");
            }
            return Err(error).context("appending to the history log");
        }
        Ok(())
    }

    /// Writes the items oldest first to a temp file and renames it over the log; returns the new log size.
    fn write_snapshot(&self, items: &[ClipItem]) -> Result<u64> {
        let temp = self.root.join(TMP_LOG_FILE);
        let mut writer = BufWriter::new(File::create(&temp).context("creating the compacted log")?);
        writer.write_all(MAGIC)?;
        for item in items.iter().rev() {
            writer.write_all(&records::frame(self.protection, &Record::Add(item.clone()))?)?;
        }
        let file = writer.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        let len = file.metadata()?.len();
        drop(file);
        fs::rename(&temp, self.log_path()).context("replacing the history log")?;
        Ok(len)
    }

    fn delete_orphan_blobs(&self, items: &[ClipItem]) -> Result<()> {
        let referenced: HashSet<&str> = items
            .iter()
            .filter_map(|item| match &item.content {
                ClipContent::Image { blob, .. } => Some(blob.as_str()),
                _ => None,
            })
            .collect();
        let orphans: Vec<PathBuf> = self
            .blob_files()?
            .into_iter()
            .filter(|path| {
                let is_blob = path.extension() == Some(OsStr::new(BLOB_EXTENSION));
                let stem = path.file_stem().and_then(OsStr::to_str);
                !(is_blob && stem.is_some_and(|stem| referenced.contains(stem)))
            })
            .collect();
        remove_all(&orphans)
    }
}

/// A log is rewritten when it holds more than twice what its live items need, or is over 8 MB with a quarter
/// of it dead.
fn needs_compaction(log_len: u64, live_len: u64) -> bool {
    log_len > 2 * live_len || (log_len > COMPACT_LOG_BYTES && log_len > live_len + live_len / 4)
}

/// Deletes every path (missing ones are fine), then reports the first failure.
fn remove_all(paths: &[PathBuf]) -> Result<()> {
    let mut first_error = None;
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert(anyhow::Error::new(error).context(format!("deleting {}", path.display())));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
