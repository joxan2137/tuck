//! `--selftest [--json]` (DESIGN §11): headless checks with no window, hook, clipboard, input or registry: catalog
//! counts and search cases, emoji coverage, the store round trip in a temporary folder with DPAPI, clipboard format
//! round trips, the app's copy and paste pipeline, settings round trip, placement, every preview, and the timing
//! budgets that can be measured headless (enforced in release builds, reported in debug builds).

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use tuck_clip::{Captured, formats};
use tuck_core::clip::text_hash;
use tuck_core::{ClipContent, History, Image, Op, PointI, RectI, Settings, ThemeMode, content_bytes, thumbnail};
use tuck_emoji::{Coverage, Kind, Usage, catalog, search};
use tuck_panel::{Panel, Tab};
use tuck_store::{Protection, Store, blob_name};
use tuck_sys::{Placement, panel_origin};
use tuck_ui::{Ctx, Gfx, OffscreenSpec, Theme, View, render_offscreen};

use crate::clips::{self, RowCache, THUMBNAIL_MAX};
use crate::ico::{IcoEntry, ico_bytes};
use crate::pickers;
use crate::preview::{ICON_SIZES, KINDS, PanelData};
use crate::store::{StoreEvent, StoreJob, StoreThread};

struct Check {
    name: String,
    ok: bool,
    detail: String,
    ms: f64,
}

fn timed(name: &str, f: impl FnOnce() -> Result<String>) -> Check {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let (ok, detail) = match result {
        Ok(Ok(detail)) => (true, detail),
        Ok(Err(error)) => (false, format!("{error:#}")),
        Err(panic) => (false, format!("panicked: {}", panic_message(&panic))),
    };
    Check { name: name.to_string(), ok, detail, ms }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

fn ms_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Fails a measured time over `limit_ms` in release builds; debug builds only report it.
fn within_budget(what: &str, ms: f64, limit_ms: f64) -> Result<String> {
    let verdict = if ms <= limit_ms { "within" } else { "over" };
    if !cfg!(debug_assertions) {
        ensure!(ms <= limit_ms, "{what} took {ms:.2} ms, budget {limit_ms} ms");
    }
    Ok(format!(
        "{what} {ms:.2} ms ({verdict} the {limit_ms} ms budget{})",
        if cfg!(debug_assertions) { ", debug build" } else { "" }
    ))
}

pub fn output_dir() -> PathBuf {
    std::env::temp_dir().join("tuck-selftest")
}

fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tuck-selftest-{name}-{}", std::process::id()))
}

/// Runs every check, prints a table (or JSON) and returns the process exit code.
pub fn run(json_output: bool) -> i32 {
    let out = output_dir();
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {error}", out.display());
        return 2;
    }
    let mut checks = vec![timed("settings round trip", settings_round_trip)];
    let gfx = match Gfx::new() {
        Ok(gfx) => gfx,
        Err(error) => {
            eprintln!("graphics unavailable: {error:#}");
            return 2;
        }
    };
    checks.push(timed("catalogs", catalogs));
    checks.push(timed("search cases", search_cases));
    checks.push(timed("emoji search budget", emoji_search_budget));
    let mut coverage = None;
    checks.push(timed("coverage", || {
        let started = Instant::now();
        let computed = Coverage::compute()?;
        let ms = ms_since(started);
        let total = catalog(Kind::Emoji).entries.len();
        ensure!(computed.supported_count() > total / 2, "only {} of {total} emoji render", computed.supported_count());
        let detail = format!("{} of {total} emoji render as one glyph ({ms:.1} ms)", computed.supported_count());
        coverage = Some(computed);
        Ok(detail)
    }));
    let coverage = coverage.unwrap_or_else(Coverage::all_supported);
    checks.push(timed("picker sections", || picker_sections(&coverage)));
    checks.push(timed("history search budget", history_search_budget));
    checks.push(timed("store round trip (DPAPI)", store_round_trip));
    checks.push(timed("store thread", store_thread));
    checks.push(timed("clipboard formats", clipboard_formats));
    checks.push(timed("copy and paste pipeline", copy_paste_pipeline));
    checks.push(timed("thumbnail budget", thumbnail_budget));
    checks.push(timed("placement", placement));
    checks.push(timed("target lookup", target_lookup));
    checks.push(timed("panel first frame", || panel_first_frame(&gfx)));
    checks.push(timed("keystroke to results", || keystroke_to_results(&gfx)));
    checks.push(timed("icon", || icon(&gfx, &out)));
    for kind in KINDS {
        let path = out.join(format!("{kind}.png"));
        checks.push(timed(&format!("preview {kind}"), || {
            let image = crate::preview::render(&gfx, kind, ThemeMode::Dark, 1.0)?;
            std::fs::write(&path, formats::encode_png(&image)?)?;
            Ok(format!("{}×{} → {}", image.width, image.height, path.display()))
        }));
    }
    report(&checks, &out, json_output);
    if checks.iter().all(|c| c.ok) { 0 } else { 1 }
}

