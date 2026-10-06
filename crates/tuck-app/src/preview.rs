//! `--preview <view>`: offscreen renders of the panel, settings window, toasts and icon art (DESIGN §11) with
//! synthetic data only: no window, hook, clipboard, registry or user data. The panel is drawn exactly as its window
//! draws it and composited over a synthetic wallpaper with a blurred stand-in for the DWM acrylic.

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Result, bail};
use tuck_core::{ClipContent, ClipId, History, PointF, RectF, RectI, Settings, SizeF, SourceApp, ThemeMode, thumbnail};
use tuck_emoji::{Coverage, Kind, Usage};
use tuck_panel::{Clock, Panel, PanelOptions, PickerSection, Tab};
use tuck_ui::{
    Bitmap, Brush, Color, Ctx, Gfx, Image, Interpolation, OffscreenSpec, Painter, Shadow, StrokeStyle, TextStyle,
    Theme, View, Weight, render_offscreen, render_view_offscreen,
};

use crate::art::{render_app_icon, render_tray_glyph};
use crate::clips::{RowCache, THUMBNAIL_MAX};
use crate::pickers;
use crate::popup::corner_layout;
use crate::resident::PANEL_BACKDROP;
use crate::settings_model::{ImportState, InstallState, SettingsEnv, panel_options};
use crate::settings_view::{SettingsView, WINDOW_SIZE};
use crate::system::RunningApp;
use crate::toast::{Tint, Toast, ToastIcon, ToastView};

pub const KINDS: [&str; 14] = [
    "panel-clipboard",
    "panel-clipboard-empty",
    "panel-clipboard-search",
    "panel-emoji",
    "panel-emoji-search",
    "panel-emoji-tones",
    "panel-kaomoji",
    "panel-symbols",
    "panel-menu",
    "settings",
    "settings-clipboard",
    "toast",
    "app-icon",
    "tray-icon",
];

pub const ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

const MARGIN: f32 = 32.0;
/// A fixed "now" (Thursday 6 Oct 2026, 15:30 UTC) so relative times never change between runs.
const NOW_MS: i64 = 1_791_300_600_000;
const MINUTE: i64 = 60_000;
const IMAGE_ID: ClipId = 6;

pub fn theme_for(mode: ThemeMode) -> Theme {
    if mode == ThemeMode::Light { Theme::light() } else { Theme::dark() }
}

/// Renders `kind` (see `KINDS`) at `scale` in `theme`.
pub fn render(gfx: &Rc<Gfx>, kind: &str, theme: ThemeMode, scale: f32) -> Result<Image> {
    let theme = theme_for(theme);
    match kind {
        "settings" => settings_page(gfx, &theme, scale),
        "settings-clipboard" => settings_clipboard(gfx, &theme, scale),
        "toast" => toasts(gfx, &theme, scale),
        "app-icon" => app_icon_sheet(gfx, &theme, scale),
        "tray-icon" => tray_icon_sheet(gfx, &theme, scale),
        panel if panel.starts_with("panel-") && KINDS.contains(&panel) => {
            let mut data = PanelData::new(gfx)?;
            panel_preview(gfx, &mut data, panel, &theme, scale)
        }
        other => bail!("unknown preview view `{other}`; one of: {}", KINDS.join(", ")),
    }
}

// ---- synthetic content ------------------------------------------------------------------------------------------

/// What every panel preview starts from: a synthetic history turned into rows by the app's own code, and the real
/// catalogs with a synthetic usage record.
pub struct PanelData {
    pub history: History,
    pub rows: RowCache,
    pub coverage: Coverage,
    pub usage: Usage,
}

struct SampleApp {
    exe: &'static str,
    name: &'static str,
    letter: &'static str,
    colors: (&'static str, &'static str),
}

const NOTEPAD: SampleApp =
    SampleApp { exe: r"C:\Windows\notepad.exe", name: "Notepad", letter: "N", colors: ("#3A96DD", "#1E5AA8") };
