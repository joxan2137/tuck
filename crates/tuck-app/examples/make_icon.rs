//! Draws the Tuck app icon with tuck-ui offscreen and writes `res/tuck.ico` (PNG entries at every Windows size)
//! plus `res/tuck-256.png`. No window is created.
//!
//! cargo run -p tuck-app --example make_icon [-- --out <dir>]

use std::path::PathBuf;

use anyhow::{Context, Result};
use tuck_app::art::render_app_icon;
use tuck_app::ico::{IcoEntry, ico_bytes};
use tuck_clip::formats::encode_png;
use tuck_ui::Gfx;

const SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

fn output_dir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("res"))
}

fn main() -> Result<()> {
    tuck_ui::enable_per_monitor_dpi_awareness();
    let out = output_dir();
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let gfx = Gfx::new()?;
    let mut entries = Vec::new();
    for size in SIZES {
        let png = encode_png(&render_app_icon(&gfx, size)?)?;
        if size == 256 {
            let path = out.join("tuck-256.png");
            std::fs::write(&path, &png).with_context(|| format!("writing {}", path.display()))?;
        }
        entries.push(IcoEntry { size, png });
    }
    let ico = out.join("tuck.ico");
    std::fs::write(&ico, ico_bytes(&entries)).with_context(|| format!("writing {}", ico.display()))?;
    println!("wrote {} and tuck-256.png", ico.display());
    Ok(())
}
