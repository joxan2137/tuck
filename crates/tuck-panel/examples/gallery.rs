//! Offscreen renders of every panel state (DESIGN §7, §11) with synthetic data, dark and light, at scale 1 and 2,
//! over a synthetic blurred wallpaper standing in for the DWM acrylic. Prints paint timings. No window is created.
//!
//! cargo run -p tuck-panel --release --example gallery -- --out <dir>

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tuck_core::{ClipContent, ClipId, ClipItem, SkinTone, TextKind, classify};
use tuck_panel::{ClipRow, Clock, Panel, PanelOptions, PickerItem, PickerSection, Tab};
use tuck_ui::{
    Bitmap, Brush, Color, Ctx, Gfx, Icon, Image, Interpolation, OffscreenSpec, Painter, PointF, RectF, Shadow, SizeF,
    StrokeStyle, TextStyle, Theme, Weight, render_offscreen,
};

const MARGIN: f32 = 32.0;
const NOW_MS: i64 = 1_791_297_000_000;
const UTC_OFFSET: i32 = 120;
const MINUTE: i64 = 60_000;

fn main() -> Result<()> {
    tuck_ui::enable_per_monitor_dpi_awareness();
    let out = output_dir();
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let gfx = Gfx::new()?;
    println!("gpu: {}", if gfx.is_software() { "WARP (software)" } else { "hardware" });
    let data = Data::new(&gfx)?;
    let mut paint_times: Vec<(String, Duration)> = Vec::new();
    for scale in [1.0f32, 2.0] {
        for theme in [Theme::dark(), Theme::light()] {
            let canvas = Canvas::new(&gfx, &theme, scale)?;
            for state in STATES {
                let name = format!("{}-{}@{scale}x.png", state.name, if theme.is_dark() { "dark" } else { "light" });
                let (image, paint) = render_state(&gfx, &data, &canvas, &theme, scale, state)?;
                save(&out, &name, &image)?;
                paint_times.push((name, paint));
            }
        }
    }
    let closeup = Canvas::new(&gfx, &Theme::dark(), 4.0)?;
    let emoji_state = STATES.iter().find(|s| s.name == "emoji").expect("emoji state");
    save(&out, "emoji-dark@4x.png", &render_state(&gfx, &data, &closeup, &Theme::dark(), 4.0, emoji_state)?.0)?;
    report_timings(&gfx, &data, &paint_times)?;
    println!("wrote {}", out.display());
    Ok(())
}

fn output_dir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("tuck-panel-gallery"))
}

fn save(dir: &Path, name: &str, image: &Image) -> Result<()> {
    let path = dir.join(name);
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let rgba: Vec<u8> = image.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    writer.write_image_data(&rgba)?;
    Ok(())
}

struct State {
    name: &'static str,
    tab: Tab,
    setup: fn(&mut Panel, &mut Ctx, &Gfx, &Data),
}

const STATES: &[State] = &[
    State { name: "clipboard", tab: Tab::Clipboard, setup: clipboard_default },
    State { name: "clipboard-ctrl", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        clipboard_default(panel, cx, gfx, data);
        panel.preview_hover_card(None);
        panel.preview_ctrl_held(true);
    } },
    State { name: "clipboard-menu", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        clipboard_default(panel, cx, gfx, data);
        panel.preview_scroll(gfx, Tab::Clipboard, 380.0);
        panel.preview_hover_card(Some(6));
        panel.preview_menu(gfx, 6, PointF::new(120.0, 168.0), Some(5));
    } },
    State { name: "clipboard-scrolled", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        clipboard_default(panel, cx, gfx, data);
        panel.preview_hover_card(None);
        panel.preview_select_card(6);
        panel.preview_scroll(gfx, Tab::Clipboard, 470.0);
    } },
    State { name: "clipboard-end", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        clipboard_default(panel, cx, gfx, data);
        panel.preview_hover_card(Some(9));
        panel.preview_select_card(10);
        panel.preview_scroll(gfx, Tab::Clipboard, 10_000.0);
    } },
    State { name: "clipboard-paused", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        panel.set_options(cx, PanelOptions { paused: true, ..options() });
        clipboard_default(panel, cx, gfx, data);
        panel.preview_hover_card(None);
    } },
    State { name: "clipboard-clear", tab: Tab::Clipboard, setup: |panel, cx, gfx, data| {
        clipboard_default(panel, cx, gfx, data);
        panel.preview_hover_card(None);
        panel.preview_confirm_clear();
    } },
    State { name: "clipboard-empty", tab: Tab::Clipboard, setup: |panel, cx, _, _| {
        panel.set_clips(cx, Vec::new(), 0);
    } },
    State { name: "clipboard-no-results", tab: Tab::Clipboard, setup: |panel, cx, _, _| {
        panel.preview_query("zebra crossing");
        panel.set_clips(cx, Vec::new(), 10);
    } },
    State { name: "emoji", tab: Tab::Emoji, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Emoji, data.emoji.clone());
        panel.preview_hover_cell(gfx, Tab::Emoji, "🥹", true);
    } },
    State { name: "emoji-tones", tab: Tab::Emoji, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Emoji, data.emoji.clone());
        panel.preview_scroll(gfx, Tab::Emoji, 330.0);
        panel.preview_tones(gfx, "👋", Some(3));
    } },
    State { name: "emoji-skin-tone", tab: Tab::Emoji, setup: |panel, cx, gfx, data| {
        panel.set_options(cx, PanelOptions { skin_tone: SkinTone::MediumLight, ..options() });
        panel.set_sections(cx, Tab::Emoji, data.emoji.clone());
        panel.preview_scroll(gfx, Tab::Emoji, 330.0);
        panel.preview_skin_picker(gfx);
    } },
    State { name: "emoji-scrolled", tab: Tab::Emoji, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Emoji, data.emoji.clone());
        panel.preview_scroll(gfx, Tab::Emoji, 560.0);
        panel.preview_select_cell(gfx, Tab::Emoji, 75);
    } },
    State { name: "emoji-search", tab: Tab::Emoji, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Emoji, data.emoji.clone());
        panel.preview_query("heart");
        panel.set_sections(cx, Tab::Emoji, data.emoji_results.clone());
        panel.preview_hover_cell(gfx, Tab::Emoji, "💜", true);
    } },
    State { name: "kaomoji", tab: Tab::Kaomoji, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Kaomoji, data.kaomoji.clone());
        panel.preview_hover_cell(gfx, Tab::Kaomoji, "(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧", false);
    } },
    State { name: "symbols", tab: Tab::Symbols, setup: |panel, cx, gfx, data| {
        panel.set_sections(cx, Tab::Symbols, data.symbols.clone());
        panel.preview_hover_cell(gfx, Tab::Symbols, "→", true);
    } },
];

