//! Panel metrics, colors and small shared painters (DESIGN §7, §9).

use tuck_core::SkinTone;
use tuck_ui::{Brush, Color, Painter, PointF, RectF, Shadow, SizeF, TextStyle, Theme, Weight};

pub const PANEL_SIZE: SizeF = SizeF::new(380.0, 460.0);
pub const PADDING: f32 = 12.0;
/// DWM's round window corner.
pub const WINDOW_RADIUS: f32 = 8.0;
pub const SEARCH_TOP: f32 = 12.0;
pub const SEARCH_HEIGHT: f32 = 34.0;
pub const GEAR: f32 = 32.0;
pub const TABS_TOP: f32 = SEARCH_TOP + SEARCH_HEIGHT + 8.0;
pub const TABS_HEIGHT: f32 = 32.0;
pub const CONTENT_TOP: f32 = TABS_TOP + TABS_HEIGHT + 8.0;
pub const BOTTOM_BAR: f32 = 40.0;
pub const SECTION_HEADER: f32 = 28.0;
/// Fade at the bottom edge of scrolled content while more is below.
pub const BOTTOM_FADE: f32 = 16.0;

pub const BLINK_TIMER: u64 = 1;
pub const CLIPBOARD_SCROLL_TIMER: u64 = 2;
pub const PICKER_SCROLL_TIMERS: [u64; 3] = [3, 4, 5];
pub const LABEL_TIMER: u64 = 6;
pub const LONG_PRESS_TIMER: u64 = 7;

/// The panel's own fill over the DWM material (or opaque without one).
pub fn glass_fill(theme: &Theme, opaque: bool) -> Color {
    match (theme.is_dark(), opaque) {
        (true, false) => Color::rgba8(30, 30, 32, 0.55),
        (false, false) => Color::rgba8(246, 246, 248, 0.62),
        (true, true) => Color::rgb8(30, 30, 32),
        (false, true) => Color::rgb8(246, 246, 248),
    }
}

/// Section captions: 11 semibold secondary.
pub fn caption_style() -> TextStyle {
    TextStyle::caption().weight(Weight::Semibold)
}

/// Bottom bar labels.
pub fn bar_label_style() -> TextStyle {
    TextStyle::new(12.0)
}

/// Swatch color of each skin tone, `SkinTone::ALL` order.
pub fn tone_color(tone: SkinTone) -> Color {
    let hex = match tone {
        SkinTone::Default => "#FFC83D",
        SkinTone::Light => "#F7DECE",
        SkinTone::MediumLight => "#F3D2A2",
        SkinTone::Medium => "#D5AB88",
        SkinTone::MediumDark => "#AF7E57",
        SkinTone::Dark => "#7C533E",
    };
    Color::hex(hex).unwrap_or(Color::WHITE)
}

pub fn tone_name(tone: SkinTone) -> &'static str {
    match tone {
        SkinTone::Default => "Default",
        SkinTone::Light => "Light",
        SkinTone::MediumLight => "Medium light",
        SkinTone::Medium => "Medium",
        SkinTone::MediumDark => "Medium dark",
        SkinTone::Dark => "Dark",
    }
}

/// Small floating glass plate for controls drawn over content (hover buttons, ⌃ badges).
pub fn paint_chip_plate(p: &mut Painter, rect: RectF, radius: f32) {
    let theme = p.theme().clone();
    let shadow = Shadow::new(1.0, 6.0, Color::rgba(0.0, 0.0, 0.0, if theme.is_dark() { 0.35 } else { 0.12 }));
    p.shadow_outside(rect, radius, &shadow);
    let fill = if theme.is_dark() { Color::rgba8(52, 52, 56, 0.96) } else { Color::rgba8(255, 255, 255, 0.96) };
    p.fill_round_rect(rect, radius, fill);
    p.hairline_round_rect(rect, radius, theme.hairline, true);
}

/// Vertical alpha mask over a scroll viewport: content is hidden for the top `top_hidden` DIP (under a sticky
/// header), fades in until `top_solid`, and fades out over the last `bottom_fade` DIP.
pub fn edge_fade_mask(area: RectF, top_hidden: f32, top_solid: f32, bottom_fade: f32) -> Brush {
    let h = area.h.max(1.0);
    let clear = Color::rgba(0.0, 0.0, 0.0, 0.0);
    let solid = Color::BLACK;
    let at = |y: f32| (y / h).clamp(0.0, 0.5);
    let mut stops = vec![(0.0, if top_solid > 0.0 { clear } else { solid })];
    if top_solid > 0.0 {
        stops.push((at(top_hidden), clear));
        stops.push((at(top_solid), solid));
    }
    if bottom_fade > 0.0 {
        stops.push((1.0 - at(bottom_fade), solid));
        stops.push((1.0, clear));
    } else {
        stops.push((1.0, solid));
    }
    Brush::linear(PointF::new(area.x, area.y), PointF::new(area.x, area.bottom()), &stops)
}

/// The mask for a scroll viewport whose content sits `offset` DIP down, with or without sticky section headers.
pub fn scroll_mask(area: RectF, offset: f32, max_offset: f32, sticky_headers: bool) -> Brush {
    let (hidden, solid) = match (offset > 0.5, sticky_headers) {
        (false, _) => (0.0, 0.0),
        (true, true) => (SECTION_HEADER - 2.0, SECTION_HEADER + 6.0),
        (true, false) => (0.0, 12.0),
    };
    let bottom = if offset < max_offset - 0.5 { BOTTOM_FADE } else { 0.0 };
    edge_fade_mask(area, hidden, solid, bottom)
}

/// A small raised key cap (⌃1 badges), right-aligned at `right` and vertically centered on `center_y`; returns its
/// left edge.
pub fn paint_keycap(p: &mut Painter, text: &str, right: f32, center_y: f32) -> f32 {
    let theme = p.theme().clone();
    let style = TextStyle::caption().weight(Weight::Semibold).tabular().centered();
    let width = (p.measure(text, &style).w + 10.0).ceil().max(22.0);
    let cap = p.snap_rect(RectF::new(right - width, center_y - 9.0, width, 18.0));
    let (fill, edge, base) = if theme.is_dark() {
        (Color::rgba(1.0, 1.0, 1.0, 0.12), Color::rgba(1.0, 1.0, 1.0, 0.10), Color::rgba(0.0, 0.0, 0.0, 0.40))
    } else {
        (Color::WHITE, Color::rgba(0.0, 0.0, 0.0, 0.12), Color::rgba(0.0, 0.0, 0.0, 0.10))
    };
    p.fill_round_rect(cap.offset(0.0, 1.0), 5.0, base);
    p.fill_round_rect(cap, 5.0, fill);
    p.hairline_round_rect(cap, 5.0, edge, true);
    p.text(text, &style, theme.text_secondary, cap);
    cap.x
}