const FIGMA: SampleApp = SampleApp {
    exe: r"C:\Users\you\AppData\Local\Figma\Figma.exe",
    name: "Figma",
    letter: "F",
    colors: ("#A259FF", "#F24E1E"),
};
const EDGE: SampleApp = SampleApp {
    exe: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    name: "Microsoft Edge",
    letter: "e",
    colors: ("#2BC48A", "#0C59A4"),
};
const CODE: SampleApp = SampleApp {
    exe: r"C:\Users\you\AppData\Local\Programs\Microsoft VS Code\Code.exe",
    name: "Visual Studio Code",
    letter: "{}",
    colors: ("#23A9F2", "#0065A9"),
};
const OUTLOOK: SampleApp = SampleApp {
    exe: r"C:\Program Files\Microsoft Office\OUTLOOK.EXE",
    name: "Outlook",
    letter: "O",
    colors: ("#28A8EA", "#0A2767"),
};
const SNIP: SampleApp = SampleApp {
    exe: r"C:\Users\you\AppData\Local\Programs\Glint\glint.exe",
    name: "Glint",
    letter: "G",
    colors: ("#3D86FF", "#7B36E9"),
};
const EXPLORER: SampleApp =
    SampleApp { exe: r"C:\Windows\explorer.exe", name: "File Explorer", letter: "E", colors: ("#FFCE45", "#E8A100") };
const TEAMS: SampleApp = SampleApp {
    exe: r"C:\Program Files\Teams\ms-teams.exe",
    name: "Microsoft Teams",
    letter: "T",
    colors: ("#7B83EB", "#4B53BC"),
};
const APPS: [&SampleApp; 8] = [&NOTEPAD, &FIGMA, &EDGE, &CODE, &OUTLOOK, &SNIP, &EXPLORER, &TEAMS];

fn text(value: &str) -> ClipContent {
    ClipContent::Text { text: value.to_string(), html: None, rtf: None }
}

impl PanelData {
    pub fn new(gfx: &Rc<Gfx>) -> Result<Self> {
        let mut history = History::new();
        let add = |history: &mut History, content: ClipContent, hash: u64, app: &SampleApp, age_min: i64| {
            let source = SourceApp { exe: PathBuf::from(app.exe), name: app.name.to_string() };
            let bytes = tuck_core::content_bytes(&content);
            history.add(content, hash, bytes, Some(source), NOW_MS - age_min * MINUTE - 20_000).0.id()
        };
        let files: Vec<PathBuf> =
            ["Quarterly report.xlsx", "Team photo.heic", "Notes.txt", "Budget 2027.xlsx", "Slides.pptx"]
                .iter()
                .map(|name| PathBuf::from(format!(r"C:\Users\you\Desktop\{name}")))
                .collect();
        let samples: Vec<(ClipContent, &SampleApp, i64)> = vec![
            (text("Wi-Fi for the studio: Lumen-Orbit-4471 (guest network, 5 GHz)"), &NOTEPAD, 3 * 24 * 60),
            (text("#FF4D8D"), &FIGMA, 125),
            (text("https://developer.apple.com/design/human-interface-guidelines/materials"), &EDGE, 0),
            (
                text(
                    "fn main() {\n    let panel = Panel::new(options);\n    app.open(WindowSpec::panel(origin, Panel::SIZE), panel)?;\n    Ok(())\n}",
                ),
                &CODE,
                2,
            ),
            (
                text(
                    "Hi team, quick update on the panel: search, pinning and the emoji grid are done. The remaining work \
                     is the settings window and the installer. I'll share a build on Thursday so everyone can try it.",
                ),
                &OUTLOOK,
                14,
            ),
            (ClipContent::Image { blob: format!("{IMAGE_ID:016x}"), width: 1920, height: 1080 }, &SNIP, 32),
            (text(r"C:\Users\you\Documents\Projects\Tuck\Design\Panel\panel-spec-final-v3.pdf"), &EXPLORER, 95),
            (text("jordan.lee@example.com"), &OUTLOOK, 20 * 60),
            (ClipContent::Files { paths: files }, &EXPLORER, 3 * 24 * 60 + 200),
            (text("Thanks! See you at 4 \u{1F44B}"), &TEAMS, 4 * 24 * 60),
        ];
        for (index, (content, app, age_min)) in samples.into_iter().enumerate() {
            add(&mut history, content, index as u64 + 1, app, age_min);
        }
        history.set_pinned(1, true);
        history.set_pinned(2, true);
        let mut rows = RowCache::default();
        for app in APPS {
            rows.set_icon(PathBuf::from(app.exe), Some(app_icon(gfx, app)?));
        }
        let screenshot = synthetic_screenshot(gfx, 1920, 1080)?;
        rows.set_thumbnail(IMAGE_ID, Bitmap::new(thumbnail(&screenshot, THUMBNAIL_MAX.0, THUMBNAIL_MAX.1)));
        let coverage = Coverage::compute().unwrap_or_else(|_| Coverage::all_supported());
        let mut usage = Usage::default();
        for (age, picks) in [
            ("😂", 9),
            ("❤️", 8),
            ("👍", 7),
            ("🙏", 6),
            ("✨", 6),
            ("🔥", 5),
            ("🥹", 5),
            ("👋", 4),
            ("🎉", 4),
            ("😭", 3),
            ("👀", 3),
            ("✅", 2),
            ("🫶", 2),
            ("💯", 2),
        ]
        .iter()
        .enumerate()
        {
            for pick in 0..picks.1 {
                usage.record(Kind::Emoji, picks.0, NOW_MS - (age as i64 * 7 + pick) * MINUTE);
            }
        }
        for (age, kaomoji) in ["¯\\_(ツ)_/¯", "(╯°□°)╯︵ ┻━┻", "(◕‿◕)"].iter().enumerate() {
            usage.record(Kind::Kaomoji, kaomoji, NOW_MS - age as i64 * MINUTE);
        }
        for (age, symbol) in ["→", "€", "©", "°", "×", "…"].iter().enumerate() {
            usage.record(Kind::Symbol, symbol, NOW_MS - age as i64 * MINUTE);
        }
        Ok(Self { history, rows, coverage, usage })
    }