fn report(checks: &[Check], out: &Path, json_output: bool) {
    let passed = checks.iter().filter(|c| c.ok).count();
    if json_output {
        let value = json!({
            "version": crate::VERSION,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "passed": passed == checks.len(),
            "summary": format!("{passed}/{} checks passed", checks.len()),
            "output_dir": out.display().to_string(),
            "checks": checks.iter().map(|c| json!({ "name": c.name, "ok": c.ok, "detail": c.detail, "ms": (c.ms * 10.0).round() / 10.0 })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        return;
    }
    println!("Tuck {} self-test", crate::VERSION);
    for check in checks {
        let status = if check.ok { "PASS" } else { "FAIL" };
        println!("  {status}  {:<28} {:>8.1} ms  {}", check.name, check.ms, check.detail);
    }
    println!("{passed}/{} checks passed; images in {}", checks.len(), out.display());
}

fn settings_round_trip() -> Result<String> {
    let dir = scratch_dir("settings");
    let previous = std::env::var_os("TUCK_DATA_DIR");
    // SAFETY: this is the first check; no other thread exists yet that could read the environment concurrently.
    unsafe { std::env::set_var("TUCK_DATA_DIR", &dir) };
    let result = (|| -> Result<String> {
        let mut settings = Settings { theme: ThemeMode::Light, ..Settings::default() };
        settings.history.max_items = 500;
        settings.history.ignored_apps = vec!["keepass.exe".into()];
        settings.shortcuts.win_period = false;
        tuck_sys::settings_store::save_settings(&settings)?;
        let path = tuck_sys::settings_store::settings_path()?;
        ensure!(path.starts_with(&dir), "settings path {} ignores TUCK_DATA_DIR", path.display());
        if tuck_sys::settings_store::load_settings() != settings {
            bail!("loaded settings differ from saved ones");
        }
        Ok(format!("saved and reloaded {}", path.display()))
    })();
    // SAFETY: as above.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TUCK_DATA_DIR", value),
            None => std::env::remove_var("TUCK_DATA_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn catalogs() -> Result<String> {
    let started = Instant::now();
    let sizes: Vec<(usize, usize)> =
        Kind::ALL.iter().map(|kind| (catalog(*kind).entries.len(), catalog(*kind).groups.len())).collect();
    let ms = ms_since(started);
    let [(emoji, emoji_groups), (kaomoji, kaomoji_groups), (symbols, symbol_groups)] = sizes[..] else {
        bail!("expected three catalogs");
    };
    ensure!(emoji >= 1900 && emoji_groups == 9, "emoji: {emoji} in {emoji_groups} groups");
    ensure!(symbols >= 1400 && symbol_groups == 9, "symbols: {symbols} in {symbol_groups} groups");
    ensure!(kaomoji >= 220 && kaomoji_groups >= 10, "kaomoji: {kaomoji} in {kaomoji_groups} groups");
    let budget = within_budget("parse", ms, 15.0)?;
    Ok(format!(
        "{emoji} emoji / {emoji_groups} groups, {symbols} symbols / {symbol_groups}, {kaomoji} kaomoji / {kaomoji_groups}; {budget}"
    ))
}

fn search_cases() -> Result<String> {
    let usage = Usage::default();
    let cases = [
        (Kind::Emoji, "heart", "❤️"),
        (Kind::Emoji, "thumbs", "👍"),
        (Kind::Symbol, "euro", "€"),
        (Kind::Symbol, "U+20AC", "€"),
        (Kind::Kaomoji, "shrug", "¯\\_(ツ)_/¯"),
    ];
    for (kind, query, expected) in cases {
        let first = search(kind, query, &usage).first().map(|&i| catalog(kind).entries[i as usize].text.as_str());
        ensure!(first == Some(expected), "{query:?} found {first:?}, expected {expected}");
    }
    Ok(format!("{} queries rank the expected entry first", cases.len()))
}

fn emoji_search_budget() -> Result<String> {
    let usage = Usage::default();
    let queries = ["h", "he", "hea", "heart", "cat face", "flag", "smil", "thumbs up", "zzz", "party"];
    let mut worst = 0.0f64;
    for _ in 0..5 {
        for query in queries {
            let started = Instant::now();
            std::hint::black_box(search(Kind::Emoji, query, &usage));
            worst = worst.max(ms_since(started));
        }
    }
    within_budget("slowest of 50 emoji searches", worst, 2.0)
}

fn picker_sections(coverage: &Coverage) -> Result<String> {
    let started = Instant::now();
    let sections: Vec<_> = Kind::ALL.iter().map(|kind| pickers::sections(*kind, coverage, &Usage::default())).collect();
    let ms = ms_since(started);
    let shown: usize = sections[0].iter().map(|s| s.items.len()).sum();
    ensure!(shown == coverage.supported_count(), "{shown} emoji shown, {} supported", coverage.supported_count());
    let hidden: Vec<&str> =
        coverage.unsupported_indices().map(|i| catalog(Kind::Emoji).entries[i as usize].text.as_str()).collect();
    let leaked = sections[0].iter().flat_map(|s| &s.items).filter(|item| hidden.contains(&item.text.as_str())).count();
    ensure!(leaked == 0, "{leaked} unsupported emoji are shown");
    Ok(format!(
        "{} emoji ({} hidden), {} kaomoji, {} symbols in {ms:.1} ms",
        shown,
        hidden.len(),
        sections[1].iter().map(|s| s.items.len()).sum::<usize>(),
        sections[2].iter().map(|s| s.items.len()).sum::<usize>()
    ))
}

/// 1000 varied text items, as a full history would hold.
fn synthetic_history(items: usize) -> History {
    let words = [
        "invoice", "meeting", "panel", "emoji", "deploy", "budget", "café", "naïve", "report", "design", "tuck",
        "glint",
    ];
    let mut history = History::new();
    for i in 0..items {
        let text = format!(
            "{} {} {} item {i}: the quick brown fox jumps over the lazy dog {}",
            words[i % words.len()],
            words[(i * 7 + 3) % words.len()],
            words[(i * 5 + 1) % words.len()],
            "and keeps running ".repeat(i % 6)
        );
        let content = ClipContent::Text { text: text.clone(), html: None, rtf: None };
        let bytes = content_bytes(&content);
        history.add(content, text_hash(&text), bytes, None, 1_000 + i as i64);
    }
    history
}

fn history_search_budget() -> Result<String> {
    let history = synthetic_history(1000);
    let mut worst = 0.0f64;
    for query in ["p", "pa", "pan", "panel", "cafe", "naive report", "zzz", "item 99"] {
        let started = Instant::now();
        std::hint::black_box(history.search(query));
        worst = worst.max(ms_since(started));
    }
    within_budget("slowest search over 1000 items", worst, 3.0)
}

fn sample_image(width: u32, height: u32) -> Image {
    let data = (0..width * height)
        .flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 31) as u8, 128 + (i % 128) as u8])
        .collect();
    Image::from_bgra(width, height, data)
}

fn store_round_trip() -> Result<String> {
    let dir = scratch_dir("store");
    let _ = std::fs::remove_dir_all(&dir);
    let result = (|| -> Result<String> {
        let started = Instant::now();
        let (mut store, mut history) = Store::open(&dir, Protection::Dpapi, true)?;
        let image = sample_image(37, 21);
        let blob = blob_name(history.peek_next_id());
        store.put_image(&blob, &image)?;
        let image_content = ClipContent::Image { blob: blob.clone(), width: image.width, height: image.height };
        let image_bytes = content_bytes(&image_content);
        let (added, mut ops) =
            history.add(image_content, tuck_core::clip::image_hash(&image), image_bytes, None, 1_000);
        let text = ClipContent::Text { text: "pinned note".into(), html: Some("<b>pinned</b>".into()), rtf: None };
        let text_bytes = content_bytes(&text);
        let (pinned, more) = history.add(text, text_hash("pinned note"), text_bytes, None, 2_000);
        ops.extend(more);
        ops.extend(history.set_pinned(pinned.id(), true));
        store.apply(&ops)?;
        drop(store);
        let (store, reopened) = Store::open(&dir, Protection::Dpapi, true)?;
        ensure!(reopened.items() == history.items(), "reopened history differs");
        ensure!(store.get_image(&blob)? == image, "image blob changed");
        drop(store);
        let (mut store, kept) = Store::open(&dir, Protection::Dpapi, false)?;
        ensure!(
            kept.len() == 1 && kept.items()[0].id == pinned.id(),
            "keep_unpinned = false kept {} items",
            kept.len()
        );
        ensure!(store.get_image(&blob).is_err(), "the dropped image's blob is still there");
        store.compact(&kept)?;
        store.apply(&[Op::Remove(pinned.id())])?;
        store.wipe()?;
        Ok(format!(
            "{} items saved, reopened, trimmed to pinned (item {}), compacted, wiped in {:.0} ms",
            history.len(),
            added.id(),
            ms_since(started)
        ))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn store_thread() -> Result<String> {
    let dir = scratch_dir("store-thread");
    let _ = std::fs::remove_dir_all(&dir);
    let result = (|| -> Result<String> {
        let (events, received) = mpsc::channel::<StoreEvent>();
        let mut thread = StoreThread::start(dir.clone(), Protection::Dpapi, true, move |event| {
            let _ = events.send(event);
        })?;
        let wait = Duration::from_secs(5);
        let StoreEvent::Opened(opened) = received.recv_timeout(wait).context("no Opened event")? else {
            bail!("the first event was not Opened");
        };
        ensure!(opened?.is_empty(), "a fresh store is not empty");
        thread.send(StoreJob::PutImage { id: 1, blob: blob_name(1), image: sample_image(1600, 900) });
        let StoreEvent::Thumbnail { id, image } = received.recv_timeout(wait).context("no thumbnail")? else {
            bail!("expected a thumbnail");
        };
        ensure!(
            id == 1 && image.width <= THUMBNAIL_MAX.0 && image.height <= THUMBNAIL_MAX.1,
            "thumbnail {}×{}",
            image.width,
            image.height
        );
        thread.send(StoreJob::LoadImage { ticket: 7, blob: blob_name(1) });
        let StoreEvent::ImageLoaded { ticket, result } = received.recv_timeout(wait).context("no image")? else {
            bail!("expected the loaded image");
        };
        ensure!(ticket == 7 && result?.width == 1600, "wrong image back");
        if let Some(handle) = thread.finish() {
            handle.join().map_err(|_| anyhow::anyhow!("the store thread panicked"))?;
        }
        Ok(format!("opened, put a 1600×900 image, thumbnail {}×{}, read it back", image.width, image.height))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn clipboard_formats() -> Result<String> {
    let image = sample_image(19, 7);
    ensure!(formats::dib_to_image(&formats::image_to_dibv5(&image))? == image, "DIBV5 round trip changed pixels");
    ensure!(formats::decode_png(&formats::encode_png(&image)?)? == image, "PNG round trip changed pixels");
    let paths =
        vec![PathBuf::from(r"C:\Users\you\Documents\Report Q3.xlsx"), PathBuf::from(r"C:\Users\you\Pictures\ü.png")];
    ensure!(formats::parse_hdrop(&formats::build_hdrop(&paths))? == paths, "HDROP round trip changed paths");
    let fragment = "<p>Hello <b>world</b></p>";
    let html = formats::cf_html_from_fragment(fragment);
    ensure!(formats::cf_html_fragment(&html) == Some(fragment), "CF_HTML fragment not recovered");
    let text = "naïve — 😀";
    ensure!(formats::decode_utf16_text(&formats::encode_utf16_text(text)) == text, "UTF-16 text round trip");
    Ok("DIBV5, PNG, HDROP, CF_HTML and UTF-16 text round trip".into())
}

/// A copy becomes a history item and rows; pastes write the right formats (no clipboard involved).
fn copy_paste_pipeline() -> Result<String> {
    let mut history = History::new();
    let captured =
        Captured { text: Some("https://example.com/a".into()), html: Some("<a>x</a>".into()), ..Captured::default() };
    let copied = clips::copied(captured, blob_name(history.peek_next_id())).context("text copy")?;
    let bytes = content_bytes(&copied.content);
    let (added, _) = history.add(copied.content, copied.hash, bytes, None, 1_000);
    let again = Captured { text: Some("https://example.com/a".into()), ..Captured::default() };
    let copied = clips::copied(again, blob_name(history.peek_next_id())).context("repeat copy")?;
    let bytes = content_bytes(&copied.content);
    let (promoted, _) = history.add(copied.content, copied.hash, bytes, None, 2_000);
    ensure!(added.id() == promoted.id() && history.len() == 1, "a repeated copy made a second item");
    let mut rows = RowCache::default();
    let (shown, _) = rows.rows(&history, &history.search(""));
    ensure!(
        shown.len() == 1 && shown[0].kind == tuck_core::TextKind::Url,
        "row kind {:?}",
        shown.first().map(|r| r.kind)
    );
    let item = history.get(added.id()).context("item")?;
    let plain = clips::payload(&item.content, true, None);
    ensure!(plain.text.as_deref() == Some("https://example.com/a") && plain.html.is_none(), "plain paste kept formats");
    Ok("copy → item → row → payload; repeated copy promoted".into())
}

fn thumbnail_budget() -> Result<String> {
    let image = sample_image(1920, 1080);
    let started = Instant::now();
    let small = thumbnail(&image, THUMBNAIL_MAX.0, THUMBNAIL_MAX.1);
    let ms = ms_since(started);
    ensure!(small.height == THUMBNAIL_MAX.1, "thumbnail is {}×{}", small.width, small.height);
    within_budget(&format!("1920×1080 → {}×{}", small.width, small.height), ms, 40.0)
}

fn placement() -> Result<String> {
    let work = RectI::new(0, 0, 1920, 1040);
    let size = (380, 460);
    let below = panel_origin(Some(Placement::Caret(RectI::new(400, 300, 2, 20))), PointI::new(0, 0), size, work, 1.0);
    ensure!(below == PointI::new(376, 326), "caret placement {below:?}");
    let above = panel_origin(Some(Placement::Caret(RectI::new(400, 900, 2, 20))), PointI::new(0, 0), size, work, 1.0);
    ensure!(above.y + size.1 <= 900, "near the bottom the panel goes above the caret: {above:?}");
    let cursor = panel_origin(None, PointI::new(1900, 1030), (570, 690), work, 1.5);
    let inside =
        cursor.x >= work.x && cursor.y >= work.y && cursor.x + 570 <= work.right() && cursor.y + 690 <= work.bottom();
    ensure!(inside, "cursor placement {cursor:?} leaves the work area");
    Ok("below / above the caret, cursor clamped at 150 %".into())
}

/// Read-only: the foreground window, its focus and caret (no hook, no input).
fn target_lookup() -> Result<String> {
    let started = Instant::now();
    let target = tuck_sys::capture_target();
    let ms = ms_since(started);
    let what = target.as_ref().and_then(|t| t.exe_name()).unwrap_or_else(|| "no foreground window".into());
    within_budget(&format!("capture_target ({what})"), ms, 1.0)
}

fn panel_with_history(gfx: &Rc<Gfx>, history: &History, tab: Tab) -> (Panel, Ctx) {
    let mut cx = Ctx::offscreen(gfx.clone(), Panel::SIZE, 1.0, Theme::dark(), 0.0);
    let mut panel = Panel::new(crate::settings_model::panel_options(&Settings::default(), false));
    let mut rows = RowCache::default();
    let (shown, _) = rows.rows(history, &history.search(""));
    panel.set_clips(&mut cx, shown, history.len());
    panel.open(&mut cx, tab);
    (panel, cx)
}

fn paint_ms(gfx: &Rc<Gfx>, panel: &mut Panel) -> Result<f64> {
    let spec = OffscreenSpec::new(Panel::SIZE, 1.0, Theme::dark());
    let mut paint = 0.0;
    render_offscreen(gfx, &spec, |cx, p| {
        let started = Instant::now();
        View::paint(panel, cx, p);
        paint = ms_since(started);
    })?;
    Ok(paint)
}

/// Opening on Clipboard with 200 items: rows, `open` and the first paint, cold and then warm.
fn panel_first_frame(gfx: &Rc<Gfx>) -> Result<String> {
    let history = synthetic_history(200);
    let measure = || -> Result<f64> {
        let started = Instant::now();
        let (mut panel, _) = panel_with_history(gfx, &history, Tab::Clipboard);
        let setup = ms_since(started);
        Ok(setup + paint_ms(gfx, &mut panel)?)
    };
    let cold = measure()?;
    let warm = measure()?;
    let budget = within_budget("rows + open + paint", warm, 16.0)?;
    Ok(format!("{budget}; first ever {cold:.1} ms"))
}

/// One keystroke in the search: search, new rows or sections and the repaint (glyphs already rasterized).
fn keystroke_to_results(gfx: &Rc<Gfx>) -> Result<String> {
    let data = PanelData::new(gfx)?;
    let (mut panel, mut cx) = panel_with_history(gfx, &data.history, Tab::Emoji);
    panel.set_sections(&mut cx, Tab::Emoji, pickers::sections(Kind::Emoji, &data.coverage, &data.usage));
    let emoji_keystroke = |panel: &mut Panel, cx: &mut Ctx| -> Result<f64> {
        let started = Instant::now();
        panel.preview_query("heart");
        panel.set_sections(cx, Tab::Emoji, pickers::search_sections(Kind::Emoji, "heart", &data.coverage, &data.usage));
        let work = ms_since(started);
        Ok(work + paint_ms(gfx, panel)?)
    };
    emoji_keystroke(&mut panel, &mut cx)?;
    let emoji = emoji_keystroke(&mut panel, &mut cx)?;
    let history = synthetic_history(1000);
    let (mut clipboard, mut cx) = panel_with_history(gfx, &history, Tab::Clipboard);
    let mut rows = RowCache::default();
    let clip_keystroke = |panel: &mut Panel, cx: &mut Ctx, rows: &mut RowCache| -> Result<f64> {
        let started = Instant::now();
        panel.preview_query("panel");
        let (found, _) = rows.rows(&history, &history.search("panel"));
        panel.set_clips(cx, found, history.len());
        let work = ms_since(started);
        Ok(work + paint_ms(gfx, panel)?)
    };
    clip_keystroke(&mut clipboard, &mut cx, &mut rows)?;
    let clip = clip_keystroke(&mut clipboard, &mut cx, &mut rows)?;
    let emoji_budget = within_budget("emoji \"heart\"", emoji, 8.0)?;
    let clip_budget = within_budget("clipboard \"panel\" over 1000 items", clip, 8.0)?;
    Ok(format!("{emoji_budget}; {clip_budget}"))
}

fn icon(gfx: &Rc<Gfx>, out: &Path) -> Result<String> {
    let mut entries = Vec::new();
    for size in ICON_SIZES {
        let image = crate::art::render_app_icon(gfx, size)?;
        ensure!(image.width == size && image.data.chunks_exact(4).any(|p| p[3] == 255), "{size} px icon is empty");
        entries.push(IcoEntry { size, png: formats::encode_png(&image)? });
    }
    let path = out.join("tuck.ico");
    std::fs::write(&path, ico_bytes(&entries))?;
    Ok(format!("{} sizes → {}", entries.len(), path.display()))
}
