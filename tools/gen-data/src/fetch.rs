//! Downloads the pinned sources with curl.exe (shipped with Windows 10+).

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

const CLDR_JSON: &str = "https://raw.githubusercontent.com/unicode-org/cldr-json/48.2.3/cldr-json";

fn sources() -> [(&'static str, String); 5] {
    [
        ("emoji-test.txt", "https://www.unicode.org/Public/18.0.0/emoji/emoji-test.txt".to_string()),
        ("UnicodeData.txt", "https://www.unicode.org/Public/18.0.0/ucd/UnicodeData.txt".to_string()),
        ("annotations.json", format!("{CLDR_JSON}/cldr-annotations-full/annotations/en/annotations.json")),
        (
            "annotationsDerived.json",
            format!("{CLDR_JSON}/cldr-annotations-derived-full/annotationsDerived/en/annotations.json"),
        ),
        ("LICENSE-UNICODE.txt", "https://www.unicode.org/license.txt".to_string()),
    ]
}

pub fn download_sources(cache_dir: &Path) -> Result<()> {
    fs::create_dir_all(cache_dir)?;
    for (name, url) in sources() {
        println!("fetching {url}");
        download(&url, &cache_dir.join(name))?;
    }
    Ok(())
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let partial = destination.with_extension("part");
    let status = Command::new("curl.exe")
        .args(["--fail", "--silent", "--show-error", "--location", "--retry", "3", "--output"])
        .arg(&partial)
        .arg(url)
        .status()
        .context("starting curl.exe")?;
    if !status.success() {
        bail!("curl.exe failed for {url}: {status}");
    }
    fs::rename(&partial, destination).with_context(|| format!("moving download to {}", destination.display()))
}