    fn sections(&self, kind: Kind) -> Vec<PickerSection> {
        pickers::sections(kind, &self.coverage, &self.usage)
    }
}

/// A rounded app tile with a gradient and a white glyph, 2× the 14 DIP meta-row size.
fn app_icon(gfx: &Rc<Gfx>, app: &SampleApp) -> Result<Rc<Bitmap>> {
    let spec = OffscreenSpec::pixels(32, 32, 1.0, Theme::dark());
    let image = render_offscreen(gfx, &spec, |_, p| {
        let tile = RectF::new(1.0, 1.0, 30.0, 30.0);
        let (top, bottom) = (Color::hex(app.colors.0).unwrap(), Color::hex(app.colors.1).unwrap());
        p.fill_round_rect(tile, 7.0, Brush::vertical(tile, top, bottom));
        p.text(app.letter, &TextStyle::new(17.0).weight(Weight::Bold).centered(), Color::WHITE, tile);
    })?;
    Ok(Bitmap::new(image))
}

/// A screenshot-like illustration: a photo window with a sky, sun and hills on a transparent margin.
pub fn synthetic_screenshot(gfx: &Rc<Gfx>, w: u32, h: u32) -> Result<Image> {
    let spec = OffscreenSpec::pixels(w, h, 1.0, Theme::dark());
    let (wf, hf) = (w as f32, h as f32);
    render_offscreen(gfx, &spec, |_, p| {
        let window = RectF::new(wf * 0.06, hf * 0.08, wf * 0.88, hf * 0.84);
        p.shadow(window, hf * 0.03, &Shadow::new(hf * 0.02, hf * 0.06, Color::rgba(0.0, 0.0, 0.0, 0.35)));
        p.clip_round_rect(window, hf * 0.03, |p| {
            p.fill_rect(
                window,
                Brush::vertical(window, Color::hex("#4FACFE").unwrap(), Color::hex("#B9E6FF").unwrap()),
            );
            p.fill_circle(
                PointF::new(window.right() - wf * 0.2, window.y + hf * 0.25),
                hf * 0.1,
                Color::hex("#FFE27A").unwrap(),
            );
            for (x, r, color) in [(0.2, 0.55, "#3FA36B"), (0.7, 0.62, "#2F8A57")] {
                p.fill_circle(
                    PointF::new(window.x + window.w * x, window.bottom() + hf * r * 0.45),
                    hf * r,
                    Color::hex(color).unwrap(),
                );
            }
            p.fill_rect(RectF::new(window.x, window.y, window.w, hf * 0.08), Color::rgba(1.0, 1.0, 1.0, 0.85));
            for (i, color) in ["#FF5F57", "#FEBC2E", "#28C840"].iter().enumerate() {
                let center = PointF::new(window.x + hf * (0.05 + i as f32 * 0.045), window.y + hf * 0.04);
                p.fill_circle(center, hf * 0.014, Color::hex(color).unwrap());
            }
        });
    })
}

