//! Prints how many emoji the installed Segoe UI Emoji draws as one glyph and the newest ones it cannot. No window.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use tuck_emoji::{Coverage, Entry, Kind, catalog};

const NEWEST_SHOWN: usize = 12;

fn main() -> Result<()> {
    let started = Instant::now();
    let coverage = Coverage::compute()?;
    let elapsed = started.elapsed();

    let entries = &catalog(Kind::Emoji).entries;
    let mut unsupported: Vec<&Entry> = coverage.unsupported_indices().map(|index| &entries[index as usize]).collect();
    println!("emoji entries: {}", entries.len());
    println!("supported:     {}", coverage.supported_count());
    println!("unsupported:   {}", unsupported.len());
    println!("time taken:    {:.1} ms", elapsed.as_secs_f64() * 1000.0);

    let mut by_version: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &unsupported {
        *by_version.entry(format!("{:.1}", entry.version)).or_default() += 1;
    }
    println!("unsupported by emoji version: {by_version:?}");

    unsupported.sort_by(|a, b| b.version.total_cmp(&a.version));
    println!("newest unsupported:");
    for entry in unsupported.iter().take(NEWEST_SHOWN) {
        let code_points: Vec<String> = entry.text.chars().map(|c| format!("{:04X}", c as u32)).collect();
        println!("  E{:.1}  {}  {}  ({})", entry.version, entry.text, entry.name, code_points.join(" "));
    }
    Ok(())
}
