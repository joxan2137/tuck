//! Tuck's icon artwork: the app icon (warm coral → pink → violet tile with a white clipboard holding a sparkle) and
//! the monochrome tray glyph. Everything is drawn with tuck-ui so the same art serves the .ico, the tray, toasts and
//! the settings header.

use std::rc::Rc;

use anyhow::Result;
use tuck_ui::{
    Brush, Color, Gfx, Image, OffscreenSpec, Painter, Path, PathBuilder, PointF, RectF, Shadow, Theme, render_offscreen,
};

/// Below this many physical pixels the art switches to a pixel-snapped glyph.
const SMALL_PX: f32 = 30.0;
/// From this size on the app icon gets its drop shadow and margin.
const FULL_DETAIL_PX: f32 = 64.0;

const GRADIENT: [(f32, &str); 3] = [(0.0, "#FF8A5B"), (0.5, "#FF4D8D"), (1.0, "#8A5CF6")];

fn gradient(tile: RectF) -> Brush {
    let stops: Vec<(f32, Color)> =
        GRADIENT.iter().map(|(offset, hex)| (*offset, Color::hex(hex).expect("valid gradient color"))).collect();
    Brush::linear(PointF::new(tile.x, tile.y), PointF::new(tile.right(), tile.bottom()), &stops)
}

/// The four-point sparkle with concave sides; horizontal rays are shorter than vertical ones.
fn sparkle_path(gfx: &Gfx, center: PointF, radius: f32) -> Option<Path> {
    let horizontal = radius * 0.82;
    let pinch = radius * 0.14;
    let mut builder = PathBuilder::new();
    builder
        .move_to(PointF::new(center.x, center.y - radius))
        .quad_to(PointF::new(center.x + pinch, center.y - pinch), PointF::new(center.x + horizontal, center.y))
        .quad_to(PointF::new(center.x + pinch, center.y + pinch), PointF::new(center.x, center.y + radius))
        .quad_to(PointF::new(center.x - pinch, center.y + pinch), PointF::new(center.x - horizontal, center.y))
        .quad_to(PointF::new(center.x - pinch, center.y - pinch), PointF::new(center.x, center.y - radius))
        .close();
    builder.build(gfx).ok()
}

/// Where the clipboard's parts sit inside the square `frame`: the board (stroke centerline), the clip across its
/// top edge and the sparkle tucked inside.
struct Glyph {
    board: RectF,
    clip: RectF,
    sparkle: PointF,
    sparkle_radius: f32,
}

fn glyph_layout(frame: RectF) -> Glyph {
    let board_w = frame.w * 0.76;
    let board_h = frame.h * 0.86;
    let board = RectF::new(frame.center().x - board_w / 2.0, frame.bottom() - board_h, board_w, board_h);
    let clip_w = frame.w * 0.4;
    let clip_h = frame.h * 0.17;
    let clip = RectF::new(frame.center().x - clip_w / 2.0, board.y - clip_h * 0.55, clip_w, clip_h);
    let sparkle = PointF::new(board.center().x, board.y + board.h * 0.56);
    Glyph { board, clip, sparkle, sparkle_radius: board.w * 0.3 }
}

/// Pixel-snapped clipboard for glyphs smaller than `SMALL_PX`: outline from filled pixel rows, a solid clip that
/// straddles the top edge (reaching into the board, so it reads as a clip and not a battery cap) and a compact
/// sparkle. `frame` is the snapped glyph box.
fn paint_small_glyph(p: &mut Painter, frame: RectF, thickness_px: f32, color: Color) {
    let px = p.px();
    let frame_px = (frame.w / px).round();
    let thickness = thickness_px * px;
    let side_inset = (frame_px * 0.1).round() * px;
    let top = frame.y + (frame_px * 0.12).round().max(1.0) * px;
    let board = RectF::new(frame.x + side_inset, top, frame.w - 2.0 * side_inset, frame.bottom() - top);
    let board_px = (board.w / px).round();
    let mut clip_px = (board_px * 0.45).round();
    if (board_px - clip_px) as i32 % 2 != 0 {
        clip_px += 1.0;
    }
    let clip_bottom = board.y + thickness + px;
    let clip = RectF::new(board.x + (board_px - clip_px) / 2.0 * px, frame.y, clip_px * px, clip_bottom - frame.y);
    for rect in [
        RectF::new(board.x, board.y, board.w, thickness),
        RectF::new(board.x, board.bottom() - thickness, board.w, thickness),
        RectF::new(board.x, board.y, thickness, board.h),
        RectF::new(board.right() - thickness, board.y, thickness, board.h),
    ] {
        p.fill_rect(rect, color);
    }
    p.fill_rect(clip, color);
    let inside = RectF::new(
        board.x + thickness,
        clip.bottom(),
        board.w - 2.0 * thickness,
        board.bottom() - thickness - clip.bottom(),
    );
    let center =
        PointF::new(((inside.center().x / px).floor() + 0.5) * px, ((inside.center().y / px).floor() + 0.5) * px);
    let radius = ((inside.w.min(inside.h) / px * 0.42).round().max(2.0) + 0.5) * px;
    if let Some(sparkle) = sparkle_path(p.gfx(), center, radius) {
        p.fill_path(&sparkle, color);
    }
}