// ---- panel ------------------------------------------------------------------------------------------------------

fn options() -> PanelOptions {
    PanelOptions { glint_available: true, close_after_click: true, ..panel_options(&Settings::default(), true) }
}

fn panel_preview(gfx: &Rc<Gfx>, data: &mut PanelData, kind: &str, theme: &Theme, scale: f32) -> Result<Image> {
    let tab = match kind {
        "panel-emoji" | "panel-emoji-search" | "panel-emoji-tones" => Tab::Emoji,
        "panel-kaomoji" => Tab::Kaomoji,
        "panel-symbols" => Tab::Symbols,
        _ => Tab::Clipboard,
    };
    let mut cx = Ctx::offscreen(gfx.clone(), Panel::SIZE, scale, theme.clone(), 0.0);
    let mut panel = Panel::new(options());
    panel.set_backdrop(PANEL_BACKDROP);
    panel.set_clock(Some(Clock { now_ms: NOW_MS, utc_offset_minutes: 120 }));
    panel.open(&mut cx, tab);
    panel.preview_caret(Some(true));
    let all = data.history.search("");
    let (clips, _) = data.rows.rows(&data.history, &all);
    panel.set_clips(&mut cx, clips, data.history.len());
    for kind in Kind::ALL {
        panel.set_sections(&mut cx, pickers::tab_of(kind), data.sections(kind));
    }
    match kind {
        "panel-clipboard" => {
            panel.preview_select_card(3);
            panel.preview_hover_card(Some(4));
        }
        "panel-clipboard-empty" => panel.set_clips(&mut cx, Vec::new(), 0),
        "panel-clipboard-search" => {
            panel.preview_query("panel");
            let found_ids = data.history.search("panel");
            let (found, _) = data.rows.rows(&data.history, &found_ids);
            panel.set_clips(&mut cx, found, data.history.len());
        }
        "panel-menu" => {
            panel.preview_scroll(gfx, Tab::Clipboard, 380.0);
            panel.preview_hover_card(Some(IMAGE_ID));
            panel.preview_menu(gfx, IMAGE_ID, PointF::new(120.0, 150.0), Some(4));
        }
        "panel-emoji" => panel.preview_hover_cell(gfx, Tab::Emoji, "🥹", true),
        "panel-emoji-search" => {
            panel.preview_query("heart");
            let results = pickers::search_sections(Kind::Emoji, "heart", &data.coverage, &data.usage);
            panel.set_sections(&mut cx, Tab::Emoji, results);
            panel.preview_hover_cell(gfx, Tab::Emoji, "💜", true);
        }
        "panel-emoji-tones" => panel.preview_tones(gfx, "👋", Some(3)),
        "panel-kaomoji" => panel.preview_hover_cell(gfx, Tab::Kaomoji, "(╯°□°)╯︵ ┻━┻", false),
        "panel-symbols" => panel.preview_hover_cell(gfx, Tab::Symbols, "€", true),
        _ => {}
    }
    let spec = OffscreenSpec::new(Panel::SIZE, scale, theme.clone());
    let panel_image = render_offscreen(gfx, &spec, |cx, p| View::paint(&mut panel, cx, p))?;
    let canvas = Canvas::new(gfx, theme, scale)?;
    let bitmap = Bitmap::new(panel_image);
    let composite = OffscreenSpec::new(canvas.size, scale, theme.clone());
    render_offscreen(gfx, &composite, |_, p| {
        canvas.paint_desktop(p);
        p.bitmap(&bitmap, RectF::new(MARGIN, MARGIN, Panel::SIZE.w, Panel::SIZE.h), None, 1.0, Interpolation::Linear);
    })
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
    p.fill_rect(
        RectF::new(0.0, 0.0, size.w, size.h),
        Brush::linear(PointF::new(0.0, 0.0), PointF::new(size.w, size.h), &stops),
    );
    let blobs = [
        ("#00C2FF", 0.15, 0.2, 0.28),
        ("#FFB000", 0.85, 0.15, 0.22),
        ("#2BD67B", 0.25, 0.75, 0.25),
        ("#FF3D7F", 0.8, 0.8, 0.3),
    ];
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

// ---- settings and toasts ----------------------------------------------------------------------------------------

fn sample_settings() -> Settings {
    let mut settings = Settings::default();
    settings.history.ignored_apps = vec!["keepass.exe".into(), "1password.exe".into()];
    settings
}

fn sample_env() -> SettingsEnv {
    SettingsEnv {
        version: crate::VERSION.into(),
        install: InstallState::Installed { path: r"C:\Users\you\AppData\Local\Programs\Tuck\tuck.exe".into() },
        history_items: 184,
        import: ImportState::Done(12),
    }
}

fn sample_apps() -> Vec<RunningApp> {
    [
        ("Discord", "discord.exe"),
        ("File Explorer", "explorer.exe"),
        ("KeePass", "keepass.exe"),
        ("Microsoft Edge", "msedge.exe"),
        ("Notepad", "notepad.exe"),
        ("Visual Studio Code", "code.exe"),
    ]
    .iter()
    .map(|(name, exe)| RunningApp { name: name.to_string(), exe_name: exe.to_string() })
    .collect()
}

fn settings_page(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let mut view = SettingsView::new(sample_settings(), sample_env());
    let height = view.content_height(gfx, WINDOW_SIZE.w);
    let spec = OffscreenSpec::new(SizeF::new(WINDOW_SIZE.w, height), scale, theme.clone());
    render_view_offscreen(gfx, &mut view, &spec)
}

/// The window as it opens, scrolled to the Clipboard section with the "Add app…" menu open.
fn settings_clipboard(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let mut view = SettingsView::new(sample_settings(), sample_env());
    view.preview_scroll_to(gfx, WINDOW_SIZE, "Clipboard");
    view.preview_app_menu(gfx, WINDOW_SIZE, sample_apps(), Some(3));
    let spec = OffscreenSpec::new(WINDOW_SIZE, scale, theme.clone());
    render_view_offscreen(gfx, &mut view, &spec)
}

fn toasts(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let items = vec![
        Toast::new(ToastIcon::App, "Tuck is running", Some("Win + V for clipboard history, Win + . for emoji")),
        Toast::new(ToastIcon::Symbol(tuck_ui::Icon::Copy, Tint::Accent), "Copied", Some("Paste it with Ctrl + V")),
    ];
    let canvas = SizeF::new(520.0, 260.0);
    let work = RectI::new(0, 0, (canvas.w * scale).round() as i32, (canvas.h * scale).round() as i32);
    let mut layers = Vec::new();
    let mut lift = 0;
    for (i, item) in items.into_iter().enumerate() {
        let card = crate::toast::card_size(gfx, &item);
        let layout = corner_layout(card, work, scale, lift);
        lift += ((layout.card.h + 12.0) * scale).round() as i32;
        let mut view = ToastView::new(i as u64, item, layout.card).settled();
        let spec = OffscreenSpec::new(layout.size, scale, theme.clone());
        let bitmap = Bitmap::new(render_view_offscreen(gfx, &mut view, &spec)?);
        layers.push((bitmap, PointF::new(layout.origin_px.x as f32 / scale, layout.origin_px.y as f32 / scale)));
    }
    let spec = OffscreenSpec::new(canvas, scale, theme.clone());
    render_offscreen(gfx, &spec, |_, p| {
        paint_wallpaper(p, canvas, theme.is_dark());
        for (bitmap, origin) in &layers {
            let origin = p.snap_point(*origin);
            let size = SizeF::new(bitmap.width() as f32 * p.px(), bitmap.height() as f32 * p.px());
            p.bitmap(bitmap, RectF::new(origin.x, origin.y, size.w, size.h), None, 1.0, Interpolation::Nearest);
        }
    })
}

// ---- icon sheets ------------------------------------------------------------------------------------------------

fn label(p: &mut Painter, text: &str, center_x: f32, y: f32, color: Color) {
    let style = TextStyle::caption().centered().tabular();
    p.text(text, &style, color, RectF::new(center_x - 60.0, y, 120.0, 16.0));
}

/// Draws `image` 1:1 with physical pixels at `origin` (DIP), optionally magnified with nearest-neighbour sampling.
fn draw_pixels(p: &mut Painter, image: &Rc<Bitmap>, origin: PointF, magnify: f32) {
    let size = SizeF::new(image.width() as f32 * p.px() * magnify, image.height() as f32 * p.px() * magnify);
    let origin = p.snap_point(origin);
    p.bitmap(image, RectF::new(origin.x, origin.y, size.w, size.h), None, 1.0, Interpolation::Nearest);
}

fn app_icon_sheet(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let icons: Vec<(u32, Rc<Bitmap>)> =
        ICON_SIZES.iter().map(|&s| Ok((s, Bitmap::new(render_app_icon(gfx, s)?)))).collect::<Result<_>>()?;
    let size = SizeF::new(1000.0, 620.0);
    let spec = OffscreenSpec::new(size, scale, theme.clone()).background(theme.window_background);
    render_offscreen(gfx, &spec, |_, p| {
        let text = theme.text_secondary;
        let mut x = 24.0;
        for (side, bitmap) in icons.iter().rev().take(5) {
            let dip = *side as f32 / scale;
            draw_pixels(p, bitmap, PointF::new(x, 24.0 + (256.0 / scale - dip)), 1.0);
            label(p, &format!("{side} px"), x + dip / 2.0, 36.0 + 256.0 / scale, text);
            x += dip + 32.0;
        }
        let mut x = 24.0;
        let top = 340.0;
        for (side, bitmap) in icons.iter().take(5) {
            let dip = *side as f32 / scale;
            draw_pixels(p, bitmap, PointF::new(x, top), 1.0);
            draw_pixels(p, bitmap, PointF::new(x, top + 48.0), 4.0);
            label(p, &format!("{side} px"), x + dip * 2.0, top + 60.0 + dip * 4.0, text);
            x += dip * 4.0 + 40.0;
        }
    })
}

fn tray_icon_sheet(gfx: &Rc<Gfx>, theme: &Theme, scale: f32) -> Result<Image> {
    let sizes = [16u32, 20, 24, 32, 40];
    let size = SizeF::new(760.0, 420.0);
    let spec = OffscreenSpec::new(size, scale, theme.clone()).background(theme.window_background);
    let strips = [(Color::rgb8(0x1C, 0x1C, 0x1C), Color::WHITE), (Color::rgb8(0xEE, 0xEE, 0xEE), Color::BLACK)];
    let glyphs: Vec<Vec<Rc<Bitmap>>> = strips
        .iter()
        .map(|(_, ink)| sizes.iter().map(|&s| Ok(Bitmap::new(render_tray_glyph(gfx, s, *ink)?))).collect::<Result<_>>())
        .collect::<Result<_>>()?;
    render_offscreen(gfx, &spec, |_, p| {
        for (row, ((background, _), bitmaps)) in strips.iter().zip(&glyphs).enumerate() {
            let top = 20.0 + row as f32 * 200.0;
            p.fill_rect(RectF::new(0.0, top, size.w, 180.0), *background);
            let mut x = 24.0;
            for (side, bitmap) in sizes.iter().zip(bitmaps) {
                draw_pixels(p, bitmap, PointF::new(x, top + 16.0), 1.0);
                draw_pixels(p, bitmap, PointF::new(x, top + 48.0), (96 / side).max(2) as f32);
                let ink = if row == 0 { Color::rgba(1.0, 1.0, 1.0, 0.6) } else { Color::rgba(0.0, 0.0, 0.0, 0.55) };
                label(p, &format!("{side} px"), x + 48.0 / scale, top + 150.0, ink);
                x += 96.0 / scale + 48.0;
            }
        }
    })
}
