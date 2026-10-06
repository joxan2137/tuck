//! Generates crates/tuck-emoji/data from Unicode and CLDR source files. See DESIGN.md §6.

mod emoji;
mod fetch;
mod symbols;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let mut fetch_first = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--fetch" => fetch_first = true,
            other => bail!("unknown argument {other:?}; usage: gen-data [--fetch]"),
        }
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.ancestors().nth(2).context("gen-data is not inside the workspace")?;
    let cache_dir = manifest_dir.join("cache");
    let data_dir = workspace_root.join("crates/tuck-emoji/data");

    if fetch_first {
        fetch::download_sources(&cache_dir)?;
    }
    let read = |name: &str| {
        fs::read_to_string(cache_dir.join(name))
            .with_context(|| format!("reading {name} from {} (run with --fetch)", cache_dir.display()))
    };
    let emoji_test = read("emoji-test.txt")?;
    let unicode_data = read("UnicodeData.txt")?;
    let annotations = emoji::Annotations::parse(&read("annotations.json")?, &read("annotationsDerived.json")?)?;

    fs::create_dir_all(&data_dir)?;
    let emoji_rows = emoji::build_rows(&emoji_test, &annotations)?;
    let toned_rows = emoji_rows.iter().filter(|row| !row.toned.is_empty()).count();
    fs::write(data_dir.join("emoji.tsv"), emoji::to_tsv(&emoji_rows))?;
    println!("emoji.tsv: {} rows, {toned_rows} with skin tones", emoji_rows.len());

    let symbol_rows = symbols::build_rows(&unicode_data, &emoji_test)?;
    fs::write(data_dir.join("symbols.tsv"), symbols::to_tsv(&symbol_rows))?;
    println!("symbols.tsv: {} rows", symbol_rows.len());

    fs::copy(cache_dir.join("LICENSE-UNICODE.txt"), data_dir.join("LICENSE-UNICODE.txt"))
        .context("copying LICENSE-UNICODE.txt")?;
    println!("wrote {}", data_dir.display());
    Ok(())
}