/// The app icon filling the square `rect` (DIP or px); detail follows the physical size.
pub fn paint_app_icon(p: &mut Painter, rect: RectF) {
    let size_px = (rect.w * p.scale()).round();
    if size_px < SMALL_PX {
        paint_small_app_icon(p, rect, size_px);
    } else {
        paint_detailed_app_icon(p, rect, size_px);
    }
}

fn paint_detailed_app_icon(p: &mut Painter, rect: RectF, size_px: f32) {
    let size = rect.w;
    let full = size_px >= FULL_DETAIL_PX;
    let tile = if full { rect.inset(size * 0.06) } else { p.snap_rect(rect.inset(size * 0.03)) };
    let radius = tile.w * 0.225;
    if full {
        p.shadow(tile, radius, &Shadow::new(size * 0.012, size * 0.05, Color::rgba(0.3, 0.04, 0.16, 0.38)));
    }
    p.fill_round_rect(tile, radius, gradient(tile));
    let sheen = Brush::linear(
        PointF::new(tile.x, tile.y),
        PointF::new(tile.x, tile.y + tile.h * 0.6),
        &[(0.0, Color::rgba(1.0, 1.0, 1.0, 0.24)), (1.0, Color::rgba(1.0, 1.0, 1.0, 0.0))],
    );
    p.fill_round_rect(tile, radius, sheen);
    let rim = (size / 256.0).max(p.px());
    p.stroke_round_rect(tile.inset(rim / 2.0), radius - rim / 2.0, Color::rgba(1.0, 1.0, 1.0, 0.18), rim);

    let frame_size = tile.w * 0.56;
    let frame =
        RectF::new(tile.center().x - frame_size / 2.0, tile.center().y - frame_size / 2.0, frame_size, frame_size);
    let glyph = glyph_layout(frame);
    let stroke = tile.w * if full { 0.06 } else { 0.074 };
    let corner = glyph.board.w * 0.17;
    let clip_radius = glyph.clip.h * 0.4;
    if full {
        let depth = Color::rgba(0.32, 0.03, 0.18, 0.22);
        p.translate(0.0, size * 0.008, |p| {
            p.stroke_round_rect(glyph.board, corner, depth, stroke);
            p.fill_round_rect(glyph.clip, clip_radius, depth);
        });
    }
    p.stroke_round_rect(glyph.board, corner, Color::WHITE, stroke);
    p.fill_round_rect(glyph.clip, clip_radius, Color::WHITE);
    if full {
        let glow = glyph.sparkle_radius * 0.55;
        let glow_rect = RectF::new(glyph.sparkle.x - glow, glyph.sparkle.y - glow, glow * 2.0, glow * 2.0);
        p.shadow(glow_rect, glow, &Shadow::new(0.0, glyph.sparkle_radius * 1.2, Color::rgba(1.0, 1.0, 1.0, 0.5)));
    }
    if let Some(sparkle) = sparkle_path(p.gfx(), glyph.sparkle, glyph.sparkle_radius) {
        p.fill_path(&sparkle, Color::WHITE);
    }
}

fn paint_small_app_icon(p: &mut Painter, rect: RectF, size_px: f32) {
    let px = p.px();
    let tile = p.snap_rect(rect);
    p.fill_round_rect(tile, (size_px * 0.22).round() * px, gradient(tile));
    let inset = (size_px * 0.19).round() * px;
    let frame = p.snap_rect(tile.inset(inset));
    paint_small_glyph(p, frame, (size_px / 14.0).round().clamp(1.0, 2.0), Color::WHITE);
}

/// Monochrome clipboard + sparkle for the notification area, filling `rect` with a one-pixel margin.
pub fn paint_tray_glyph(p: &mut Painter, rect: RectF, color: Color) {
    let px = p.px();
    let size_px = (rect.w / px).round();
    let thickness_px = (size_px / 14.0).round().max(1.0);
    let frame = p.snap_rect(rect.inset((size_px / 16.0).round().max(1.0) * px));
    if size_px < SMALL_PX {
        paint_small_glyph(p, frame, thickness_px, color);
        return;
    }
    let glyph = glyph_layout(frame);
    let stroke = thickness_px * px;
    p.stroke_round_rect(glyph.board, glyph.board.w * 0.16, color, stroke);
    p.fill_round_rect(glyph.clip, glyph.clip.h * 0.4, color);
    if let Some(sparkle) = sparkle_path(p.gfx(), p.snap_point(glyph.sparkle), glyph.sparkle_radius) {
        p.fill_path(&sparkle, color);
    }
}

/// The app icon at exactly `size_px` × `size_px`.
pub fn render_app_icon(gfx: &Rc<Gfx>, size_px: u32) -> Result<Image> {
    let spec = OffscreenSpec::pixels(size_px, size_px, 1.0, Theme::dark());
    let side = size_px as f32;
    render_offscreen(gfx, &spec, |_, p| paint_app_icon(p, RectF::new(0.0, 0.0, side, side)))
}

/// The tray glyph at exactly `size_px` × `size_px` in `color`.
pub fn render_tray_glyph(gfx: &Rc<Gfx>, size_px: u32, color: Color) -> Result<Image> {
    let spec = OffscreenSpec::pixels(size_px, size_px, 1.0, Theme::dark());
    let side = size_px as f32;
    render_offscreen(gfx, &spec, |_, p| paint_tray_glyph(p, RectF::new(0.0, 0.0, side, side), color))
}