fn options() -> PanelOptions {
    PanelOptions { skin_tone: SkinTone::Default, close_after_click: true, plain_by_default: false, glint_available: true, paused: false }
}

fn clipboard_default(panel: &mut Panel, cx: &mut Ctx, _gfx: &Gfx, data: &Data) {
    panel.set_clips(cx, data.clips.clone(), 48);
    for (id, bitmap) in &data.thumbnails {
        panel.set_thumbnail(cx, *id, bitmap.clone());
    }
    panel.preview_select_card(3);
    panel.preview_hover_card(Some(4));
}

fn build_panel(gfx: &Rc<Gfx>, data: &Data, theme: &Theme, scale: f32, state: &State) -> Panel {
    let mut cx = Ctx::offscreen(gfx.clone(), Panel::SIZE, scale, theme.clone(), 0.0);
    let mut panel = Panel::new(options());
    panel.set_clock(Some(Clock { now_ms: NOW_MS, utc_offset_minutes: UTC_OFFSET }));
    panel.open(&mut cx, state.tab);
    panel.preview_caret(Some(true));
    (state.setup)(&mut panel, &mut cx, gfx, data);
    panel
}

/// Renders the panel alone (timed) and composites it over the wallpaper canvas.
fn render_state(gfx: &Rc<Gfx>, data: &Data, canvas: &Canvas, theme: &Theme, scale: f32, state: &State) -> Result<(Image, Duration)> {
    let mut panel = build_panel(gfx, data, theme, scale, state);
    let (image, paint) = render_panel(gfx, &mut panel, theme, scale)?;
    let panel_bitmap = Bitmap::new(image);
    let size = canvas.size;
    let spec = OffscreenSpec::new(size, scale, theme.clone());
    let composite = render_offscreen(gfx, &spec, |_, p| {
        canvas.paint_desktop(p);
        let rect = RectF::new(MARGIN, MARGIN, Panel::SIZE.w, Panel::SIZE.h);
        p.bitmap(&panel_bitmap, rect, None, 1.0, Interpolation::Linear);
    })?;
    Ok((composite, paint))
}

fn render_panel(gfx: &Rc<Gfx>, panel: &mut Panel, theme: &Theme, scale: f32) -> Result<(Image, Duration)> {
    let spec = OffscreenSpec::new(Panel::SIZE, scale, theme.clone());
    let mut paint = Duration::ZERO;
    let image = render_offscreen(gfx, &spec, |cx, p| {
        let start = Instant::now();
        tuck_ui::View::paint(panel, cx, p);
        paint = start.elapsed();
    })?;
    Ok((image, paint))
}

/// Paint timings: every state once (cold caches), then the emoji tab warm at scale 2 while scrolling.
fn report_timings(gfx: &Rc<Gfx>, data: &Data, first_paints: &[(String, Duration)]) -> Result<()> {
    println!("first paint per state (includes text shaping and emoji rasterization):");
    for (name, time) in first_paints {
        println!("  {name:44} {:6.2} ms", time.as_secs_f64() * 1000.0);
    }
    let theme = Theme::dark();
    let emoji = STATES.iter().find(|s| s.name == "emoji").expect("emoji state");
    let mut panel = build_panel(gfx, data, &theme, 2.0, emoji);
    let mut warm = Vec::new();
    let mut totals = Vec::new();
    for frame in 0..40 {
        let offset = (frame % 20) as f32 * 9.0;
        panel.preview_scroll(gfx, Tab::Emoji, offset);
        let start = Instant::now();
        let (_, paint) = render_panel(gfx, &mut panel, &theme, 2.0)?;
        let total = start.elapsed();
        if frame >= 20 {
            warm.push(paint);
            totals.push(total);
        }
    }
    warm.sort();
    totals.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "emoji tab @2x warm paint (CPU record): median {:.2} ms, max {:.2} ms; with GPU flush + readback: median {:.2} ms",
        ms(warm[warm.len() / 2]),
        ms(*warm.last().unwrap()),
        ms(totals[totals.len() / 2])
    );
    println!("emoji atlas entries: {}", gfx.emoji_cache_len());
    Ok(())
}

