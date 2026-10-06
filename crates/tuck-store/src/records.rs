//! The log's record format: `[u32 LE length][payload]`, payload = protected JSON of one `Record`.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tuck_core::history::content_bytes;
use tuck_core::{ClipContent, ClipId, ClipItem, Op, SourceApp};

use crate::Protection;

pub const MAGIC: &[u8; 8] = b"TUCKLOG1";

const LENGTH_PREFIX: usize = 4;

/// Serde mirror of `tuck_core::Op`.
#[derive(Serialize, Deserialize)]
pub enum Record {
    Add(ClipItem),
    Touch { id: ClipId, used_ms: i64, content: Option<ClipContent>, source: Option<SourceApp> },
    Pin { id: ClipId, pinned: bool },
    Remove(ClipId),
}

impl From<&Op> for Record {
    fn from(op: &Op) -> Self {
        match op {
            Op::Add(item) => Self::Add(item.clone()),
            Op::Touch { id, used_ms, content, source } => {
                Self::Touch { id: *id, used_ms: *used_ms, content: content.clone(), source: source.clone() }
            }
            Op::Pin { id, pinned } => Self::Pin { id: *id, pinned: *pinned },
            Op::Remove(id) => Self::Remove(*id),
        }
    }
}

/// One record as it appears in the file: length prefix and protected payload.
pub fn frame(protection: Protection, record: &Record) -> Result<Vec<u8>> {
    let payload = protection.protect(&serde_json::to_vec(record).context("serializing a history record")?)?;
    let Ok(len) = u32::try_from(payload.len()) else {
        bail!("history record of {} bytes is too large", payload.len());
    };
    let mut framed = Vec::with_capacity(LENGTH_PREFIX + payload.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

fn decode(protection: Protection, payload: &[u8]) -> Result<Record> {
    let json = protection.unprotect(payload)?;
    serde_json::from_slice(&json).context("parsing a history record")
}

/// What replaying a log produced.
#[derive(Default)]
pub struct Loaded {
    pub items: Vec<ClipItem>,
    /// Size the log would have if it held only the live items.
    pub live_len: u64,
    /// Bytes up to the end of the last decodable record; anything after it is a torn or damaged tail.
    pub valid_len: u64,
    pub readable: usize,
    /// Complete records that could not be decoded, anywhere in the log.
    pub undecodable: usize,
    /// Undecodable records followed by a decodable one; these are skipped, not truncated.
    pub skipped: usize,
}

/// Replays `bytes` (which start with `MAGIC`). Undecodable records between decodable ones are skipped; an
/// incomplete or undecodable tail ends the replay.
pub fn load(bytes: &[u8], protection: Protection) -> Loaded {
    let mut items: BTreeMap<ClipId, ClipItem> = BTreeMap::new();
    let mut live_sizes: BTreeMap<ClipId, u64> = BTreeMap::new();
    let (mut readable, mut undecodable, mut skipped, mut undecodable_run) = (0, 0, 0, 0);
    let mut valid_len = MAGIC.len();
    let mut position = MAGIC.len();
    while let Some(payload_start) = position.checked_add(LENGTH_PREFIX).filter(|&start| start <= bytes.len()) {
        let length =
            u32::from_le_bytes([bytes[position], bytes[position + 1], bytes[position + 2], bytes[position + 3]]);
        let end = payload_start + length as usize;
        if end > bytes.len() {
            break;
        }
        let record_len = (end - position) as u64;
        match decode(protection, &bytes[payload_start..end]) {
            Ok(record) => {
                readable += 1;
                skipped += undecodable_run;
                undecodable_run = 0;
                valid_len = end;
                apply(&mut items, &mut live_sizes, record, record_len);
            }
            Err(error) => {
                undecodable += 1;
                undecodable_run += 1;
                log::warn!("undecodable history record: {error:#}");
            }
        }
        position = end;
    }
    Loaded {
        live_len: MAGIC.len() as u64 + live_sizes.values().sum::<u64>(),
        items: items.into_values().collect(),
        valid_len: valid_len as u64,
        readable,
        undecodable,
        skipped,
    }
}

fn apply(items: &mut BTreeMap<ClipId, ClipItem>, live_sizes: &mut BTreeMap<ClipId, u64>, record: Record, size: u64) {
    match record {
        Record::Add(item) => {
            live_sizes.insert(item.id, size);
            items.insert(item.id, item);
        }
        Record::Touch { id, used_ms, content, source } => {
            let Some(item) = items.get_mut(&id) else { return };
            item.used_ms = used_ms;
            if let Some(content) = content {
                item.bytes = content_bytes(&content);
                item.content = content;
                live_sizes.insert(id, size);
            }
            if source.is_some() {
                item.source = source;
            }
        }
        Record::Pin { id, pinned } => {
            if let Some(item) = items.get_mut(&id) {
                item.pinned = pinned;
            }
        }
        Record::Remove(id) => {
            items.remove(&id);
            live_sizes.remove(&id);
        }
    }
}