/// The synthetic desktop behind the panel: a wallpaper, and the acrylic stand-in (blurred, tinted wallpaper) under
/// the panel rect.
struct Canvas {
    size: SizeF,
    wallpaper: Rc<Bitmap>,
    blurred: Rc<Bitmap>,
    dark: bool,
}

impl Canvas {
    fn new(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Self> {
        let size = SizeF::new(Panel::SIZE.w + 2.0 * MARGIN, Panel::SIZE.h + 2.0 * MARGIN);
        let spec = OffscreenSpec::new(size, scale, theme.clone());
        let wallpaper = Bitmap::new(render_offscreen(gfx, &spec, |_, p| paint_wallpaper(p, size, theme.is_dark()))?);
        let blurred = wallpaper.blurred(30.0 * scale, 1.4);
        Ok(Self { size, wallpaper, blurred, dark: theme.is_dark() })
    }

    fn paint_desktop(&self, p: &mut Painter) {
        let full = RectF::new(0.0, 0.0, self.size.w, self.size.h);
        p.bitmap(&self.wallpaper, full, None, 1.0, Interpolation::Linear);
        let panel = RectF::new(MARGIN, MARGIN, Panel::SIZE.w, Panel::SIZE.h);
        p.shadow(panel, 8.0, &Shadow::new(16.0, 48.0, Color::rgba(0.0, 0.0, 0.0, if self.dark { 0.5 } else { 0.28 })));
        p.shadow(panel, 8.0, &Shadow::new(2.0, 6.0, Color::rgba(0.0, 0.0, 0.0, if self.dark { 0.3 } else { 0.12 })));
        p.clip_round_rect(panel, 8.0, |p| {
            p.bitmap(&self.blurred, full, None, 1.0, Interpolation::Linear);
            let tint = if self.dark { Color::rgba8(32, 32, 32, 0.45) } else { Color::rgba8(243, 243, 243, 0.45) };
            p.fill_rect(panel, tint);
        });
        let edge = if self.dark { Color::rgba(0.0, 0.0, 0.0, 0.5) } else { Color::rgba(0.0, 0.0, 0.0, 0.16) };
        p.hairline_round_rect(panel, 8.0, edge, false);
    }
}

fn paint_wallpaper(p: &mut Painter, size: SizeF, dark: bool) {
    let stops: [(f32, &str); 4] = if dark {
        [(0.0, "#0B1D3A"), (0.4, "#3B1E5C"), (0.75, "#7A2348"), (1.0, "#C2662D")]
    } else {
        [(0.0, "#8EC5FC"), (0.45, "#C3A6F2"), (0.8, "#F7B2C4"), (1.0, "#FFD9A0")]
    };
    let stops: Vec<(f32, Color)> = stops.iter().map(|(o, hex)| (*o, Color::hex(hex).unwrap())).collect();
    p.fill_rect(RectF::new(0.0, 0.0, size.w, size.h), Brush::linear(PointF::new(0.0, 0.0), PointF::new(size.w, size.h), &stops));
    let blobs = [("#00C2FF", 0.15, 0.2, 0.28), ("#FFB000", 0.85, 0.15, 0.22), ("#2BD67B", 0.25, 0.75, 0.25), ("#FF3D7F", 0.8, 0.8, 0.3)];
    for (hex, x, y, r) in blobs {
        p.fill_circle(PointF::new(size.w * x, size.h * y), size.w * r, Color::hex(hex).unwrap().with_alpha(0.7));
    }
    let ink = if dark { Color::rgba(1.0, 1.0, 1.0, 0.12) } else { Color::rgba(0.0, 0.0, 0.0, 0.08) };
    let mut y = 20.0;
    while y < size.h {
        p.line(PointF::new(0.0, y), PointF::new(size.w, y + 60.0), ink, 6.0, &StrokeStyle::default());
        y += 44.0;
    }
}

/// Synthetic content shared by all states.
struct Data {
    clips: Vec<ClipRow>,
    thumbnails: Vec<(ClipId, Rc<Bitmap>)>,
    emoji: Vec<PickerSection>,
    emoji_results: Vec<PickerSection>,
    kaomoji: Vec<PickerSection>,
    symbols: Vec<PickerSection>,
}

impl Data {
    fn new(gfx: &Rc<Gfx>) -> Result<Self> {
        let icon = |letter: &str, a: &str, b: &str| app_icon(gfx, letter, a, b);
        let notepad = (icon("N", "#3A96DD", "#1E5AA8")?, "Notepad");
        let figma = (icon("F", "#A259FF", "#F24E1E")?, "Figma");
        let edge = (icon("e", "#2BC48A", "#0C59A4")?, "Microsoft Edge");
        let code = (icon("{}", "#23A9F2", "#0065A9")?, "Visual Studio Code");
        let outlook = (icon("O", "#28A8EA", "#0A2767")?, "Outlook");
        let snip = (icon("S", "#B46CF0", "#5B2DB8")?, "Snipping Tool");
        let explorer = (icon("E", "#FFCE45", "#E8A100")?, "File Explorer");
        let teams = (icon("T", "#7B83EB", "#4B53BC")?, "Microsoft Teams");
        let text = |id: ClipId, text: &str, app: &(Rc<Bitmap>, &str), age_min: i64, pinned: bool| {
            row(id, ClipContent::Text { text: text.into(), html: None, rtf: None }, classify(text), app, age_min, pinned)
        };
        let clips = vec![
            text(1, "Wi-Fi for the studio: Lumen-Orbit-4471 (guest network, 5 GHz)", &notepad, 3 * 24 * 60, true),
            text(2, "#0A84FF", &figma, 125, true),
            text(3, "https://developer.apple.com/design/human-interface-guidelines/materials", &edge, 0, false),
            text(
                4,
                "fn main() {\n    let panel = Panel::new(options);\n    app.open(WindowSpec::panel(origin, Panel::SIZE), panel)?;\n    Ok(())\n}",
                &code,
                2,
                false,
            ),
            text(
                5,
                "Hi team, quick update on the panel: search, pinning and the emoji grid are done. The remaining work is the \
                 settings window and the installer. I'll share a build on Thursday so everyone can try it before the review.",
                &outlook,
                14,
                false,
            ),
            row(6, ClipContent::Image { blob: "0006".into(), width: 1920, height: 1080 }, TextKind::Plain, &snip, 32, false),
            text(7, r"C:\Users\you\Documents\Projects\Tuck\Design\Panel\panel-spec-final-v3.pdf", &explorer, 95, false),
            text(8, "jordan.lee@example.com", &outlook, 20 * 60, false),
            row(
                9,
                ClipContent::Files {
                    paths: ["Quarterly report.xlsx", "Team photo.heic", "Notes.txt", "Budget 2027.xlsx", "Slides.pptx"]
                        .iter()
                        .map(|n| PathBuf::from(format!(r"C:\Users\you\Desktop\{n}")))
                        .collect(),
                },
                TextKind::Plain,
                &explorer,
                3 * 24 * 60 + 200,
                false,
            ),
            text(10, "Thanks! See you at 4 \u{1F44B}", &teams, 4 * 24 * 60, false),
        ];
        let thumbnail = Bitmap::new(render_offscreen(gfx, &OffscreenSpec::pixels(664, 374, 1.0, Theme::dark()), |_, p| paint_screenshot(p, SizeF::new(664.0, 374.0)))?);
        Ok(Self {
            clips,
            thumbnails: vec![(6, thumbnail)],
            emoji: emoji_sections(),
            emoji_results: vec![section("", Icon::Heart, HEART_RESULTS)],
            kaomoji: kaomoji_sections(),
            symbols: symbol_sections(),
        })
    }
}

fn row(id: ClipId, content: ClipContent, kind: TextKind, app: &(Rc<Bitmap>, &str), age_min: i64, pinned: bool) -> ClipRow {
    let used_ms = NOW_MS - age_min * MINUTE - 20_000;
    let item = ClipItem { id, content, hash: id as u64, created_ms: used_ms, used_ms, pinned, source: None, bytes: 64 };
    ClipRow { item, kind, thumbnail: None, app_icon: Some(app.0.clone()), app_name: Some(app.1.to_string()) }
}

/// A rounded app tile with a gradient and a white glyph, 2× the 14 DIP meta-row size.
fn app_icon(gfx: &Rc<Gfx>, letter: &str, top: &str, bottom: &str) -> Result<Rc<Bitmap>> {
    let spec = OffscreenSpec::pixels(32, 32, 1.0, Theme::dark());
    let image = render_offscreen(gfx, &spec, |_, p| {
        let tile = RectF::new(1.0, 1.0, 30.0, 30.0);
        p.fill_round_rect(tile, 7.0, Brush::vertical(tile, Color::hex(top).unwrap(), Color::hex(bottom).unwrap()));
        p.text(letter, &TextStyle::new(17.0).weight(Weight::Bold).centered(), Color::WHITE, tile);
    })?;
    Ok(Bitmap::new(image))
}

/// A screenshot-like illustration with transparent margins, so the card's checkerboard shows.
fn paint_screenshot(p: &mut Painter, size: SizeF) {
    let window = RectF::new(40.0, 28.0, size.w - 80.0, size.h - 56.0);
    p.shadow(window, 14.0, &Shadow::new(10.0, 28.0, Color::rgba(0.0, 0.0, 0.0, 0.35)));
    p.clip_round_rect(window, 14.0, |p| {
        let sky = Brush::vertical(window, Color::hex("#4FACFE").unwrap(), Color::hex("#B9E6FF").unwrap());
        p.fill_rect(window, sky);
        p.fill_circle(PointF::new(window.right() - 120.0, window.y + 90.0), 44.0, Color::hex("#FFE27A").unwrap());
        let hill = |p: &mut Painter, cx: f32, r: f32, color: &str| p.fill_circle(PointF::new(cx, window.bottom() + r * 0.45), r, Color::hex(color).unwrap());
        hill(p, window.x + 120.0, 210.0, "#3FA36B");
        hill(p, window.x + 420.0, 240.0, "#2F8A57");
        p.fill_rect(RectF::new(window.x, window.y, window.w, 36.0), Color::rgba(1.0, 1.0, 1.0, 0.85));
        for (i, color) in ["#FF5F57", "#FEBC2E", "#28C840"].iter().enumerate() {
            p.fill_circle(PointF::new(window.x + 22.0 + i as f32 * 20.0, window.y + 18.0), 6.0, Color::hex(color).unwrap());
        }
    });
}

fn section(title: &str, icon: Icon, entries: &[(&str, &str)]) -> PickerSection {
    let items = entries.iter().map(|(text, name)| PickerItem::new(text, name)).collect();
    PickerSection { title: title.into(), icon, items }
}

/// The five Fitzpatrick variants after the default, as `PickerItem::variants` wants them.
fn toned(base: &str) -> Vec<String> {
    let stem: String = base.chars().filter(|c| *c != '\u{FE0F}').collect();
    std::iter::once(base.to_string())
        .chain(SkinTone::ALL[1..].iter().filter_map(|t| t.modifier()).map(|m| format!("{stem}{m}")))
        .collect()
}

fn emoji_sections() -> Vec<PickerSection> {
    let mut people = section("People & Body", Icon::Hand, PEOPLE);
    for item in &mut people.items {
        if HANDS_WITH_TONES.contains(&item.text.as_str()) {
            item.variants = toned(&item.text);
        }
    }
    vec![
        section("Frequently used", Icon::Clock, FREQUENT),
        section("Smileys & Emotion", Icon::Smile, SMILEYS),
        people,
        section("Animals & Nature", Icon::PawPrint, ANIMALS),
        section("Food & Drink", Icon::Apple, FOOD),
        section("Travel & Places", Icon::Car, TRAVEL),
        section("Activities", Icon::Volleyball, ACTIVITIES),
        section("Objects", Icon::Lightbulb, OBJECTS),
        section("Symbols", Icon::Heart, SYMBOL_EMOJI),
        section("Flags", Icon::Flag, FLAGS),
    ]
}

fn kaomoji_sections() -> Vec<PickerSection> {
    vec![
        section("Happy", Icon::Smile, KAOMOJI_HAPPY),
        section("Love", Icon::Heart, KAOMOJI_LOVE),
        section("Greeting", Icon::Hand, KAOMOJI_GREETING),
        section("Shrug", Icon::Sparkles, KAOMOJI_SHRUG),
        section("Sad", Icon::Smile, KAOMOJI_SAD),
        section("Angry", Icon::Smile, KAOMOJI_ANGRY),
        section("Animals", Icon::PawPrint, KAOMOJI_ANIMALS),
        section("Table flip", Icon::Sparkles, KAOMOJI_TABLE),
    ]
}

fn symbol_sections() -> Vec<PickerSection> {
    vec![
        section("Arrows", Icon::ArrowLeftRight, ARROWS),
        section("Math", Icon::Sigma, MATH),
        section("Currency", Icon::Euro, CURRENCY),
        section("Punctuation", Icon::Quote, PUNCTUATION),
        section("Greek", Icon::Omega, GREEK),
        section("Geometric", Icon::Shapes, GEOMETRIC),
        section("Letterlike & technical", Icon::Type, LETTERLIKE),
    ]
}

const HANDS_WITH_TONES: &[&str] = &["👋", "🤚", "✋", "🖖", "👌", "🤌", "🤏", "✌️", "🤞", "🫰", "🤟", "🤘", "🤙", "👍", "👎", "✊", "👊", "👏", "🙌", "🫶", "🙏", "💪"];

const FREQUENT: &[(&str, &str)] = &[
    ("😂", "face with tears of joy"), ("❤️", "red heart"), ("👍", "thumbs up"), ("🙏", "folded hands"), ("😭", "loudly crying face"),
    ("🥰", "smiling face with hearts"), ("😊", "smiling face with smiling eyes"), ("🔥", "fire"), ("✨", "sparkles"), ("🎉", "party popper"),
    ("😍", "smiling face with heart-eyes"), ("💯", "hundred points"), ("🤔", "thinking face"), ("👀", "eyes"), ("🥹", "face holding back tears"),
    ("😅", "grinning face with sweat"), ("🙌", "raising hands"), ("💀", "skull"), ("🫶", "heart hands"), ("✅", "check mark button"),
];

const SMILEYS: &[(&str, &str)] = &[
    ("😀", "grinning face"), ("😃", "grinning face with big eyes"), ("😄", "grinning face with smiling eyes"), ("😁", "beaming face with smiling eyes"),
    ("😆", "grinning squinting face"), ("😅", "grinning face with sweat"), ("🤣", "rolling on the floor laughing"), ("😂", "face with tears of joy"),
    ("🙂", "slightly smiling face"), ("🙃", "upside-down face"), ("🫠", "melting face"), ("😉", "winking face"), ("😊", "smiling face with smiling eyes"),
    ("😇", "smiling face with halo"), ("🥰", "smiling face with hearts"), ("😍", "smiling face with heart-eyes"), ("🤩", "star-struck"),
    ("😘", "face blowing a kiss"), ("😗", "kissing face"), ("☺️", "smiling face"), ("😚", "kissing face with closed eyes"), ("😙", "kissing face with smiling eyes"),
    ("🥲", "smiling face with tear"), ("😋", "face savoring food"), ("😛", "face with tongue"), ("😜", "winking face with tongue"), ("🤪", "zany face"),
    ("😝", "squinting face with tongue"), ("🤑", "money-mouth face"), ("🤗", "smiling face with open hands"), ("🤭", "face with hand over mouth"),
    ("🫢", "face with open eyes and hand over mouth"), ("🤫", "shushing face"), ("🤔", "thinking face"), ("🫡", "saluting face"), ("🤐", "zipper-mouth face"),
    ("🤨", "face with raised eyebrow"), ("😐", "neutral face"), ("😑", "expressionless face"), ("😶", "face without mouth"), ("🫥", "dotted line face"),
    ("😏", "smirking face"), ("😒", "unamused face"), ("🙄", "face with rolling eyes"), ("😬", "grimacing face"), ("🥹", "face holding back tears"),
    ("😌", "relieved face"), ("😔", "pensive face"), ("😪", "sleepy face"), ("🤤", "drooling face"), ("😴", "sleeping face"), ("😷", "face with medical mask"),
    ("🤒", "face with thermometer"), ("🤕", "face with head-bandage"), ("🤢", "nauseated face"), ("🤮", "face vomiting"), ("🥵", "hot face"), ("🥶", "cold face"),
];

const PEOPLE: &[(&str, &str)] = &[
    ("👋", "waving hand"), ("🤚", "raised back of hand"), ("✋", "raised hand"), ("🖖", "vulcan salute"), ("👌", "OK hand"), ("🤌", "pinched fingers"),
    ("🤏", "pinching hand"), ("✌️", "victory hand"), ("🤞", "crossed fingers"), ("🫰", "hand with index finger and thumb crossed"), ("🤟", "love-you gesture"),
    ("🤘", "sign of the horns"), ("🤙", "call me hand"), ("👍", "thumbs up"), ("👎", "thumbs down"), ("✊", "raised fist"), ("👊", "oncoming fist"),
    ("👏", "clapping hands"), ("🙌", "raising hands"), ("🫶", "heart hands"), ("🙏", "folded hands"), ("💪", "flexed biceps"), ("🧠", "brain"), ("👀", "eyes"),
    ("👶", "baby"), ("🧒", "child"), ("👩‍💻", "woman technologist"), ("🧑‍🚀", "astronaut"), ("👨‍👩‍👧", "family: man, woman, girl"),
];

const ANIMALS: &[(&str, &str)] = &[
    ("🐶", "dog face"), ("🐱", "cat face"), ("🐭", "mouse face"), ("🐹", "hamster"), ("🐰", "rabbit face"), ("🦊", "fox"), ("🐻", "bear"), ("🐼", "panda"),
    ("🐨", "koala"), ("🐯", "tiger face"), ("🦁", "lion"), ("🐮", "cow face"), ("🐷", "pig face"), ("🐸", "frog"), ("🐵", "monkey face"), ("🐔", "chicken"),
    ("🐧", "penguin"), ("🐦", "bird"), ("🦄", "unicorn"), ("🐝", "honeybee"), ("🦋", "butterfly"), ("🐢", "turtle"), ("🐙", "octopus"), ("🐬", "dolphin"),
    ("🌵", "cactus"), ("🌲", "evergreen tree"), ("🌸", "cherry blossom"),
];

const FOOD: &[(&str, &str)] = &[
    ("🍏", "green apple"), ("🍎", "red apple"), ("🍐", "pear"), ("🍊", "tangerine"), ("🍋", "lemon"), ("🍌", "banana"), ("🍉", "watermelon"), ("🍇", "grapes"),
    ("🍓", "strawberry"), ("🫐", "blueberries"), ("🍒", "cherries"), ("🍑", "peach"), ("🥭", "mango"), ("🍍", "pineapple"), ("🥥", "coconut"), ("🥝", "kiwi fruit"),
    ("🍅", "tomato"), ("🥑", "avocado"), ("🍕", "pizza"), ("🍔", "hamburger"), ("🍟", "french fries"), ("🌮", "taco"), ("🍣", "sushi"), ("🍩", "doughnut"),
    ("🍪", "cookie"), ("☕", "hot beverage"), ("🍵", "teacup without handle"),
];

const TRAVEL: &[(&str, &str)] = &[
    ("🚗", "automobile"), ("🚕", "taxi"), ("🚙", "sport utility vehicle"), ("🚌", "bus"), ("🏎️", "racing car"), ("🚓", "police car"), ("🚑", "ambulance"),
    ("🚒", "fire engine"), ("🚲", "bicycle"), ("🛴", "kick scooter"), ("🚂", "locomotive"), ("✈️", "airplane"), ("🚀", "rocket"), ("🛸", "flying saucer"),
    ("🚁", "helicopter"), ("⛵", "sailboat"), ("🗺️", "world map"), ("🏔️", "snow-capped mountain"), ("🏝️", "desert island"), ("🏠", "house"),
];

const ACTIVITIES: &[(&str, &str)] = &[
    ("⚽", "soccer ball"), ("🏀", "basketball"), ("🏈", "american football"), ("⚾", "baseball"), ("🎾", "tennis"), ("🏐", "volleyball"), ("🎱", "pool 8 ball"),
    ("🏓", "ping pong"), ("🏸", "badminton"), ("🥅", "goal net"), ("⛳", "flag in hole"), ("🎯", "bullseye"), ("🎮", "video game"), ("🎲", "game die"),
    ("🧩", "puzzle piece"), ("🎨", "artist palette"), ("🎬", "clapper board"), ("🎤", "microphone"),
];

const OBJECTS: &[(&str, &str)] = &[
    ("⌚", "watch"), ("📱", "mobile phone"), ("💻", "laptop"), ("⌨️", "keyboard"), ("🖥️", "desktop computer"), ("🖨️", "printer"), ("🖱️", "computer mouse"),
    ("💾", "floppy disk"), ("📷", "camera"), ("🔋", "battery"), ("💡", "light bulb"), ("🔦", "flashlight"), ("📚", "books"), ("✏️", "pencil"), ("📎", "paperclip"),
    ("📌", "pushpin"), ("✂️", "scissors"), ("🔒", "locked"), ("🔑", "key"), ("🧰", "toolbox"),
];

const SYMBOL_EMOJI: &[(&str, &str)] = &[
    ("❤️", "red heart"), ("🧡", "orange heart"), ("💛", "yellow heart"), ("💚", "green heart"), ("💙", "blue heart"), ("💜", "purple heart"), ("🖤", "black heart"),
    ("🤍", "white heart"), ("💔", "broken heart"), ("💕", "two hearts"), ("💯", "hundred points"), ("✅", "check mark button"), ("❌", "cross mark"),
    ("⚠️", "warning"), ("♻️", "recycling symbol"), ("➕", "plus"), ("❓", "red question mark"), ("‼️", "double exclamation mark"),
];

const FLAGS: &[(&str, &str)] = &[
    ("🏁", "chequered flag"), ("🚩", "triangular flag"), ("🎌", "crossed flags"), ("🏴", "black flag"), ("🏳️", "white flag"), ("🏳️‍🌈", "rainbow flag"),
    ("🏳️‍⚧️", "transgender flag"), ("🏴‍☠️", "pirate flag"),
];

const HEART_RESULTS: &[(&str, &str)] = &[
    ("❤️", "red heart"), ("🧡", "orange heart"), ("💛", "yellow heart"), ("💚", "green heart"), ("💙", "blue heart"), ("🩵", "light blue heart"),
    ("💜", "purple heart"), ("🤎", "brown heart"), ("🖤", "black heart"), ("🩶", "grey heart"), ("🤍", "white heart"), ("🩷", "pink heart"),
    ("💘", "heart with arrow"), ("💝", "heart with ribbon"), ("💖", "sparkling heart"), ("💗", "growing heart"), ("💓", "beating heart"),
    ("💞", "revolving hearts"), ("💕", "two hearts"), ("❣️", "heart exclamation"), ("💔", "broken heart"), ("❤️‍🔥", "heart on fire"),
    ("❤️‍🩹", "mending heart"), ("😍", "smiling face with heart-eyes"), ("🥰", "smiling face with hearts"), ("😻", "smiling cat with heart-eyes"),
    ("🫶", "heart hands"), ("💌", "love letter"),
];

const KAOMOJI_HAPPY: &[(&str, &str)] = &[
    ("(^▽^)", "happy"), ("(◕‿◕)", "smile"), ("(*^‿^*)", "blush"), ("ヽ(・∀・)ﾉ", "cheer"), ("(≧▽≦)", "delighted"), ("(✿◠‿◠)", "flower smile"),
    ("(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧", "sparkle joy"), ("٩(◕‿◕｡)۶", "yay"), ("(＾▽＾)", "grin"), ("o(≧▽≦)o", "excited"),
];
const KAOMOJI_LOVE: &[(&str, &str)] = &[
    ("(♥ω♥*)", "love"), ("(´∀｀)♡", "heart"), ("(｡♥‿♥｡)", "in love"), ("♡(˘▽˘>ԅ( ˘⌣˘)", "hug"), ("(づ｡◕‿‿◕｡)づ", "hug"), ("( ˘ ³˘)♥", "kiss"),
];
const KAOMOJI_GREETING: &[(&str, &str)] = &[("(・ω・)ノ", "hi"), ("ヾ(・ω・*)", "wave"), ("(^_^)/", "hello"), ("(￣▽￣)ノ", "bye"), ("ヽ(^o^)丿", "hey")];
const KAOMOJI_SHRUG: &[(&str, &str)] = &[("¯\\_(ツ)_/¯", "shrug"), ("┐(￣ヘ￣)┌", "whatever"), ("ヽ(ー_ー )ノ", "dunno"), ("┐(´～｀)┌", "meh")];
const KAOMOJI_SAD: &[(&str, &str)] = &[("(╥﹏╥)", "crying"), ("(ಥ﹏ಥ)", "tears"), ("(っ˘̩╭╮˘̩)っ", "sad"), ("(｡•́︿•̀｡)", "pout"), ("(-_-;)", "awkward")];
const KAOMOJI_ANGRY: &[(&str, &str)] = &[("(╬ Ò﹏Ó)", "angry"), ("(ノಠ益ಠ)ノ", "rage"), ("(＃`Д´)", "mad"), ("ψ(｀∇´)ψ", "devil")];
const KAOMOJI_ANIMALS: &[(&str, &str)] = &[("(=^･ω･^=)", "cat"), ("U・ᴥ・U", "dog"), ("ʕ•ᴥ•ʔ", "bear"), ("(・θ・)", "bird"), ("<コ:彡", "squid")];
const KAOMOJI_TABLE: &[(&str, &str)] = &[("(╯°□°）╯︵ ┻━┻", "table flip"), ("┬─┬ノ( º _ ºノ)", "put the table back"), ("(ノ°Д°）ノ︵ ┻━┻", "flip")];

const ARROWS: &[(&str, &str)] = &[
    ("←", "leftwards arrow"), ("↑", "upwards arrow"), ("→", "rightwards arrow"), ("↓", "downwards arrow"), ("↔", "left right arrow"), ("↕", "up down arrow"),
    ("↖", "north west arrow"), ("↗", "north east arrow"), ("↘", "south east arrow"), ("↙", "south west arrow"), ("⇐", "leftwards double arrow"),
    ("⇒", "rightwards double arrow"), ("⇔", "left right double arrow"), ("↩", "leftwards arrow with hook"), ("↪", "rightwards arrow with hook"),
    ("⟵", "long leftwards arrow"), ("⟶", "long rightwards arrow"), ("↻", "clockwise open circle arrow"), ("⤴", "arrow pointing rightwards then curving upwards"),
    ("➜", "heavy round-tipped rightwards arrow"),
];
const MATH: &[(&str, &str)] = &[
    ("±", "plus-minus sign"), ("×", "multiplication sign"), ("÷", "division sign"), ("≠", "not equal to"), ("≈", "almost equal to"), ("≤", "less-than or equal to"),
    ("≥", "greater-than or equal to"), ("∞", "infinity"), ("√", "square root"), ("∑", "n-ary summation"), ("∏", "n-ary product"), ("∫", "integral"),
    ("∂", "partial differential"), ("∆", "increment"), ("∈", "element of"), ("∩", "intersection"), ("∪", "union"), ("°", "degree sign"), ("‰", "per mille sign"),
    ("½", "vulgar fraction one half"),
];
const CURRENCY: &[(&str, &str)] = &[
    ("€", "euro sign"), ("£", "pound sign"), ("¥", "yen sign"), ("$", "dollar sign"), ("¢", "cent sign"), ("₹", "indian rupee sign"), ("₽", "ruble sign"),
    ("₩", "won sign"), ("₺", "turkish lira sign"), ("₿", "bitcoin sign"), ("₫", "dong sign"), ("₪", "new sheqel sign"),
];
const PUNCTUATION: &[(&str, &str)] = &[
    ("“", "left double quotation mark"), ("”", "right double quotation mark"), ("‘", "left single quotation mark"), ("’", "right single quotation mark"),
    ("«", "left-pointing double angle quotation mark"), ("»", "right-pointing double angle quotation mark"), ("—", "em dash"), ("–", "en dash"),
    ("…", "horizontal ellipsis"), ("·", "middle dot"), ("•", "bullet"), ("§", "section sign"), ("¶", "pilcrow sign"), ("†", "dagger"), ("‡", "double dagger"),
    ("¿", "inverted question mark"), ("¡", "inverted exclamation mark"), ("‽", "interrobang"),
];
const GREEK: &[(&str, &str)] = &[
    ("α", "greek small letter alpha"), ("β", "greek small letter beta"), ("γ", "greek small letter gamma"), ("δ", "greek small letter delta"),
    ("ε", "greek small letter epsilon"), ("θ", "greek small letter theta"), ("λ", "greek small letter lamda"), ("μ", "greek small letter mu"),
    ("π", "greek small letter pi"), ("σ", "greek small letter sigma"), ("φ", "greek small letter phi"), ("ω", "greek small letter omega"),
    ("Δ", "greek capital letter delta"), ("Σ", "greek capital letter sigma"), ("Ω", "greek capital letter omega"),
];
const GEOMETRIC: &[(&str, &str)] = &[
    ("■", "black square"), ("□", "white square"), ("▲", "black up-pointing triangle"), ("△", "white up-pointing triangle"), ("●", "black circle"),
    ("○", "white circle"), ("◆", "black diamond"), ("◇", "white diamond"), ("★", "black star"), ("☆", "white star"), ("◐", "circle with left half black"),
    ("▶", "black right-pointing triangle"), ("◀", "black left-pointing triangle"), ("⬟", "black pentagon"),
];
const LETTERLIKE: &[(&str, &str)] = &[
    ("©", "copyright sign"), ("®", "registered sign"), ("™", "trade mark sign"), ("℃", "degree celsius"), ("℉", "degree fahrenheit"), ("№", "numero sign"),
    ("℮", "estimated symbol"), ("⌘", "place of interest sign"), ("⌥", "option key"), ("⌃", "up arrowhead"), ("⇧", "upwards white arrow"), ("⏎", "return symbol"),
    ("⌫", "erase to the left"), ("⎋", "broken circle with northwest arrow"),
];
